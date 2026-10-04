//! glTF 2.0 import into an [`Asset`]: the document, its images, its scene's
//! mesh nodes, with rigid nodes' transforms baked into the vertices and
//! skinned ones' kept in bind space, and its rig (`rig`): nodes, skins,
//! morph weights and animation clips.
use std::path::Path;

use glam::{Mat4, Vec3};
use gltf::{
    mesh::Mode,
    texture::{MagFilter, MinFilter, WrappingMode},
};
use std::collections::HashMap;

use super::asset::{Asset, CpuMesh, Material, Result, Vertex};
use super::deformation::{MeshDeformation, MorphTarget};
use material::read_material;
use rig::{Rigging, read_influences, read_morph_targets};

mod material;
mod rig;
#[cfg(test)]
mod rig_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

/// Optional application adaptations applied while loading an asset.
#[derive(Clone, Copy, Debug, Default)]
pub struct LoadOptions {
    /// Maximum authored emissive strength, applied before the emissive color.
    /// `None` preserves authored intensity. A cap must be finite and nonnegative.
    pub emissive_strength_cap: Option<f32>,
}

/// Load a `.gltf` or `.glb` file, preserving authored emissive strength.
pub fn load(path: &Path) -> Result<Asset> {
    load_with_options(path, LoadOptions::default())
}

/// Load a file with explicit application adaptations. External buffers and
/// images resolve beside it. A browser has no file system: it fetches the
/// bytes and calls [`load_slice_with_options`].
pub fn load_with_options(path: &Path, options: LoadOptions) -> Result<Asset> {
    load_inner(path, options).map_err(|error| format!("{}: {error}", path.display()).into())
}

fn load_inner(path: &Path, options: LoadOptions) -> Result<Asset> {
    check(options)?;
    let bytes = std::fs::read(path)?;
    let (document, buffers, images) = import(&bytes, path.parent()).map_err(|error| {
        format!("glTF import failed: {error}; check referenced files and re-export valid glTF from the Blender source")
    })?;
    decode(
        document,
        buffers,
        images,
        options.emissive_strength_cap,
        &|_| true,
    )
}

/// Load an embedded glTF/GLB with the same material and geometry rules as [`load`].
/// External file URIs cannot be resolved from bytes; embed buffers and images.
pub fn load_slice(bytes: &[u8]) -> Result<Asset> {
    load_slice_with_options(bytes, LoadOptions::default())
}

/// Load an embedded glTF/GLB with explicit application adaptations, under
/// the same rules as [`load_with_options`].
pub fn load_slice_with_options(bytes: &[u8], options: LoadOptions) -> Result<Asset> {
    check(options)?;
    load_embedded(bytes, options, &|_| true)
}

/// Load mesh nodes selected by the caller from an embedded asset.
/// The predicate sees each mesh node's optional authored name. All ancestor
/// transforms remain applied, including ancestors excluded by the predicate.
/// Selection does not implicitly include descendants; each mesh node is tested.
/// Unsupported content is rejected under the same rules as file loading.
pub fn load_slice_filtered(
    bytes: &[u8],
    include_node: impl Fn(Option<&str>) -> bool,
) -> Result<Asset> {
    load_embedded(bytes, LoadOptions::default(), &include_node)
}

fn load_embedded(
    bytes: &[u8],
    options: LoadOptions,
    include_node: &dyn Fn(Option<&str>) -> bool,
) -> Result<Asset> {
    let (document, buffers, images) =
        import(bytes, None).map_err(|error| format!("embedded glTF import failed: {error}"))?;
    decode(
        document,
        buffers,
        images,
        options.emissive_strength_cap,
        include_node,
    )
}

fn check(options: LoadOptions) -> Result<()> {
    if options
        .emissive_strength_cap
        .is_some_and(|cap| !cap.is_finite() || cap < 0.0)
    {
        return Err("emissive strength cap must be finite and nonnegative".into());
    }
    Ok(())
}

// The gltf crate exposes generic extension values but does not recognize required
// KHR_materials_anisotropy. Remove only that declaration for its core validation;
// all structural validation and every other required extension remain enforced.
fn import(
    bytes: &[u8],
    base: Option<&Path>,
) -> Result<(
    gltf::Document,
    Vec<gltf::buffer::Data>,
    Vec<gltf::image::Data>,
)> {
    let gltf::Gltf { document, blob } = gltf::Gltf::from_slice_without_validation(bytes)?;
    let mut root = document.into_json();
    root.extensions_required
        .retain(|extension| extension != "KHR_materials_anisotropy");
    let document = gltf::Document::from_json(root)?;
    let buffers = gltf::import_buffers(&document, base, blob)?;
    let images = gltf::import_images(&document, base, &buffers)?;
    Ok((document, buffers, images))
}

