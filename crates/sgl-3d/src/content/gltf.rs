//! glTF 2.0 import into an [`Asset`]: the document, its images, its scene's
//! mesh nodes, with rigid nodes' transforms baked into the vertices and
//! skinned ones' kept in bind space, its instanced nodes' meshes and
//! placements (`instancing`), its rig (`rig`): nodes, skins, morph weights
//! and animation clips, and its authored lights (`lights`).
use std::path::Path;

use glam::Mat4;

use super::asset::{Asset, Material, Result};
use super::images::Image;
use super::load_budget::{Budget, LoadBudget};
pub use instancing::{InstancedMesh, InstancedNode};
pub use lights::{AssetLights, LightPlacement, PlacedLight, PunctualKind, PunctualLight};
use material::read_material;
use meshes::{Walk, batch, ignored_attribute, read_node};
use rig::Rigging;

use accessors::{check_accessor, in_buffer};
use image_step::{ImageStep, decode_here, read_images};
use import::{Unread, import};
use scene_checks::{charge_scene, check_roots};

mod accessors;
mod attributes;
#[cfg(test)]
mod attributes_tests;
#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::{decode_image_in_browser, load_slice_in_browser};
mod image_step;
mod import;
mod instancing;
#[cfg(test)]
mod instancing_tests;
mod lights;
#[cfg(test)]
mod lights_tests;
mod material;
mod meshes;
mod meshopt;
#[cfg(test)]
mod meshopt_tests;
mod rig;
#[cfg(test)]
mod rig_tests;
mod scene_checks;
mod sources;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod texture_transform_tests;
mod topology;
#[cfg(test)]
mod topology_tests;

/// Optional application adaptations applied while loading an asset.
#[derive(Clone, Copy, Default)]
pub struct LoadOptions<'a> {
    /// Independent source and decoded safety caps, checked before expansion.
    /// Defaults to 4 GiB per cap natively and 1 GiB on wasm32. This does not
    /// reserve memory or raise image dimensions or device limits.
    pub budget: LoadBudget,
    /// Maximum authored emissive strength, applied before the emissive color.
    /// `None` preserves authored intensity. A cap must be finite and nonnegative.
    pub emissive_strength_cap: Option<f32>,
    /// Where each of the glTF's images a material map samples comes from,
    /// asked once per such image in image order. `None` decodes each one.
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
            .field("budget", &self.budget)
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
/// images resolve beneath its directory, without parent segments, schemes
/// or symlinks; buffers must match `byteLength`. Source reads are bounded,
/// and decoded scene data and images share a separate budget
/// ([`LoadOptions::budget`]); defaults are 4 GiB per cap natively and
/// 1 GiB on wasm32. Image sides are at most 16384, checked before decoding.
/// A browser has no file system: it fetches the
/// bytes and calls [`load_slice_with_options`].
pub fn load_with_options(path: &Path, options: LoadOptions<'_>) -> Result<Asset> {
    load_inner(path, options).map_err(|error| format!("{}: {error}", path.display()).into())
}

fn load_inner(path: &Path, options: LoadOptions<'_>) -> Result<Asset> {
    check(options)?;
    let mut budget = Budget::source(options.budget);
    let mut decoded_budget = Budget::decoded(options.budget);
    let bytes = budget.read_file(path, None)?;
    let (document, buffers, unread) = import(&bytes, path.parent(), (&mut budget, &mut decoded_budget)).map_err(|error| {
        format!("glTF import failed: {error}; check referenced files and re-export valid glTF from the Blender source")
    })?;
    decode(
        (document, unread),
        &buffers,
        path.parent(),
        options,
        (&mut budget, decoded_budget),
        &mut decode_here,
    )
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
    load_slice_with_step(bytes, options, &mut decode_here)
}

