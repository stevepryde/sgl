//! glTF 2.0 import into an [`Asset`]: the document, its images, its scene's
//! mesh nodes, with rigid nodes' transforms baked into the vertices and
//! skinned ones' kept in bind space, and its rig (`rig`): nodes, skins,
//! morph weights and animation clips.
use std::path::Path;

use glam::{Mat4, Vec3};
use gltf::{
    mesh::Mode,
    texture::{MagFilter, MinFilter},
};
use std::collections::HashMap;

use super::asset::{Asset, CpuMesh, Material, Result, Vertex};
use super::deformation::{MeshDeformation, MorphTarget};
use super::images::Image;
use material::read_material;
use rig::{Rigging, read_influences, read_morph_targets};

mod material;
mod rig;
#[cfg(test)]
mod rig_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

/// Optional application adaptations applied while loading an asset.
#[derive(Clone, Copy, Default)]
pub struct LoadOptions<'a> {
    /// Maximum authored emissive strength, applied before the emissive color.
    /// `None` preserves authored intensity. A cap must be finite and nonnegative.
    pub emissive_strength_cap: Option<f32>,
    /// Where each of the glTF's images comes from, asked once per image in
    /// image order. `None` decodes every image.
    #[allow(clippy::type_complexity)]
    pub images: Option<&'a (dyn Fn(GltfImage<'_>) -> Result<ImageSource> + Sync)>,
    /// Which of the scene's mesh nodes load, asked once per mesh node with
    /// its authored name; `None` loads every one. Every ancestor's
    /// transform applies, selected or not, and each descendant is asked
    /// itself. Selecting none is an error.
    #[allow(clippy::type_complexity)]
    pub nodes: Option<&'a (dyn Fn(Option<&str>) -> bool + Sync)>,
}

impl std::fmt::Debug for LoadOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadOptions")
            .field("emissive_strength_cap", &self.emissive_strength_cap)
            .field("images", &self.images.map(|_| "Fn"))
            .field("nodes", &self.nodes.map(|_| "Fn"))
            .finish()
    }
}

/// A glTF image the loader is about to read, as `LoadOptions::images` sees
/// it.
#[derive(Clone, Copy, Debug)]
pub struct GltfImage<'a> {
    /// Its index in the glTF, which materials' texture indices and
    /// `Asset::images` use.
    pub index: usize,
    /// Its authored name.
    pub name: Option<&'a str>,
    /// The file it refers to, relative to the glTF and percent-encoded, as
    /// authored; `None` when it is embedded (a buffer view or a data URI).
    pub uri: Option<&'a str>,
}

/// Where a loaded asset's image comes from, as Bevy's glTF loader resolves
/// each image's source (`load_image`): the glTF's own, decoded, or the
/// game's.
#[derive(Clone, Debug)]
pub enum ImageSource {
    /// Read and decode the glTF's image: an 8-bit PNG or JPEG, embedded or
    /// beside a glTF file.
    Decode,
    /// This image instead, such as a compressed chain from the game's export
    /// step. The glTF's image is neither read nor decoded.
    Supplied(Image),
}

/// Load a `.gltf` or `.glb` file, preserving authored emissive strength.
pub fn load(path: &Path) -> Result<Asset> {
    load_with_options(path, LoadOptions::default())
}

/// Load a file with explicit application adaptations. External buffers and
/// images resolve beside it. A browser has no file system: it fetches the
/// bytes and calls [`load_slice_with_options`].
pub fn load_with_options(path: &Path, options: LoadOptions<'_>) -> Result<Asset> {
    load_inner(path, options).map_err(|error| format!("{}: {error}", path.display()).into())
}

fn load_inner(path: &Path, options: LoadOptions<'_>) -> Result<Asset> {
    check(options)?;
    let bytes = std::fs::read(path)?;
    let (document, buffers) = import(&bytes, path.parent()).map_err(|error| {
        format!("glTF import failed: {error}; check referenced files and re-export valid glTF from the Blender source")
    })?;
    decode(document, &buffers, path.parent(), options)
}

