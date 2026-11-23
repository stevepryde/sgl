//! A glTF's `EXT_mesh_gpu_instancing` nodes read as shared geometry and
//! placements ([`InstancedMesh`]): each glTF mesh an instanced node of the
//! loaded scene uses, read once in its own space, and each such node's
//! instance transforms, composed once here. The meshes' vertices are read
//! by the node walk (`meshes`); this module decides which nodes are
//! instanced ([`Plan`]) and reads their transforms ([`transforms`]).
//!
//! The extension (Khronos glTF 54a3abd,
//! `extensions/2.0/Vendor/EXT_mesh_gpu_instancing/README.md`) is the
//! authority: `TRANSLATION` a FLOAT VEC3, `ROTATION` a FLOAT, normalized BYTE
//! or normalized SHORT VEC4, `SCALE` a FLOAT VEC3, every attribute of a node
//! one `count`, and `WorldPosition = NodeWorldXform * InstanceXform *
//! VertexPosition` with `InstanceXform = Translation * Rotation * Scale`.
//! three.js r187's `GLTFLoader` (`GLTFMeshGpuInstancing`, MIT) draws an
//! instanced node as one `InstancedMesh` sharing its geometry, composing
//! each instance with `Matrix4::compose`. Bevy's `bevy_gltf` (ea06813,
//! MIT/Apache-2.0, `loader/extensions/ext_mesh_gpu_instancing.rs`) places
//! each instance at `world_transform * instance`, normalises a dequantised
//! rotation, counts custom attributes toward the shared count, and spawns a
//! skinned instanced node once.
use glam::{Mat4, Quat, Vec3, Vec4};
use gltf::accessor::DataType::{F32, I8, I16};
use gltf::accessor::Dimensions;
use gltf::accessor::Item;

use super::super::asset::{CpuMesh, Result};
use super::Ignored;
use super::attributes::read;
use super::check_accessor;
use super::meshes::singular;

/// The extension's name, as `extensionsUsed` lists it.
pub(super) const EXTENSION: &str = "EXT_mesh_gpu_instancing";

/// One glTF mesh that instanced nodes of the loaded scene use, read once:
/// its geometry and every placement of it.
#[derive(Clone)]
pub struct InstancedMesh {
    /// The mesh's index in the glTF.
    pub mesh: usize,
    /// Its primitives in the mesh's own space (no node transform applied),
    /// batched by material as [`Asset::meshes`](super::super::asset::Asset::meshes)
    /// are, their materials indexing `Asset::materials`.
    pub meshes: Vec<CpuMesh>,
    /// Each instanced node of the loaded scene that uses the mesh,
    /// depth-first with the scene's roots and each node's children in
    /// declared order.
    pub nodes: Vec<InstancedNode>,
}

/// A node of the loaded scene that instances a mesh
/// (`EXT_mesh_gpu_instancing`).
#[derive(Clone, Debug, PartialEq)]
pub struct InstancedNode {
    /// The node's index in the glTF.
    pub node: usize,
    /// The node's authored name, by which a game can pick among them.
    pub name: Option<String>,
    /// Each instance's transform from the mesh's space to the asset's, in
    /// instance order: the node's world transform (every ancestor applied,
    /// at the scene's authored pose; animation does not move it) times the
    /// instance's translation, rotation and scale. Finite and invertible; a
    /// negative determinant mirrors the instance.
    pub transforms: Vec<Mat4>,
}

/// How a node of the loaded scene takes the extension.
#[derive(Clone)]
pub(super) enum NodeUse {
    /// Its mesh is instanced: these attribute accessors, every one with
    /// `count` elements.
    Instanced {
        attributes: Vec<(String, usize)>,
        count: usize,
    },
    /// It loads as an ordinary mesh node, the extension left out
    /// (`Ignored::Instancing`).
    Ordinary,
}

/// Which nodes of the loaded scene are instanced, decided from the document
/// alone before anything is charged or read.
pub(super) struct Plan {
    nodes: Vec<Option<NodeUse>>,
}

impl Plan {
    /// No node instanced, for a document that does not use the extension.
    fn none(nodes: usize) -> Self {
        Self {
            nodes: vec![None; nodes],
        }
    }

