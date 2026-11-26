//! A glTF scene's mesh nodes read into meshes, rigid nodes' transforms baked
//! into their vertices and skinned ones' kept in bind space, and batched by
//! what primitives must share to be drawn as one mesh. An instanced node's
//! mesh (`instancing`) is read once, in its own space, and its node's
//! instances become placements.
use glam::{Mat3, Mat4, Vec3};
use gltf::Semantic;
use gltf::accessor::DataType::{self, F32, I8, I16, U8, U16, U32};
use gltf::accessor::Dimensions::{self, Scalar, Vec2, Vec4};
use std::collections::HashMap;

use super::super::asset::{CpuMesh, Material, Result, UvSet, Vertex, tangent_off_normal};
use super::super::deformation::{MeshDeformation, MorphTarget};
use super::super::tangents;
use super::TangentFrames;
use super::attributes::{DIRECTION_TYPES, attribute, check_direction};
use super::check_accessor;
use super::instancing::{InstancedNode, NodeUse, Plan, transforms};
use super::rig::{Rigging, read_influences, read_morph_targets};
use super::topology::{check_mode, triangle_list};

/// Whether SGL3D leaves out a vertex attribute (`Ignored::Attribute`):
/// nothing it draws reads a third UV set, a second colour set or an
/// application-specific attribute.
pub(super) fn ignored_attribute(semantic: &Semantic) -> bool {
    matches!(
        semantic,
        Semantic::TexCoords(2..) | Semantic::Colors(1..) | Semantic::Extras(_)
    )
}

/// What primitives must share to be drawn as one mesh: a material, and
/// whether a skin and which node's morph weights deform them.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct BatchKey {
    material: usize,
    skinned: bool,
    morphed: Option<usize>,
}

/// `meshes` merged by `BatchKey` to reduce draw calls: rigid ones by
/// material in material order, then deforming ones in first-use order.
pub(super) fn batch(meshes: Vec<(BatchKey, CpuMesh)>, materials: usize) -> Vec<CpuMesh> {
    let mut keys: Vec<BatchKey> = (0..materials)
        .map(|material| BatchKey {
            material,
            skinned: false,
            morphed: None,
        })
        .collect();
    let mut index: HashMap<BatchKey, usize> =
        keys.iter().enumerate().map(|(i, k)| (*k, i)).collect();
    let mut batches: Vec<CpuMesh> = keys
        .iter()
        .map(|key| CpuMesh {
            vertices: Vec::new(),
            indices: Vec::new(),
            material: key.material,
            deformation: MeshDeformation::default(),
        })
        .collect();
    for (key, mesh) in meshes {
        let at = *index.entry(key).or_insert_with(|| {
            keys.push(key);
            batches.push(CpuMesh {
                vertices: Vec::new(),
                indices: Vec::new(),
                material: key.material,
                deformation: MeshDeformation {
                    influences: Vec::new(),
                    morph_targets: mesh
                        .deformation
                        .morph_targets
                        .iter()
                        .map(|target| MorphTarget {
                            weight: target.weight,
                            deltas: Vec::new(),
                        })
                        .collect(),
                },
            });
            batches.len() - 1
        });
        let batch = &mut batches[at];
        let offset = batch.vertices.len() as u32;
        batch
            .indices
            .extend(mesh.indices.into_iter().map(|index| index + offset));
        batch.vertices.extend(mesh.vertices);
        batch
            .deformation
            .influences
            .extend(mesh.deformation.influences);
        for (merged, target) in batch
            .deformation
            .morph_targets
            .iter_mut()
            .zip(mesh.deformation.morph_targets)
        {
            merged.deltas.extend(target.deltas);
        }
    }
    batches
        .into_iter()
        .filter(|mesh| !mesh.indices.is_empty())
        .collect()
}

/// What the node walk accumulates: the ordinary nodes' meshes, baked, and
/// the instanced nodes' meshes, each read once, with their placements.
#[derive(Default)]
pub(super) struct Walk {
    pub(super) meshes: Vec<(BatchKey, CpuMesh)>,
    pub(super) instanced: Vec<InstancedRead>,
    /// Each instanced glTF mesh's entry in `instanced`.
    by_mesh: HashMap<usize, usize>,
    /// The tangent frames generated for each glTF mesh's primitive (by
    /// their indices), which every node that uses it takes.
    generated: Generated,
}