/// Load an embedded glTF/GLB with the same material and geometry rules as [`load`].
/// External file URIs cannot be resolved from bytes; embed buffers and images,
/// or supply the images ([`load_slice_with_options`]).
pub fn load_slice(bytes: &[u8]) -> Result<Asset> {
    load_slice_with_options(bytes, LoadOptions::default())
}

/// Load an embedded glTF/GLB with explicit application adaptations, under
/// the same rules as [`load_with_options`]. An image the options supply
/// may be an external file the bytes cannot resolve.
pub fn load_slice_with_options(bytes: &[u8], options: LoadOptions<'_>) -> Result<Asset> {
    check(options)?;
    let (document, buffers) =
        import(bytes, None).map_err(|error| format!("embedded glTF import failed: {error}"))?;
    decode(document, &buffers, None, options)
}

fn check(options: LoadOptions<'_>) -> Result<()> {
    if options
        .emissive_strength_cap
        .is_some_and(|cap| !cap.is_finite() || cap < 0.0)
    {
        return Err("emissive strength cap must be finite and nonnegative".into());
    }
    Ok(())
}

/// The glTF extensions SGL3D honours: a file may require any of them. The
/// loader reads each itself, from the extension's values.
const SUPPORTED_EXTENSIONS: [&str; 10] = [
    "KHR_materials_anisotropy",
    "KHR_materials_clearcoat",
    "KHR_materials_dispersion",
    "KHR_materials_emissive_strength",
    "KHR_materials_ior",
    "KHR_materials_specular",
    "KHR_materials_transmission",
    "KHR_materials_unlit",
    "KHR_materials_volume",
    "EXT_materials_bump",
];

/// Something in a glTF that SGL3D does not render and the file does not
/// require, which glTF lets a loader leave out (glTF 2.0 5.17.1): the load
/// lists it in [`Asset::ignored`] and renders the rest. What the file
/// requires is never left out: a load fails on it instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ignored {
    /// An extension the file lists in `extensionsUsed` but not in
    /// `extensionsRequired`, which SGL3D does not support. What it adds is
    /// left out: a texture takes its core source, a material its core
    /// values.
    Extension(String),
    /// Material `material`'s occlusion map (an index into
    /// [`Asset::materials`]): SGL3D samples occlusion only from the red
    /// channel of the material's metallic-roughness image on `TEXCOORD_0`
    /// (ORM packing), not yet from an image of its own.
    OcclusionMap { material: usize },
    /// Material `material`'s `KHR_materials_specular` specular or specular
    /// colour texture: SGL3D takes the extension's factors alone so far.
    SpecularMap { material: usize },
}

/// The document in `bytes`, validated, and its buffers. glTF 2.0 5.17.2: a
/// file that requires an extension SGL3D does not support fails to load.
/// The gltf crate's validation knows none of the material extensions SGL3D
/// reads itself, so once every required one is known to be supported the
/// list is cleared for it; all its structural validation remains.
fn import(bytes: &[u8], base: Option<&Path>) -> Result<(gltf::Document, Vec<gltf::buffer::Data>)> {
    let gltf::Gltf { document, blob } = gltf::Gltf::from_slice_without_validation(bytes)?;
    let mut root = document.into_json();
    if let Some(extension) = root
        .extensions_required
        .iter()
        .find(|extension| !SUPPORTED_EXTENSIONS.contains(&extension.as_str()))
    {
        return Err(format!("the file requires glTF extension {extension}, which SGL3D does not support; bake it into the export or add renderer support").into());
    }
    root.extensions_required.clear();
    let document = gltf::Document::from_json(root)?;
    let buffers = gltf::import_buffers(&document, base, blob)?;
    Ok((document, buffers))
}

