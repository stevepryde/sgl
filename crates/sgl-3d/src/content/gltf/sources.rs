//! URI confinement and bounded source reads. Import validates the document
//! before using these readers; gltf's file importer allows unbounded reads.
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use super::super::asset::Result;
use super::super::load_budget::{Budget, MAX_IMAGE_SIDE};
use image::ImageDecoder;

impl Budget {
    pub(super) fn read_file(&mut self, path: &Path, declared: Option<usize>) -> Result<Vec<u8>> {
        // Check before opening: opening a named pipe could block even without
        // reading. Referenced paths have also rejected every symlink component.
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err("source must be a regular file, not a symlink or device".into());
        }
        if metadata.len() > self.remaining {
            return Err(format!(
                "source exceeds the remaining {} byte load budget",
                self.remaining
            )
            .into());
        }
        let length = declared.map_or(metadata.len(), |length| length as u64);
        if metadata.len() != length {
            return Err(format!(
                "buffer file has {} bytes, but byteLength declares {length}",
                metadata.len()
            )
            .into());
        }
        let file = std::fs::File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err("source must be a regular file".into());
        }
        // The sentinel detects a file changing size after metadata without
        // allowing its contents to determine an unbounded allocation.
        let mut bytes = Vec::new();
        file.take(length + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != length {
            return Err(
                format!("source length changed while reading (expected {length} bytes)").into(),
            );
        }
        self.charge(length)?;
        Ok(bytes)
    }
}

/// Decode before inspecting path components, including escaped separators.
fn relative_file(base: Option<&Path>, uri: &str) -> Result<PathBuf> {
    let mut decoded = Vec::with_capacity(uri.len());
    let mut bytes = uri.bytes();
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let digit = |byte: Option<u8>| {
                byte.and_then(|byte| (byte as char).to_digit(16))
                    .ok_or("invalid percent escape in URI")
            };
            (digit(bytes.next())? * 16 + digit(bytes.next())?) as u8
        } else {
            byte
        });
    }
    let decoded = std::str::from_utf8(&decoded)?;
    let path = Path::new(decoded);
    if decoded.is_empty()
        || decoded.contains([':', '\\', '\0'])
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(
            "URI must be a relative path without schemes, absolute paths or parent segments".into(),
        );
    }
    let mut resolved = base
        .ok_or("external URI cannot be resolved from embedded bytes")?
        .to_path_buf();
    for component in path.components() {
        if let Component::Normal(name) = component {
            resolved.push(name);
            if std::fs::symlink_metadata(&resolved)?
                .file_type()
                .is_symlink()
            {
                return Err("URI must not traverse a symlink".into());
            }
        }
    }
    Ok(resolved)
}

fn data_uri(uri: &str, budget: &mut Budget) -> Result<Vec<u8>> {
    let (_, encoded) = uri
        .strip_prefix("data:")
        .and_then(|uri| uri.split_once(";base64,"))
        .ok_or("data URI must contain base64 bytes")?;
    // A validated base64 string's decoded size, before gltf allocates it.
    // Invalid encodings are refused by its base64 decoder below.
    let padding = encoded
        .bytes()
        .rev()
        .take_while(|&byte| byte == b'=')
        .count();
    let length = (encoded.len() as u64 / 4) * 3
        + match encoded.len() % 4 {
            2 => 1,
            3 => 2,
            _ => 0,
        };
    let length = length.saturating_sub(padding.min(2) as u64);
    budget.charge(length)?;
    // No base path is ever passed to the gltf importer: this call cannot read
    // a file. It adds alignment padding, which is not part of the URI bytes.
    let mut decoded = gltf::buffer::Data::from_source(gltf::buffer::Source::Uri(uri), None)?.0;
    decoded.truncate(length as usize);
    Ok(decoded)
}

pub(super) fn buffers(
    document: &gltf::Document,
    base: Option<&Path>,
    mut blob: Option<&[u8]>,
    budget: &mut Budget,
) -> Result<Vec<gltf::buffer::Data>> {
    document
        .buffers()
        .map(|buffer| {
            let bytes = match buffer.source() {
                gltf::buffer::Source::Bin => {
                    let bytes = blob.take().ok_or("GLB buffer has no BIN chunk")?;
                    budget.charge(bytes.len() as u64)?;
                    // glTF 2.0 permits the BIN chunk's 4-byte alignment padding.
                    if bytes.len() < buffer.length() || bytes.len() - buffer.length() > 3 {
                        return Err(format!(
                            "buffer {}: BIN chunk length does not match byteLength",
                            buffer.index()
                        )
                        .into());
                    }
                    bytes[..buffer.length()].to_vec()
                }
                gltf::buffer::Source::Uri(uri) => {
                    let mut read = || -> Result<Vec<u8>> {
                        if uri.starts_with("data:") {
                            data_uri(uri, budget)
                        } else {
                            budget.read_file(&relative_file(base, uri)?, Some(buffer.length()))
                        }
                    };
                    let bytes = read().map_err(|error| {
                        format!("buffer {} URI {uri:?}: {error}", buffer.index())
                    })?;
                    if bytes.len() != buffer.length() {
                        return Err(format!(
                            "buffer {} URI {uri:?}: decoded bytes do not match byteLength {}",
                            buffer.index(),
                            buffer.length()
                        )
                        .into());
                    }
                    bytes
                }
            };
            Ok(gltf::buffer::Data(bytes))
        })
        .collect()
}

