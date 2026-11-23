//! The loader's image step: which of a document's images the load reads,
//! where each comes from (`LoadOptions::images`), and how one it decodes
//! becomes an [`Image`].

use std::path::Path;

use super::super::asset::{Material, Result};
use super::super::images::Image;
use super::super::load_budget::Budget;
use super::{GltfImage, Ignored, ImageSource, LoadOptions, in_buffer, sources};

/// How a load turns an image it decodes into an [`Image`], given its index,
/// its encoded bytes, its MIME type and the decoded budget: natively the
/// `image` crate decodes it there (`decode_here`); in the browser the
/// browser's decode starts there, and the load awaits it after
/// (`load_slice_in_browser`).
pub(super) type ImageStep<'s> =
    &'s mut dyn FnMut(usize, &[u8], Option<&str>, &mut Budget) -> Result<Image>;

/// The native image step: the `image` crate decodes it here.
pub(super) fn decode_here(
    _: usize,
    bytes: &[u8],
    mime_type: Option<&str>,
    budget: &mut Budget,
) -> Result<Image> {
    Ok(Image::Rgba8(
        sources::decode_image(bytes, mime_type, budget)?.into_rgba8(),
    ))
}

/// The error a load reports for image `index`'s `error`.
pub(super) fn image_error(index: usize, error: &dyn std::fmt::Display) -> String {
    format!(
        "image {index} import failed: {error}; check referenced files and re-export valid glTF from the Blender source"
    )
}

/// `document`'s images, each one `materials` sample from where
/// `options.images` says, decoded where it says nothing. Bevy's glTF loader
/// resolves each image's source as it loads (9d12036
/// `crates/bevy_gltf/src/loader/mod.rs` `load_image`): it decodes an
/// embedded image and leaves an external one to its asset server, so the
/// loader never decodes it. Here the game, which has no asset server,
/// chooses for each image. A file's images resolve beside it (`base`). An
/// image no material samples is a placeholder, listed in `ignored`: glTF
/// lets a loader leave out what an unsupported extension adds (glTF 2.0
/// 5.17.1), such as a texture's `KHR_texture_basisu` or `EXT_texture_webp`
/// source, which a loader without it replaces with the core `source`, and
/// SGL3D never binds an ignored map's image. Each image decoded is read
/// under the source budget and charged to the decoded one, and `step`
/// decodes it.
pub(super) fn read_images(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    base: Option<&Path>,
    options: LoadOptions<'_>,
    materials: &[Material],
    ignored: &mut Vec<Ignored>,
    (budget, decoded_budget, decode_step): (&mut Budget, &mut Budget, ImageStep<'_>),
) -> Result<Vec<Image>> {
    let mut sampled = vec![false; document.images().len()];
    for image in materials.iter().flat_map(sampled_images) {
        sampled[image] = true;
    }
    document
        .images()
        .map(|image| {
            let index = image.index();
            if !sampled[index] {
                ignored.push(Ignored::Image { image: index });
                return Ok(Image::Rgba8(image::RgbaImage::new(1, 1)));
            }
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
                ImageSource::Supplied(supplied) => {
                    supplied.charge_bytes(decoded_budget)
                        .map_err(|error| format!("supplied image {index}: {error}"))?;
                    Ok(supplied)
                },
                ImageSource::Decode => {
                    if let gltf::image::Source::View { view, .. } = image.source()
                        && !in_buffer(&view)
                    {
                        return Err(format!("image {index}'s buffer view does not fit its buffer; re-export valid glTF").into());
                    }
                    let (bytes, mime_type) = sources::encoded(image.source(), base, buffers, budget)
                        .map_err(|error| image_error(index, &error))?;
                    decode_step(index, &bytes, mime_type, decoded_budget)
                        .map_err(|error| image_error(index, &error).into())
                }
            }
        })
        .collect()
}

/// The images `material`'s maps sample: every one it names but an occlusion
/// map's outside its metallic-roughness image, which SGL3D does not sample.
fn sampled_images(material: &Material) -> Vec<usize> {
    let mut images: Vec<usize> = material
        .textures()
        .into_iter()
        .flatten()
        .map(|texture| texture.image)
        .collect();
    if let Some(occlusion) = material
        .occlusion_texture
        .filter(|_| !material.packed_occlusion())
        && let Some(slot) = images.iter().position(|&image| image == occlusion.image)
    {
        images.swap_remove(slot);
    }
    images
}