    /// The extension on each node of `scene` that has a mesh. A node
    /// without a mesh has no defined behaviour under it and is left alone,
    /// as three.js and Bevy leave it. A skinned node (whose skin ignores the
    /// node transform instances compose with) or one with morph targets
    /// (SGL3D does not instance a deforming mesh) fails the load when the
    /// file requires the extension (`required`), and otherwise loads as an
    /// ordinary node, listed in `ignored`, as a loader without the
    /// extension draws it (glTF 2.0 5.17.1). Each attribute other than
    /// TRANSLATION, ROTATION and SCALE is listed in `ignored` too. Every
    /// instanced node of the scene is planned, selected by
    /// `LoadOptions::nodes` or not, as every node is charged; only a selected
    /// one's transforms are read. `check_roots` has made the scene a set of
    /// trees, so the walk visits each node once.
    pub(super) fn read(
        document: &gltf::Document,
        scene: &gltf::Scene<'_>,
        required: bool,
        ignored: &mut Vec<Ignored>,
    ) -> Result<Self> {
        let mut plan = Self::none(document.nodes().len());
        if !document.extensions_used().any(|name| name == EXTENSION) {
            return Ok(plan);
        }
        let accessors = document.accessors().len();
        let mut pending: Vec<_> = scene.nodes().collect();
        pending.reverse();
        while let Some(node) = pending.pop() {
            let first_child = pending.len();
            pending.extend(node.children());
            pending[first_child..].reverse();
            let (Some(mesh), Some(value)) = (node.mesh(), node.extension_value(EXTENSION)) else {
                continue;
            };
            let index = node.index();
            let invalid = |what: &str| -> Box<dyn std::error::Error> {
                format!("node {index}'s {EXTENSION}: {what}; re-export valid glTF").into()
            };
            let deforming = node.skin().is_some()
                || mesh
                    .primitives()
                    .any(|primitive| primitive.morph_targets().next().is_some());
            if deforming {
                if required {
                    return Err(format!("node {index} instances a skinned or morphed mesh ({EXTENSION}), which SGL3D does not instance; export the deforming mesh without GPU instancing").into());
                }
                ignored.push(Ignored::Instancing { node: index });
                plan.nodes[index] = Some(NodeUse::Ordinary);
                continue;
            }
            let attributes = value
                .get("attributes")
                .and_then(|attributes| attributes.as_object())
                .ok_or_else(|| invalid("its attributes are not an object"))?;
            let attributes = attributes
                .iter()
                .map(|(name, accessor)| {
                    accessor
                        .as_u64()
                        .and_then(|accessor| usize::try_from(accessor).ok())
                        .filter(|&accessor| accessor < accessors)
                        .map(|accessor| (name.clone(), accessor))
                        .ok_or_else(|| invalid(&format!("attribute {name} names no accessor")))
                })
                .collect::<Result<Vec<_>>>()?;
            let mut count = None;
            for (name, accessor) in &attributes {
                let elements = document.accessors().nth(*accessor).map_or(0, |a| a.count());
                if *count.get_or_insert(elements) != elements {
                    return Err(invalid(&format!(
                        "attribute {name} has {elements} elements where the others have {}",
                        count.unwrap_or(0)
                    )));
                }
                if !matches!(name.as_str(), "TRANSLATION" | "ROTATION" | "SCALE") {
                    ignored.push(Ignored::InstanceAttribute {
                        node: index,
                        name: name.clone(),
                    });
                }
            }
            let count = count.ok_or_else(|| invalid("it has no attributes, so no instances"))?;
            plan.nodes[index] = Some(NodeUse::Instanced { attributes, count });
        }
        Ok(plan)
    }

    /// How `node` takes the extension; `None` without it.
    pub(super) fn node(&self, node: usize) -> Option<&NodeUse> {
        self.nodes[node].as_ref()
    }
}