pub(super) fn image(
    source: gltf::image::Source<'_>,
    base: Option<&Path>,
    buffers: &[gltf::buffer::Data],
    budget: &mut Budget,
    decoded_budget: &mut Budget,
) -> Result<image::DynamicImage> {
    let (uri, mime_type) = match source {
        gltf::image::Source::View { view, mime_type } => {
            // Its buffer view has already been bounds-checked and charged once.
            let bytes =
                &buffers[view.buffer().index()].0[view.offset()..view.offset() + view.length()];
            return decode_image(bytes, Some(mime_type), decoded_budget);
        }
        gltf::image::Source::Uri { uri, mime_type } => (uri, mime_type),
    };
    let mut read = || -> Result<_> {
        if uri.starts_with("data:") {
            data_uri(uri, budget)
        } else {
            budget.read_file(&relative_file(base, uri)?, None)
        }
    };
    let bytes = read().map_err(|error| format!("URI {uri:?}: {error}"))?;
    let mime_type = mime_type.or_else(|| {
        uri.strip_prefix("data:")?
            .split_once(';')
            .map(|(mime, _)| mime)
    });
    decode_image(&bytes, mime_type, decoded_budget)
}

fn decode_image(
    bytes: &[u8],
    mime_type: Option<&str>,
    budget: &mut Budget,
) -> Result<image::DynamicImage> {
    let format = match mime_type {
        Some("image/png") => image::ImageFormat::Png,
        Some("image/jpeg") => image::ImageFormat::Jpeg,
        Some(_) => return Err("unsupported image encoding; export PNG or JPEG".into()),
        None => image::guess_format(bytes)?,
    };
    if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        return Err("unsupported image encoding; export PNG or JPEG".into());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(budget.remaining());
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    reader.limits(limits.clone());
    // into_decoder reads the header and enforces strict dimension limits;
    // it does not decode the pixels (image's PNG/JPEG decoders).
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| format!("PNG/JPEG header (maximum side {MAX_IMAGE_SIDE}): {error}"))?;
    let (width, height) = decoder.dimensions();
    if !matches!(
        decoder.color_type(),
        image::ColorType::L8
            | image::ColorType::La8
            | image::ColorType::Rgb8
            | image::ColorType::Rgba8
    ) {
        return Err(format!(
            "unsupported {:?} pixels; export an 8-bit PNG or JPEG",
            decoder.color_type()
        )
        .into());
    }
    // Account for the retained RGBA result, including greyscale's fourfold
    // expansion, before any pixel decoding or conversion allocates it.
    budget.charge(u64::from(width) * u64::from(height) * 4)?;
    // ImageReader::decode reserves its output separately from the decoder's
    // working memory; do the same when using its header decoder directly.
    limits.reserve(decoder.total_bytes())?;
    decoder.set_limits(limits)?;
    Ok(image::DynamicImage::from_decoder(decoder)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Defects: embedded sources reset rather than share the load budget,
    // or gltf's generated alignment padding changes a source's real length.
    // Oracle: base64's published encoding of one/two/three literal bytes,
    // and a 5-byte budget cannot hold two 3-byte sources.
    #[wasm_bindgen_test(unsupported = test)]
    fn data_uri_bytes_share_one_budget_without_alignment_padding() {
        for (uri, expected) in [
            ("data:application/octet-stream;base64,AQ==", &[1][..]),
            ("data:application/octet-stream;base64,AQI=", &[1, 2][..]),
            ("data:application/octet-stream;base64,AQID", &[1, 2, 3][..]),
            ("data:application/octet-stream;base64,AQ", &[1][..]),
            ("data:application/octet-stream;base64,AQI", &[1, 2][..]),
        ] {
            assert_eq!(
                data_uri(uri, &mut Budget { remaining: 5 }).unwrap(),
                expected
            );
        }
        let mut budget = Budget { remaining: 5 };
        assert_eq!(
            data_uri("data:application/octet-stream;base64,AQID", &mut budget).unwrap(),
            [1, 2, 3]
        );
        let error = data_uri("data:application/octet-stream;base64,BAUG", &mut budget)
            .unwrap_err()
            .to_string();
        assert!(error.contains("budget"), "{error}");
    }
    // Defect: RGBA expansion is charged per image or not charged at all.
    // Oracle: two 4x2 grey images retain 64 RGBA bytes, exceeding a 48-byte
    // shared budget even though their grey pixels need only 16 bytes total.
    #[wasm_bindgen_test(unsupported = test)]
    fn decoded_images_share_the_rgba_budget_and_preserve_grey_alpha() {
        use image::ImageEncoder;
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                &[10, 20, 30, 40, 50, 60, 70, 80],
                4,
                2,
                image::ExtendedColorType::L8,
            )
            .unwrap();
        let mut budget = Budget { remaining: 48 };
        let image = decode_image(&bytes, Some("image/png"), &mut budget)
            .unwrap()
            .into_rgba8();
        assert_eq!(image.get_pixel(0, 0).0, [10, 10, 10, 255]);
        assert_eq!(image.get_pixel(3, 1).0, [80, 80, 80, 255]);
        let error = decode_image(&bytes, Some("image/png"), &mut budget)
            .unwrap_err()
            .to_string();
        assert!(error.contains("budget"), "{error}");

        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(&[45, 70], 1, 1, image::ExtendedColorType::La8)
            .unwrap();
        let image = decode_image(&bytes, Some("image/png"), &mut Budget::new())
            .unwrap()
            .into_rgba8();
        assert_eq!(image.get_pixel(0, 0).0, [45, 45, 45, 70]);
    }

    // Converting to RGBA8 must not silently down-convert previously refused
    // 16-bit content. The PNG's authored pixel type supplies the oracle.
    #[wasm_bindgen_test(unsupported = test)]
    fn sixteen_bit_png_pixels_remain_unsupported() {
        use image::ImageEncoder;
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(&[1, 2], 1, 1, image::ExtendedColorType::L16)
            .unwrap();
        let error = decode_image(&bytes, Some("image/png"), &mut Budget::new())
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported L16"), "{error}");
    }
}