/// [`load_slice_with_options`] with `step` taking each image it decodes.
fn load_slice_with_step(
    bytes: &[u8],
    options: LoadOptions<'_>,
    step: ImageStep<'_>,
) -> Result<Asset> {
    check(options)?;
    let mut budget = Budget::source(options.budget);
    let mut decoded_budget = Budget::decoded(options.budget);
    budget.charge(bytes.len() as u64)?;
    let (document, buffers, unread) = import(bytes, None, (&mut budget, &mut decoded_budget))
        .map_err(|error| format!("embedded glTF import failed: {error}"))?;
    decode(
        (document, unread),
        &buffers,
        None,
        options,
        (&mut budget, decoded_budget),
        step,
    )
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
/// loader reads each itself, from the extension's values, for
/// `KHR_mesh_quantization` the integer attributes it allows (`attributes`),
/// and for the meshopt pair the compressed buffer views (`meshopt`).
const SUPPORTED_EXTENSIONS: [&str; 19] = [
    "KHR_lights_punctual",
    "KHR_materials_anisotropy",
    "KHR_materials_clearcoat",
    "KHR_materials_diffuse_transmission",
    "KHR_materials_dispersion",
    "KHR_materials_emissive_strength",
    "KHR_materials_ior",
    "KHR_materials_iridescence",
    "KHR_materials_sheen",
    "KHR_materials_specular",
    "KHR_materials_transmission",
    "KHR_materials_unlit",
    "KHR_materials_volume",
    "KHR_mesh_quantization",
    "KHR_meshopt_compression",
    "KHR_texture_transform",
    "EXT_materials_bump",
    "EXT_mesh_gpu_instancing",
    "EXT_meshopt_compression",
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
    /// channel of the material's metallic-roughness map on `TEXCOORD_0`
    /// (ORM packing: the same image through the same sampler), not yet
    /// from an image or sampler of its own.
    OcclusionMap { material: usize },
    /// Material `material`'s `KHR_materials_specular` specular or specular
    /// colour texture: SGL3D takes the extension's factors alone so far.
    SpecularMap { material: usize },
    /// Image `image` (an index into [`Asset::images`]), which no material
    /// map SGL3D samples uses, such as an extension's texture source or an
    /// ignored map's image: it is neither asked of
    /// [`LoadOptions::images`], read nor decoded, and its entry is a
    /// one-texel placeholder.
    Image { image: usize },
    /// Vertex attribute `name` of mesh `mesh`'s primitive `primitive`,
    /// which SGL3D does not read: a third or later UV set (`TEXCOORD_2`
    /// on), a second or later colour set (`COLOR_1` on) or an
    /// application-specific one (`_NAME`).
    Attribute {
        mesh: usize,
        primitive: usize,
        name: String,
    },
    /// Node `node`'s `EXT_mesh_gpu_instancing`, which the file uses
    /// without requiring it, on a skinned or morphed mesh, which SGL3D does
    /// not instance: the node loads as an ordinary mesh node, once, as a
    /// loader without the extension draws it. A file that requires the
    /// extension fails to load instead.
    Instancing { node: usize },
    /// Attribute `name` of morph target `target` of mesh `mesh`'s primitive
    /// `primitive`, whose displacement SGL3D does not apply: any but
    /// `POSITION`, `NORMAL` and `TANGENT`, such as a `TEXCOORD_n` or
    /// `COLOR_n`, which glTF lets a loader leave out (glTF 2.0 3.7.2.2).
    /// The target's other displacements still apply.
    MorphTargetAttribute {
        mesh: usize,
        primitive: usize,
        target: usize,
        name: String,
    },
    /// Instance attribute `name` of node `node`'s `EXT_mesh_gpu_instancing`,
    /// which SGL3D does not read: an application-specific one (`_ID`) or
    /// any other but `TRANSLATION`, `ROTATION` and `SCALE`. It is checked
    /// like them, and counts toward their shared count.
    InstanceAttribute { node: usize, name: String },
}

fn decode(
    (document, unread): (gltf::Document, Unread),
    buffers: &[gltf::buffer::Data],
    base: Option<&Path>,
    options: LoadOptions<'_>,
    (budget, mut decoded_budget): (&mut Budget, Budget),
    step: ImageStep<'_>,
) -> Result<Asset> {
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .ok_or("GLB contains no scene")?;
    let parents = rig::parents(&document)?;
    // What the file uses but does not require (import refused the rest).
    let mut ignored: Vec<Ignored> = document
        .extensions_used()
        .filter(|extension| !SUPPORTED_EXTENSIONS.contains(extension))
        .map(|extension| Ignored::Extension(extension.to_owned()))
        .collect();
    // The roots first: every walk after it, the plan's included, then
    // visits each node of the scene once.
    check_roots(&scene, &parents)?;
    let plan = instancing::Plan::read(&document, &scene, unread.instancing_required, &mut ignored)?;
    charge_scene(&document, &scene, &plan, &mut decoded_budget)
        .map_err(|error| format!("scene {}: {error}", scene.index()))?;
    let lights = lights::read(&document, &scene)?;
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
    let images = read_images(
        &document,
        buffers,
        base,
        options,
        &materials,
        &mut ignored,
        (budget, &mut decoded_budget, step),
    )?;
    for mesh in document.meshes() {
        for primitive in mesh.primitives() {
            ignored.extend(
                primitive
                    .attributes()
                    .filter(|(semantic, _)| ignored_attribute(semantic))
                    .map(|(semantic, _)| Ignored::Attribute {
                        mesh: mesh.index(),
                        primitive: primitive.index(),
                        name: semantic.to_string(),
                    }),
            );
        }
    }
    ignored.extend(unread.morph_attributes);
    // glTF primitives may omit a material; retain the specification default.
    let default_material = materials.len();
    materials.push(Material::default());
    let buffer_bytes = buffers.iter().map(|buffer| buffer.0.len()).sum();
    let mut rigging = Rigging::new(&document, &parents, (buffers, buffer_bytes))?;
    let include_node = options.nodes.unwrap_or(&|_| true);
    let mut walk = Walk::default();
    for node in scene.nodes() {
        read_node(
            node,
            Mat4::IDENTITY,
            (&document, &plan, include_node),
            (buffers, buffer_bytes),
            (default_material, &materials),
            &mut rigging,
            &mut walk,
        )?;
    }
    // After batching, which leaves out meshes without triangles, such as a
    // strip of only degenerates.
    let meshes = batch(walk.meshes, materials.len());
    let instanced: Vec<InstancedMesh> = walk
        .instanced
        .into_iter()
        .map(|read| InstancedMesh {
            mesh: read.mesh,
            meshes: batch(read.primitives, materials.len()),
            nodes: read.nodes,
        })
        .filter(|instanced| !instanced.meshes.is_empty())
        .collect();
    if meshes.is_empty() && instanced.is_empty() {
        return Err("scene has no triangle meshes".into());
    }
    Ok(Asset {
        meshes,
        materials,
        images,
        rig: rigging.rig,
        ignored,
        lights,
        instanced,
    })
}