/// `node`'s instance transforms, each `world` times the instance's
/// translation, rotation and scale; a missing one is the identity. Every
/// attribute is checked first: its type, its count and that it lies within
/// the file's data (`check_accessor`, which bounds a count without a
/// buffer view by the file's buffers). A rotation is normalised, since a
/// normalized integer one is only near unit; one that is zero or not finite,
/// a translation or scale that is not finite, and an instance whose
/// transform is singular fail the load, naming the node and instance.
pub(super) fn transforms(
    node: &gltf::Node<'_>,
    world: Mat4,
    (attributes, count): (&[(String, usize)], usize),
    document: &gltf::Document,
    (buffers, buffer_bytes): (&[gltf::buffer::Data], usize),
) -> Result<Vec<Mat4>> {
    let index = node.index();
    let mut translations = None;
    let mut rotations = None;
    let mut scales = None;
    for (position, (name, accessor)) in attributes.iter().enumerate() {
        let accessor = document
            .accessors()
            .nth(*accessor)
            .ok_or("instance accessor out of range")?;
        let what = format!("node {index}'s {EXTENSION} {name}");
        // The first attribute's count is bounded by the file's data; the
        // rest must match it.
        let fixed = (position > 0).then_some(count);
        let all = [
            F32,
            I8,
            gltf::accessor::DataType::U8,
            I16,
            gltf::accessor::DataType::U16,
            gltf::accessor::DataType::U32,
        ];
        let shapes = [
            Dimensions::Scalar,
            Dimensions::Vec2,
            Dimensions::Vec3,
            Dimensions::Vec4,
            Dimensions::Mat2,
            Dimensions::Mat3,
            Dimensions::Mat4,
        ];
        let (types, shapes): (&[_], &[_]) = match name.as_str() {
            "TRANSLATION" | "SCALE" => (&[F32], &[Dimensions::Vec3]),
            "ROTATION" => (&[F32, I8, I16], &[Dimensions::Vec4]),
            _ => (&all, &shapes),
        };
        check_accessor(
            &accessor,
            &what,
            types,
            shapes,
            fixed,
            (buffers, buffer_bytes),
        )?;
        match name.as_str() {
            "TRANSLATION" => translations = Some(elements::<3>(&accessor, buffers, &what)?),
            "SCALE" => scales = Some(elements::<3>(&accessor, buffers, &what)?),
            "ROTATION" => {
                if accessor.data_type() != F32 && !accessor.normalized() {
                    return Err(format!("{what} accessor {} holds unnormalized {:?}, where the extension allows FLOAT or normalized BYTE or SHORT; re-export valid glTF", accessor.index(), accessor.data_type()).into());
                }
                rotations = Some(elements::<4>(&accessor, buffers, &what)?);
            }
            _ => {}
        }
    }
    (0..count)
        .map(|instance| {
            let invalid = |what: &str| -> Box<dyn std::error::Error> {
                format!("node {index}'s {EXTENSION} instance {instance}: {what}; re-export valid glTF").into()
            };
            let translation = translations
                .as_ref()
                .map_or(Vec3::ZERO, |values| Vec3::from_array(values[instance]));
            let scale = scales
                .as_ref()
                .map_or(Vec3::ONE, |values| Vec3::from_array(values[instance]));
            if !translation.is_finite() || !scale.is_finite() {
                return Err(invalid("its translation or scale is not finite"));
            }
            let rotation = match &rotations {
                None => Quat::IDENTITY,
                Some(values) => Vec4::from_array(values[instance])
                    .try_normalize()
                    .map(Quat::from_vec4)
                    .ok_or_else(|| invalid("its rotation is zero or not finite"))?,
            };
            let transform = world * Mat4::from_scale_rotation_translation(scale, rotation, translation);
            if singular(transform) {
                return Err(invalid("its transform, with its node's, is singular or not finite; give it a nonzero scale"));
            }
            Ok(transform)
        })
        .collect()
}

/// `accessor`'s elements as floats. glTF requires a buffer view or sparse
/// values of an accessor, which the document's validation enforces.
fn elements<const N: usize>(
    accessor: &gltf::Accessor<'_>,
    buffers: &[gltf::buffer::Data],
    what: &str,
) -> Result<Vec<[f32; N]>>
where
    [f32; N]: Item,
    [i8; N]: Item,
    [u8; N]: Item,
    [i16; N]: Item,
    [u16; N]: Item,
{
    read::<N>(accessor, buffers).ok_or_else(|| {
        format!(
            "{what} accessor {} holds no data; re-export valid glTF",
            accessor.index()
        )
        .into()
    })
}