/// Tangent frames generated for glTF primitives, by mesh and primitive
/// index: in the mesh's own space and winding, whichever node uses it;
/// none where too many triangles share a vertex, or without the
/// `mikktspace` feature.
type Generated = HashMap<(usize, usize), Option<tangents::Framed>>;

impl Walk {
    /// The primitives (mesh and primitive index) whose frames were not
    /// generated, because too many triangles share one of their vertices
    /// or the build has no generator (the `mikktspace` feature), in order.
    pub(super) fn unframed(&self) -> Vec<(usize, usize)> {
        let mut unframed: Vec<_> = self
            .generated
            .iter()
            .filter(|(_, framed)| framed.is_none())
            .map(|(&key, _)| key)
            .collect();
        unframed.sort_unstable();
        unframed
    }
}

/// Which mesh nodes load, asked with each one's name (`LoadOptions::nodes`).
type NodeFilter<'a> = &'a dyn Fn(Option<&str>) -> bool;

/// One glTF mesh instanced nodes use: its primitives, read once in its own
/// space, and each such node with its instances' transforms.
pub(super) struct InstancedRead {
    pub(super) mesh: usize,
    pub(super) primitives: Vec<(BatchKey, CpuMesh)>,
    pub(super) nodes: Vec<InstancedNode>,
}