fn decode(
    document: gltf::Document,
    buffers: Vec<gltf::buffer::Data>,
    images: Vec<gltf::image::Data>,
    emission_cap: Option<f32>,
    include_node: &dyn Fn(Option<&str>) -> bool,
) -> Result<Asset> {
    for extension in document.extensions_used() {
        if !matches!(
            extension,
            "KHR_materials_clearcoat"
                | "KHR_materials_emissive_strength"
                | "KHR_materials_unlit"
                | "KHR_materials_anisotropy"
                | "EXT_materials_bump"
        ) {
            return Err(format!("unsupported glTF extension {extension}; bake it into the static export or add renderer support").into());
        }
    }
    for texture in document.textures() {
        let sampler = texture.sampler();
        if !matches!(sampler.mag_filter(), None | Some(MagFilter::Linear))
            || !matches!(
                sampler.min_filter(),
                None | Some(MinFilter::LinearMipmapLinear)
            )
        {
            return Err(format!("texture {} uses unsupported sampling; export linear magnification and trilinear minification", texture.index()).into());
        }
    }
    let mut materials = document
        .materials()
        .map(|material| {
            let strength = material.emissive_strength().unwrap_or(1.0);
            let mut result = read_material(material, &document)?;
            if let Some(cap) = emission_cap
                && strength > cap
            {
                result.emissive = result.emissive.map(|value| value * cap / strength);
            }
            Ok(result)
        })
        .collect::<Result<Vec<_>>>()?;
    // glTF primitives may omit a material; retain the specification default.
    let default_material = materials.len();
    materials.push(Material {
        name: String::new(),
        visibility_group: 0,
        casts_directional_shadow: true,
        base: [1.0; 4],
        emissive: [0.0; 3],
        metallic: 1.0,
        roughness: 1.0,
        clearcoat: 0.0,
        coat_roughness: 0.0,
        anisotropy_strength: 0.0,
        anisotropy_rotation: 0.0,
        anisotropy_texture: None,
        base_texture: None,
        mr_texture: None,
        emissive_texture: None,
        normal_texture: None,
        normal_scale: 1.0,
        bump_texture: None,
        bump_scale: 0.0,
        wrap: [WrappingMode::Repeat; 2],
        double_sided: false,
        unlit: false,
        alpha: crate::AlphaMode::Opaque,
    });
    let images = images
        .into_iter()
        .enumerate()
        .map(|(index, data)| {
            use gltf::image::Format;
            let channels = match data.format {
                Format::R8 => 1,
                Format::R8G8 => 2,
                Format::R8G8B8 => 3,
                Format::R8G8B8A8 => 4,
                other => return Err(format!(
                    "image {index} has unsupported {other:?} pixels; export an 8-bit PNG or JPEG"
                )
                .into()),
            };
            let rgba = data
                .pixels
                .chunks_exact(channels)
                .flat_map(|p| match channels {
                    1 => [p[0], p[0], p[0], 255],
                    2 => [p[0], p[0], p[0], p[1]],
                    3 => [p[0], p[1], p[2], 255],
                    _ => [p[0], p[1], p[2], p[3]],
                })
                .collect();
            image::RgbaImage::from_raw(data.width, data.height, rgba)
                .map(super::images::Image::Rgba8)
                .ok_or_else(|| format!("image {index} has inconsistent pixel dimensions").into())
        })
        .collect::<Result<Vec<_>>>()?;
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .ok_or("GLB contains no scene")?;
    let mut rigging = Rigging::new(&document, &buffers)?;
    let mut meshes = Vec::new();
    for node in scene.nodes() {
        read_node(
            node,
            Mat4::IDENTITY,
            &buffers,
            (default_material, &materials),
            &mut rigging,
            &mut meshes,
            include_node,
        )?;
    }
    if meshes.is_empty() {
        return Err("scene has no triangle meshes".into());
    }
    Ok(Asset {
        meshes: batch(meshes, materials.len()),
        materials,
        images,
        rig: rigging.rig,
    })
}

/// What primitives must share to be drawn as one mesh: a material, and
/// whether a skin and which node's morph weights deform them.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BatchKey {
    material: usize,
    skinned: bool,
    morphed: Option<usize>,
}