/// `document`'s images, each from where `options.images` says, decoded
/// where it says nothing. Bevy's glTF loader resolves each image's source
/// as it loads (9d12036 `crates/bevy_gltf/src/loader/mod.rs` `load_image`):
/// it decodes an embedded image and leaves an external one to its asset
/// server, so the loader never decodes it. Here the game, which has no
/// asset server, chooses for each image. A file's images resolve beside it
/// (`base`).
fn read_images(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    base: Option<&Path>,
    options: LoadOptions<'_>,
) -> Result<Vec<Image>> {
    document
        .images()
        .map(|image| {
            let index = image.index();
            let file = match image.source() {
                gltf::image::Source::Uri { uri, .. } if !uri.starts_with("data:") => Some(uri),
                _ => None,
            };
            let source = match options.images {
                None => ImageSource::Decode,
                Some(sources) => sources(GltfImage {
                    index,
                    name: image.name(),
                    uri: file,
                })
                .map_err(|error| format!("image {index}: {error}"))?,
            };
            match source {
                ImageSource::Supplied(supplied) => Ok(supplied),
                ImageSource::Decode => {
                    // An embedded image reads no file, but gltf 1.4.1's
                    // `image::Data::from_source` refuses every URI without
                    // a base path, a data URI's too. Give an embedded image
                    // an unused one, so bytes decode a data URI as a file
                    // load does, and as Bevy's `load_image` does either way.
                    let base = match file {
                        Some(_) => base,
                        None => Some(Path::new("")),
                    };
                    let data = gltf::image::Data::from_source(image.source(), base, buffers)
                        .map_err(|error| format!("image {index} import failed: {error}; check referenced files and re-export valid glTF from the Blender source"))?;
                    rgba(index, data)
                }
            }
        })
        .collect()
}

/// A decoded 8-bit image as RGBA8.
fn rgba(index: usize, data: gltf::image::Data) -> Result<Image> {
    use gltf::image::Format;
    let channels = match data.format {
        Format::R8 => 1,
        Format::R8G8 => 2,
        Format::R8G8B8 => 3,
        Format::R8G8B8A8 => 4,
        other => {
            return Err(format!(
                "image {index} has unsupported {other:?} pixels; export an 8-bit PNG or JPEG"
            )
            .into());
        }
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
        .map(Image::Rgba8)
        .ok_or_else(|| format!("image {index} has inconsistent pixel dimensions").into())
}

fn decode(
    document: gltf::Document,
    buffers: &[gltf::buffer::Data],
    base: Option<&Path>,
    options: LoadOptions<'_>,
) -> Result<Asset> {
    // What the file uses but does not require (import refused the rest).
    let mut ignored: Vec<Ignored> = document
        .extensions_used()
        .filter(|extension| !SUPPORTED_EXTENSIONS.contains(extension))
        .map(|extension| Ignored::Extension(extension.to_owned()))
        .collect();
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
        .enumerate()
        .map(|(index, material)| {
            let strength = material.emissive_strength().unwrap_or(1.0);
            let mut result = read_material(material, &document, index, &mut ignored)?;
            if let Some(cap) = options.emissive_strength_cap
                && strength > cap
            {
                result.emissive = result.emissive.map(|value| value * cap / strength);
            }
            Ok(result)
        })
        .collect::<Result<Vec<_>>>()?;
    // glTF primitives may omit a material; retain the specification default.
    let default_material = materials.len();
    materials.push(Material::default());
    let images = read_images(&document, buffers, base, options)?;
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .ok_or("GLB contains no scene")?;
    let mut rigging = Rigging::new(&document, buffers)?;
    let include_node = options.nodes.unwrap_or(&|_| true);
    let mut meshes = Vec::new();
    for node in scene.nodes() {
        read_node(
            node,
            Mat4::IDENTITY,
            buffers,
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
        ignored,
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
                || material.occlusion_texture.is_some()
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
