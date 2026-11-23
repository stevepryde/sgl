//! The loader's image step in the browser: the browser decodes each image
//! off the page's thread (`createImageBitmap`), as three.js r187's
//! `GLTFLoader` has it do (`ImageBitmapLoader`, `src/loaders/
//! ImageBitmapLoader.js`: `premultiplyAlpha: 'none'`,
//! `colorSpaceConversion: 'none'`; MIT), and the image stays the browser's
//! (`Image::Browser`). Every image first passes the checks the `image`
//! crate's decode does (`sources::open`). The `image` crate decodes one
//! with an EXIF orientation, which the browser applies (HTML's
//! `imageOrientation` has no value that ignores it) and the `image` crate
//! does not, and one the browser cannot decode or decodes to another size,
//! so every file a native load accepts loads, with the same texels.
use image::ImageDecoder;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use super::super::asset::{Asset, Result};
use super::super::images::{BrowserImage, Image};
use super::super::load_budget::{Budget, LoadBudget};
use super::image_step::image_error;
use super::{LoadOptions, load_slice_with_step, sources};

/// Loads an embedded glTF/GLB as [`load_slice_with_options`] does, with
/// each PNG or JPEG it decodes decoded by the browser, off the page's
/// thread, instead of by the `image` crate on the calling thread: the
/// browser's decoding of each starts as the loader reaches it, the loader
/// reads the rest of the file meanwhile, and the returned future completes
/// once every image has decoded. Each decoded image is an
/// [`Image::Browser`]; one whose EXIF orientation the browser would apply,
/// or that the browser cannot decode, is decoded by the `image` crate, as
/// natively, into an [`Image::Rgba8`]. Checks, budgets and options are
/// [`load_slice_with_options`]'s, and so are its errors, but that where
/// several images fail it may name a later one: the browser's failures are
/// known once the rest of the file is read.
///
/// [`load_slice_with_options`]: super::load_slice_with_options
pub async fn load_slice_in_browser(bytes: &[u8], options: LoadOptions<'_>) -> Result<Asset> {
    let mut decoding = Vec::new();
    let mut asset =
        load_slice_with_step(bytes, options, &mut |index, bytes, mime_type, budget| {
            decoding.push((index, Decoding::start(bytes, mime_type, budget)?));
            // Until the browser's decode completes below.
            Ok(Image::Rgba8(image::RgbaImage::new(1, 1)))
        })?;
    for (index, decoding) in decoding {
        asset.images[index] = decoding
            .finish()
            .await
            .map_err(|error| image_error(index, &error))?;
    }
    Ok(asset)
}

/// One PNG or JPEG decoded as [`load_slice_in_browser`] decodes a glTF's,
/// under the default [`LoadBudget`]: an image a game fetched itself, such as
/// a `.gltf`'s external image it supplies through `LoadOptions::images`, or
/// a sprite image.
pub async fn decode_image_in_browser(bytes: &[u8]) -> Result<Image> {
    let mut budget = Budget::decoded(LoadBudget::default());
    Decoding::start(bytes, None, &mut budget)?.finish().await
}

/// An image whose decode has started.
enum Decoding {
    /// Decoded by the `image` crate.
    Decoded(Image),
    /// The browser's decode of `blob`, with what its fallback needs.
    Browser {
        bitmap: JsFuture,
        blob: web_sys::Blob,
        size: [u32; 2],
        mime_type: Option<String>,
        /// The decoded budget left before the image was charged, which the
        /// `image` crate decodes under.
        remaining: u64,
    },
}

impl Decoding {
    /// Checks `bytes` and charges `budget` as the native decode does, then
    /// starts the browser's decode, or decodes them with the `image` crate
    /// where their EXIF orientation says the browser would turn them.
    fn start(bytes: &[u8], mime_type: Option<&str>, budget: &mut Budget) -> Result<Self> {
        let remaining = budget.remaining();
        let (mut decoder, _) = sources::open(bytes, mime_type, budget)?;
        let (width, height) = decoder.dimensions();
        if decoder.orientation()? != image::metadata::Orientation::NoTransforms {
            return Ok(Self::Decoded(decode_here(bytes, mime_type, remaining)?));
        }
        let array = js_sys::Uint8Array::new_with_length(bytes.len() as u32);
        array.copy_from(bytes);
        let blob = web_sys::Blob::new_with_u8_array_sequence(&js_sys::Array::of1(&array))
            .map_err(js_error)?;
        let options = web_sys::ImageBitmapOptions::new();
        options.set_premultiply_alpha(web_sys::PremultiplyAlpha::None);
        options.set_color_space_conversion(web_sys::ColorSpaceConversion::None);
        let global = js_sys::global();
        let promise = if let Some(window) = global.dyn_ref::<web_sys::Window>() {
            window.create_image_bitmap_with_blob_and_image_bitmap_options(&blob, &options)
        } else if let Some(worker) = global.dyn_ref::<web_sys::WorkerGlobalScope>() {
            worker.create_image_bitmap_with_blob_and_image_bitmap_options(&blob, &options)
        } else {
            return Err("no window or worker to decode images with".into());
        }
        .map_err(js_error)?;
        Ok(Self::Browser {
            bitmap: JsFuture::from(promise),
            blob,
            size: [width, height],
            mime_type: mime_type.map(str::to_owned),
            remaining,
        })
    }

    /// The decoded image, once the browser's decode completes.
    async fn finish(self) -> Result<Image> {
        let (bitmap, blob, size, mime_type, remaining) = match self {
            Self::Decoded(image) => return Ok(image),
            Self::Browser {
                bitmap,
                blob,
                size,
                mime_type,
                remaining,
            } => (bitmap, blob, size, mime_type, remaining),
        };
        if let Ok(bitmap) = bitmap.await {
            let image = BrowserImage {
                bitmap: bitmap.unchecked_into(),
            };
            // Another size means the browser turned it by metadata the
            // `image` crate did not read as an orientation.
            if image.size() == size {
                return Ok(Image::Browser(image));
            }
        }
        // The browser refused it, or turned it: the `image` crate decodes
        // it, or its error is the load's, as natively.
        let bytes = JsFuture::from(blob.array_buffer())
            .await
            .map_err(js_error)?;
        let bytes = js_sys::Uint8Array::new(&bytes).to_vec();
        decode_here(&bytes, mime_type.as_deref(), remaining)
    }
}

/// `bytes` decoded by the `image` crate with `remaining` decoded bytes
/// left, as the native load decodes them.
fn decode_here(bytes: &[u8], mime_type: Option<&str>, remaining: u64) -> Result<Image> {
    let mut budget = Budget::new(remaining, "decoded");
    Ok(Image::Rgba8(
        sources::decode_image(bytes, mime_type, &mut budget)?.into_rgba8(),
    ))
}

fn js_error(error: wasm_bindgen::JsValue) -> Box<dyn std::error::Error> {
    format!("{error:?}").into()
}