/// `meshes` merged by `BatchKey` to reduce draw calls: rigid ones by
/// material in material order, then deforming ones in first-use order.
fn batch(meshes: Vec<(BatchKey, CpuMesh)>, materials: usize) -> Vec<CpuMesh> {
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

fn read_node(
    node: gltf::Node<'_>,
    parent: Mat4,
    buffers: &[gltf::buffer::Data],
    (default_material, materials): (usize, &[Material]),
    rigging: &mut Rigging,
    meshes: &mut Vec<(BatchKey, CpuMesh)>,
    include_node: &dyn Fn(Option<&str>) -> bool,
) -> Result<()> {
    let global = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
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
        if !transform.is_finite() || transform.determinant().abs() < 1e-10 {
            return Err(format!(
                "node {} has a singular/nonfinite transform; apply a nonzero scale in Blender",
                node.index()
            )
            .into());
        }
        let normal_transform = transform.inverse().transpose();
        for primitive in mesh.primitives() {
            let label = format!("mesh {} primitive {}", mesh.index(), primitive.index());
            if primitive.mode() != Mode::Triangles {
                return Err(format!(
                    "{label}: only triangle primitives are supported; triangulate before export"
                )
                .into());
            }
            for (semantic, _) in primitive.attributes() {
                if matches!(
                    semantic,
                    gltf::Semantic::Joints(1..) | gltf::Semantic::Weights(1..)
                ) {
                    return Err(format!("{label}: more than four joint influences per vertex; limit influences to four in the export").into());
                }
                if !matches!(
                    semantic,
                    gltf::Semantic::Positions
                        | gltf::Semantic::Normals
                        | gltf::Semantic::Tangents
                        | gltf::Semantic::TexCoords(0)
                        | gltf::Semantic::Colors(0)
                        | gltf::Semantic::Joints(0)
                        | gltf::Semantic::Weights(0)
                ) {
                    return Err(format!("{label}: unsupported attribute {semantic:?}; add its renderer input or remove it from the export").into());
                }
            }
            let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()].0));
            let positions = reader
                .read_positions()
                .ok_or_else(|| format!("{label}: missing POSITION"))?
                .collect::<Vec<_>>();
            let normals = reader
                .read_normals()
                .ok_or_else(|| format!("{label}: missing NORMAL; export vertex normals"))?
                .collect::<Vec<_>>();
            let material_index = primitive.material().index().unwrap_or(default_material);
            let material = &materials[material_index];
            let tangents = reader
                .read_tangents()
                .map(|values| values.collect::<Vec<_>>());
            if material.anisotropy_strength > 0.0 && tangents.is_none() {
                return Err(format!("{label}: anisotropic material {} requires authored TANGENT; export tangents from Blender with vertex normals", material.name).into());
            }
            let uvs = reader
                .read_tex_coords(0)
                .map(|uv| uv.into_f32().collect::<Vec<_>>());
            let lightmap_uvs = reader
                .read_tex_coords(1)
                .map(|uv| uv.into_f32().collect::<Vec<_>>());
            let textured = material.base_texture.is_some()
                || material.mr_texture.is_some()
                || material.anisotropy_texture.is_some();
            if textured && uvs.is_none() {
                return Err(format!("{label}: textured primitive is missing TEXCOORD_0").into());
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
                        let transformed = transform.transform_vector3(direction);
                        let projected = transformed - normal * normal.dot(transformed);
                        if !direction.is_finite() || direction == Vec3::ZERO
                            || Vec3::from_array(normals[i]).cross(direction).try_normalize().is_none()
                            || !matches!(authored[3], -1.0 | 1.0)
                            || projected.try_normalize().is_none()
                        {
                            return Err(format!("{label}: vertex {i} has invalid TANGENT; export finite nonzero tangents perpendicular to NORMAL with handedness +1 or -1").into());
                        }
                        let tangent = projected.normalize();
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
                        color: colors.as_ref().map_or([1.0; 4], |colors| colors[i]),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let mut indices = reader
                .read_indices()
                .map(|indices| indices.into_u32().collect::<Vec<_>>())
                .unwrap_or_else(|| (0..vertices.len() as u32).collect());
            if indices.len() % 3 != 0
                || indices
                    .iter()
                    .any(|index| *index as usize >= vertices.len())
            {
                return Err(format!("{label}: invalid triangle indices").into());
            }
            if transform.determinant() < 0.0 {
                for triangle in indices.as_chunks_mut::<3>().0 {
                    triangle.swap(1, 2);
                }
            }
            let influences = match skin {
                Some(skin) => read_influences(&reader, skin, vertices.len(), &label)?,
                None => Vec::new(),
            };
            let target_count = primitive.morph_targets().count();
            let morph_targets = if target_count == 0 {
                Vec::new()
            } else {
                let first = rigging.morph_weights(&node, target_count)?;
                read_morph_targets(
                    &reader,
                    first,
                    (transform, normal_transform),
                    (&normals, tangents.as_deref()),
                    &label,
                )?
            };
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
    for child in node.children() {
        read_node(
            child,
            global,
            buffers,
            (default_material, materials),
            rigging,
            meshes,
            include_node,
        )?;
    }
    Ok(())
}