pub(super) fn read_node(
    node: gltf::Node<'_>,
    parent: Mat4,
    (document, plan, include_node): (&gltf::Document, &Plan, NodeFilter<'_>),
    (buffers, buffer_bytes): (&[gltf::buffer::Data], usize),
    (default_material, materials, frames): (usize, &[Material], TangentFrames),
    rigging: &mut Rigging,
    walk: &mut Walk,
) -> Result<()> {
    let mut pending = vec![(node, parent)];
    while let Some((node, parent)) = pending.pop() {
        let global = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
        if let Some(NodeUse::Instanced { attributes, count }) = plan.node(node.index()) {
            if let Some(mesh) = node.mesh()
                && include_node(node.name())
            {
                let transforms = transforms(
                    &node,
                    global,
                    (attributes, *count),
                    document,
                    (buffers, buffer_bytes),
                )?;
                let entry = match walk.by_mesh.get(&mesh.index()) {
                    Some(&entry) => entry,
                    None => {
                        // The mesh's own space: no node transform, no skin
                        // and (the plan has refused them) no morph targets.
                        let mut primitives = Vec::new();
                        read_primitives(
                            &node,
                            &mesh,
                            (Mat4::IDENTITY, None),
                            (buffers, buffer_bytes),
                            (default_material, materials, frames),
                            rigging,
                            (&mut primitives, &mut walk.generated),
                        )?;
                        walk.instanced.push(InstancedRead {
                            mesh: mesh.index(),
                            primitives,
                            nodes: Vec::new(),
                        });
                        walk.by_mesh.insert(mesh.index(), walk.instanced.len() - 1);
                        walk.instanced.len() - 1
                    }
                };
                walk.instanced[entry].nodes.push(InstancedNode {
                    node: node.index(),
                    name: node.name().map(str::to_owned),
                    transforms,
                });
            }
        } else {
            read_mesh_node(
                &node,
                global,
                (buffers, buffer_bytes),
                (default_material, materials, frames),
                rigging,
                (&mut walk.meshes, &mut walk.generated),
                include_node,
            )?;
        }
        // Reverse just the appended children: the stack then visits them
        // depth-first in declared order, preserving batching and selection order.
        let first_child = pending.len();
        pending.extend(node.children().map(|child| (child, global)));
        pending[first_child..].reverse();
    }
    Ok(())
}

/// Whether `transform` cannot place a mesh: not finite, or singular
/// relative to its axes' own lengths, so a uniformly tiny scale
/// (determinant s³) passes and only a collapsed axis is refused.
pub(super) fn singular(transform: Mat4) -> bool {
    let axes = Mat3::from_mat4(transform);
    let volume = axes.x_axis.length() * axes.y_axis.length() * axes.z_axis.length();
    // Normals use only the 3x3, so an inverted translation cannot overflow it.
    let normal_transform = axes.inverse().transpose();
    !transform.is_finite()
        || axes.determinant().abs() <= 1e-6 * volume
        || !normal_transform.is_finite()
}

fn read_mesh_node(
    node: &gltf::Node<'_>,
    global: Mat4,
    (buffers, buffer_bytes): (&[gltf::buffer::Data], usize),
    (default_material, materials, frames): (usize, &[Material], TangentFrames),
    rigging: &mut Rigging,
    (meshes, generated): (&mut Vec<(BatchKey, CpuMesh)>, &mut Generated),
    include_node: &dyn Fn(Option<&str>) -> bool,
) -> Result<()> {
    if let Some(mesh) = node.mesh()
        && include_node(node.name())
    {
        // A skinned mesh's vertices stay in bind space: glTF ignores the
        // transform of the node that holds it, and its joints place it.
        let skin = node.skin().map(|skin| rigging.skin(&skin));
        let transform = if skin.is_some() {
            Mat4::IDENTITY
        } else {
            global
        };
        if singular(transform) {
            return Err(format!(
                "node {} has a singular/nonfinite transform; apply a nonzero scale in Blender",
                node.index()
            )
            .into());
        }
        read_primitives(
            node,
            &mesh,
            (transform, skin),
            (buffers, buffer_bytes),
            (default_material, materials, frames),
            rigging,
            (meshes, generated),
        )?;
    }
    Ok(())
}

/// `mesh`'s primitives, held by `node`, with `transform` baked into their
/// vertices (mirrored winding corrected) and `skin`'s influences.
fn read_primitives(
    node: &gltf::Node<'_>,
    mesh: &gltf::Mesh<'_>,
    (transform, skin): (Mat4, Option<(u32, u32)>),
    (buffers, buffer_bytes): (&[gltf::buffer::Data], usize),
    (default_material, materials, frames): (usize, &[Material], TangentFrames),
    rigging: &mut Rigging,
    (meshes, generated): (&mut Vec<(BatchKey, CpuMesh)>, &mut Generated),
) -> Result<()> {
    {
        let normal_transform = Mat4::from_mat3(Mat3::from_mat4(transform).inverse().transpose());
        // Every primitive with morph targets has the mesh's, which the
        // node's one set of weights drives; one without is unmorphed.
        let mesh_targets = rigging.mesh_targets(mesh);
        for primitive in mesh.primitives() {
            let label = format!("mesh {} primitive {}", mesh.index(), primitive.index());
            let target_count = primitive.morph_targets().count();
            if target_count != 0 && target_count != mesh_targets {
                return Err(format!("{label}: {target_count} morph targets where the mesh's other morphed primitives have {mesh_targets}; give every morphed primitive of a mesh the same shape keys").into());
            }
            check_mode(primitive.mode(), &label)?;
            // glTF 2.0 3.7.2.1's accessor types for each attribute, its
            // indices and its morph targets' displacements, with
            // KHR_mesh_quantization's for positions, directions, UVs and
            // displacements. Every attribute and displacement holds one
            // element a vertex, as many as POSITION (glTF 2.0 3.7.2.1 and
            // 3.7.2.2).
            let vertices = primitive.get(&Semantic::Positions).map(|p| p.count());
            for (semantic, accessor) in primitive.attributes() {
                // Neither checked nor read.
                if ignored_attribute(&semantic) {
                    continue;
                }
                let (types, shapes): (&[DataType], &[Dimensions]) = match semantic {
                    Semantic::Joints(1..) | Semantic::Weights(1..) => {
                        return Err(format!("{label}: more than four joint influences per vertex; limit influences to four in the export").into());
                    }
                    Semantic::Positions => (&[F32, I8, U8, I16, U16], &[Dimensions::Vec3]),
                    Semantic::Normals => (DIRECTION_TYPES, &[Dimensions::Vec3]),
                    Semantic::Tangents => (DIRECTION_TYPES, &[Vec4]),
                    // TEXCOORD_1 is the lightmap UV (`Vertex::lightmap_uv`)
                    // and, where a map of the material samples it, the
                    // second UV set (`Vertex::uv1`).
                    Semantic::TexCoords(0 | 1) => (&[F32, I8, U8, I16, U16], &[Vec2]),
                    Semantic::Colors(0) => (&[F32, U8, U16], &[Dimensions::Vec3, Vec4]),
                    Semantic::Joints(0) => (&[U8, U16], &[Vec4]),
                    Semantic::Weights(0) => (&[F32, U8, U16], &[Vec4]),
                    _ => {
                        return Err(format!("{label}: unsupported attribute {semantic:?}; add its renderer input or remove it from the export").into());
                    }
                };
                let what = format!("{label}: {}", semantic.to_string());
                check_accessor(
                    &accessor,
                    &what,
                    types,
                    shapes,
                    vertices.filter(|_| semantic != Semantic::Positions),
                    (buffers, buffer_bytes),
                )?;
                if matches!(semantic, Semantic::Normals | Semantic::Tangents) {
                    check_direction(&accessor, &what)?;
                }
            }
            if let Some(indices) = primitive.indices() {
                check_accessor(
                    &indices,
                    &format!("{label}: indices"),
                    &[U8, U16, U32],
                    &[Scalar],
                    None,
                    (buffers, buffer_bytes),
                )?;
            }
            for (target, displacements) in primitive.morph_targets().enumerate() {
                let what = format!("{label}: morph target {target}");
                for (accessor, direction) in [
                    (displacements.positions(), false),
                    (displacements.normals(), true),
                    (displacements.tangents(), true),
                ] {
                    let Some(accessor) = accessor else { continue };
                    let types: &[DataType] = if direction {
                        DIRECTION_TYPES
                    } else {
                        &[F32, I8, I16]
                    };
                    check_accessor(
                        &accessor,
                        &what,
                        types,
                        &[Dimensions::Vec3],
                        vertices,
                        (buffers, buffer_bytes),
                    )?;
                    if direction {
                        check_direction(&accessor, &what)?;
                    }
                }
            }
            let positions: Vec<[f32; 3]> = attribute(&primitive, Semantic::Positions, buffers)
                .ok_or_else(|| format!("{label}: missing POSITION"))?;
            let normals: Vec<[f32; 3]> = attribute(&primitive, Semantic::Normals, buffers)
                .ok_or_else(|| format!("{label}: missing NORMAL; export vertex normals"))?;
            let material_index = primitive.material().index().unwrap_or(default_material);
            let material = &materials[material_index];
            let tangents: Option<Vec<[f32; 4]>> =
                attribute(&primitive, Semantic::Tangents, buffers);
            let uvs: Option<Vec<[f32; 2]>> = attribute(&primitive, Semantic::TexCoords(0), buffers);
            let lightmap_uvs: Option<Vec<[f32; 2]>> =
                attribute(&primitive, Semantic::TexCoords(1), buffers);
            // A primitive without TANGENT whose material takes a vertex
            // frame gets MikkTSpace's on the frame's set: the normal map's,
            // the first without one (glTF 2.0 3.7.2.1).
            let frame_set = material.frame_set();
            let frame_uvs = match frame_set {
                UvSet::Uv0 => uvs.as_deref(),
                UvSet::Uv1 => lightmap_uvs.as_deref(),
            };
            let generate = tangents.is_none()
                && frames == TangentFrames::Generate
                && material.takes_vertex_frame()
                && frame_uvs.is_some();
            if material.anisotropy_strength > 0.0 && tangents.is_none() && !generate {
                return Err(format!("{label}: anisotropic material {} requires TANGENT; export tangents, or load with LoadOptions::tangents Generate and texture coordinates on its frame's set", material.name).into());
            }
            // Each UV set a map SGL3D samples names must be the
            // primitive's (glTF 2.0 3.9.2); an occlusion map it leaves out
            // asks for none.
            let samples = |set: UvSet| {
                material
                    .sampled_textures()
                    .any(|texture| texture.uv_set == set)
            };
            if samples(UvSet::Uv0) && uvs.is_none() {
                return Err(format!("{label}: textured primitive is missing TEXCOORD_0").into());
            }
            // The second set is kept only where a map samples it.
            let second = samples(UvSet::Uv1);
            if second && lightmap_uvs.is_none() {
                return Err(format!(
                    "{label}: material {} samples TEXCOORD_1, which the primitive lacks",
                    material.name
                )
                .into());
            }
            if normals.len() != positions.len()
                || tangents
                    .as_ref()
                    .is_some_and(|t| t.len() != positions.len())
                || uvs.as_ref().is_some_and(|uv| uv.len() != positions.len())
                || lightmap_uvs
                    .as_ref()
                    .is_some_and(|uv| uv.len() != positions.len())
            {
                return Err(format!("{label}: attribute counts differ").into());
            }
            let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()].0));
            let colors = reader
                .read_colors(0)
                .map(|values| values.into_rgba_f32().collect::<Vec<_>>());
            if colors
                .as_ref()
                .is_some_and(|colors| colors.len() != positions.len())
            {
                return Err(format!("{label}: color count differs from positions").into());
            }
            let vertices = positions
                .iter()
                .enumerate()
                .map(|(i, position)| {
                    let position = transform.transform_point3(Vec3::from_array(*position));
                    let normal = normal_transform
                        .transform_vector3(Vec3::from_array(normals[i]))
                        .normalize_or_zero();
                    if !position.is_finite() || !normal.is_finite() || normal == Vec3::ZERO {
                        return Err(format!(
                            "{label}: vertex {i} has an invalid position or normal"
                        )
                        .into());
                    }
                    let tangent = tangents.as_ref().map(|tangents| -> Result<[f32; 4]> {
                        let authored = tangents[i];
                        let direction = Vec3::new(authored[0], authored[1], authored[2]);
                        let invalid = || format!("{label}: vertex {i} has invalid TANGENT; export finite nonzero tangents perpendicular to NORMAL with handedness +1 or -1").into();
                        if !direction.is_finite() || direction == Vec3::ZERO
                            || !matches!(authored[3], -1.0 | 1.0)
                        {
                            return Err(invalid());
                        }
                        // Valid glTF, but along the normal, as authored
                        // or as the transform leaves it, it spans no
                        // frame: absent, unless anisotropy needs it.
                        let tangent = tangent_off_normal(Vec3::from_array(normals[i]), direction)
                            .and(tangent_off_normal(normal, transform.transform_vector3(direction)));
                        let Some(tangent) = tangent else {
                            return if material.anisotropy_strength > 0.0 {
                                Err(invalid())
                            } else {
                                Ok([0.0; 4])
                            };
                        };
                        // Winding correction changes indices only: this determinant sign
                        // converts the frame once, including mirrored UV handedness.
                        Ok([tangent.x, tangent.y, tangent.z, authored[3] * transform.determinant().signum()])
                    }).transpose()?.unwrap_or([0.0; 4]);
                    Ok(Vertex {
                        tangent,
                        lightmap_bounds: [0., 0., 1., 1.],
                        lightmap_uv: lightmap_uvs.as_ref().map_or([0.; 2], |uv| uv[i]),
                        position: position.to_array(),
                        normal: normal.to_array(),
                        uv: uvs.as_ref().map_or([0.0; 2], |uv| uv[i]),
                        uv1: match &lightmap_uvs {
                            Some(uv) if second => uv[i],
                            _ => [0.0; 2],
                        },
                        color: colors.as_ref().map_or([1.0; 4], |colors| colors[i]),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let indices = reader
                .read_indices()
                .map(|indices| indices.into_u32().collect::<Vec<_>>())
                .unwrap_or_else(|| (0..vertices.len() as u32).collect());
            if indices
                .iter()
                .any(|index| *index as usize >= vertices.len())
            {
                return Err(format!("{label}: invalid triangle indices").into());
            }
            let mut indices = triangle_list(primitive.mode(), indices, &label)?;
            let mut influences = match skin {
                Some(skin) => read_influences(&reader, skin, vertices.len(), &label)?,
                None => Vec::new(),
            };
            let mut morph_targets = if target_count == 0 {
                Vec::new()
            } else {
                let first = rigging.morph_weights(node, target_count)?;
                read_morph_targets(
                    (&primitive, buffers),
                    first,
                    (transform, normal_transform),
                    (&normals, tangents.as_deref()),
                    &vertices,
                    &label,
                )?
            };
            let mut vertices = vertices;
            // None where too many triangles share a vertex
            // (`tangents::MOST_TRIANGLES_AT_A_VERTEX`) or without the
            // `mikktspace` feature: the primitive keeps the frame of each
            // triangle's coordinates, and the load lists it
            // (`Ignored::TangentFrames`).
            let framed = frame_uvs.filter(|_| generate).and_then(|frame_uvs| {
                generated
                    .entry((mesh.index(), primitive.index()))
                    .or_insert_with(|| {
                        tangents::generate(&positions, &normals, frame_uvs, &indices)
                    })
                    .as_ref()
            });
            if generate && framed.is_none() && material.anisotropy_strength > 0.0 {
                return Err(if cfg!(feature = "mikktspace") {
                    format!("{label}: anisotropic material {}: more than {} triangles share one of its vertices, too many to generate tangent frames for; export TANGENT", material.name, tangents::MOST_TRIANGLES_AT_A_VERTEX)
                } else {
                    format!("{label}: anisotropic material {} requires TANGENT, which sgl-3d generates only with its mikktspace feature; export tangents, or enable the feature", material.name)
                }
                .into());
            }
            if let Some(framed) = framed {
                // In the mesh's own space and winding, then carried into
                // the vertices' space as an authored tangent is. A vertex
                // MikkTSpace leaves without a frame (one of degenerate
                // coordinates alone, where glTF's tangent space is
                // undefined) takes one in its normal's plane, as the
                // packed vertex takes an absent tangent's, so its mesh
                // keeps its vertex frames; Bevy takes MikkTSpace's
                // default frame there (bevy_mesh `generate_tangents_for_mesh`).
                let handedness = transform.determinant().signum();
                vertices = framed
                    .source
                    .iter()
                    .zip(&framed.frames)
                    .map(|(&source, frame)| {
                        let source = source as usize;
                        let mut vertex = vertices[source];
                        let normal = Vec3::from_array(vertex.normal);
                        vertex.tangent = frame
                            .and_then(|[x, y, z, w]| {
                                let direction = Vec3::new(x, y, z);
                                tangent_off_normal(Vec3::from_array(normals[source]), direction)
                                    .and(tangent_off_normal(
                                        normal,
                                        transform.transform_vector3(direction),
                                    ))
                                    .map(|tangent| {
                                        [tangent.x, tangent.y, tangent.z, w * handedness]
                                    })
                            })
                            .unwrap_or_else(|| {
                                normal.any_orthonormal_vector().extend(1.).to_array()
                            });
                        vertex
                    })
                    .collect();
                if !influences.is_empty() {
                    influences = framed
                        .source
                        .iter()
                        .map(|&source| influences[source as usize])
                        .collect();
                }
                for target in &mut morph_targets {
                    target.deltas = framed
                        .source
                        .iter()
                        .map(|&source| target.deltas[source as usize])
                        .collect();
                }
                indices = framed.triangles.clone();
            }
            if transform.determinant() < 0.0 {
                for triangle in indices.as_chunks_mut::<3>().0 {
                    triangle.swap(1, 2);
                }
            }
            let key = BatchKey {
                material: material_index,
                skinned: skin.is_some(),
                morphed: (target_count > 0).then_some(node.index()),
            };
            meshes.push((
                key,
                CpuMesh {
                    vertices,
                    indices,
                    material: material_index,
                    deformation: MeshDeformation {
                        influences,
                        morph_targets,
                    },
                },
            ));
        }
    }
    Ok(())
}
