//! glTF 2.0 import into an [`Asset`]: the document, its images, its scene's
//! mesh nodes, with rigid nodes' transforms baked into the vertices and
//! skinned ones' kept in bind space, and its rig (`rig`): nodes, skins,
//! morph weights and animation clips.
use std::path::Path;

use glam::Mat4;
use gltf::accessor::{DataType, Dimensions};

use super::asset::{Asset, Material, Result};
use super::images::Image;
use material::read_material;
use meshes::{batch, read_node};
use rig::Rigging;

mod material;
mod meshes;
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
const SUPPORTED_EXTENSIONS: [&str; 13] = [
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
                    if let gltf::image::Source::View { view, .. } = image.source()
                        && !in_buffer(&view)
                    {
                        return Err(format!("image {index}'s buffer view does not fit its buffer; re-export valid glTF").into());
                    }
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

/// Refuses an accessor the gltf crate's readers cannot read, which its
/// validation lets through and its readers (gltf 1.4.1 `mesh::Reader`,
/// `skin::Reader`, `animation::util::Reader` and `accessor::util::Iter`)
/// assume away, panicking or misreading otherwise: one holding a component
/// type or shape glTF 2.0 does not allow for its use (`types`, `shapes`),
/// one of no elements, and one whose elements or sparse substitutions do
/// not lie within their buffer views at a stride no narrower than an
/// element, or whose views do not lie within their buffers. `what` names
/// its use.
fn check_accessor(
    accessor: &gltf::Accessor<'_>,
    what: &str,
    types: &[DataType],
    shapes: &[Dimensions],
) -> Result<()> {
    let index = accessor.index();
    let (data_type, shape) = (accessor.data_type(), accessor.dimensions());
    if !types.contains(&data_type) || !shapes.contains(&shape) {
        return Err(format!("{what} accessor {index} holds {data_type:?} {shape:?}, where glTF allows {types:?} {shapes:?}; re-export valid glTF").into());
    }
    let sparse = accessor.sparse();
    if accessor.count() == 0 || sparse.as_ref().is_some_and(|sparse| sparse.count() == 0) {
        return Err(
            format!("{what} accessor {index} has no elements; re-export valid glTF").into(),
        );
    }
    let size = accessor.size();
    let within = accessor
        .view()
        .is_none_or(|view| fits(&view, accessor.offset(), accessor.count(), size))
        && sparse.is_none_or(|sparse| {
            let (indices, values) = (sparse.indices(), sparse.values());
            fits(
                &indices.view(),
                indices.offset(),
                sparse.count(),
                indices.index_type().size(),
            ) && fits(&values.view(), values.offset(), sparse.count(), size)
        });
    if !within {
        return Err(format!(
            "{what} accessor {index} does not fit its buffer view and buffer; re-export valid glTF"
        )
        .into());
    }
    Ok(())
}

/// Whether `count` elements of `size` bytes from `offset` lie within `view`
/// at its stride, an element's when it has none, as gltf's
/// `accessor::util::Iter::new` reads them, and `view` within its buffer.
fn fits(view: &gltf::buffer::View<'_>, offset: usize, count: usize, size: usize) -> bool {
    let stride = view.stride().unwrap_or(size);
    in_buffer(view)
        && stride >= size
        && count
            .checked_sub(1)
            .and_then(|last| last.checked_mul(stride))
            .and_then(|start| start.checked_add(offset))
            .and_then(|start| start.checked_add(size))
            .is_some_and(|end| end <= view.length())
}

/// Whether `view` lies within its buffer, which gltf slices it from
/// unchecked (gltf 1.4.1 `accessor::util::buffer_view_slice` and
/// `image::Data::from_source` add its offset and length unchecked, and the
/// latter indexes the buffer's data with them). `import_buffers` holds each
/// buffer's data to at least its declared length.
fn in_buffer(view: &gltf::buffer::View<'_>) -> bool {
    view.offset()
        .checked_add(view.length())
        .is_some_and(|end| end <= view.buffer().length())
}
