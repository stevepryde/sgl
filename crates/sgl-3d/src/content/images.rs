//! Material images: decoded RGBA8 texels, or a block-compressed mip chain as
//! a game's export step stores it, read from KTX2 as Bevy 9d12036 reads it
//! (`crates/bevy_image/src/ktx2.rs`, `ktx2_buffer_to_image`: its stored
//! levels, Zstandard supercompression, and a BC7 block format sampled as sRGB
//! or linear by the channel that uses it). MIT OR Apache-2.0
//! (src/LICENSE-bevy.txt).
use std::borrow::Cow;
use std::io::Read;

use super::asset::Result;
use super::load_budget::{Budget, MAX_IMAGE_SIDE};

/// An image materials sample. Its colour space belongs to each channel that
/// samples it: base and emissive maps read it as sRGB, the others as linear
/// data.
#[derive(Clone, Debug)]
pub enum Image {
    /// Decoded texels. The scene filters their mip chain when it adds them,
    /// in linear light for a channel that samples them as colour.
    Rgba8(image::RgbaImage),
    /// A block-compressed mip chain, uploaded as stored. Adding it needs
    /// `wgpu::Features::TEXTURE_COMPRESSION_BC`.
    Compressed(CompressedImage),
}

/// A block-compressed image's encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressedFormat {
    /// BC7: 16 bytes per block of 4x4 RGBA texels.
    Bc7,
}

/// A block-compressed mip chain. A scene adds it when its sides are
/// multiples of the block size and it holds one to a full chain of levels,
/// each of the size its level needs; otherwise it refuses it with
/// `SceneError::InvalidCompressedImage`.
#[derive(Clone, Debug)]
pub struct CompressedImage {
    pub format: CompressedFormat,
    /// Level 0's width in texels.
    pub width: u32,
    /// Level 0's height in texels.
    pub height: u32,
    /// The stored levels, level 0 first and each next one half the size
    /// (at least one texel): each one's blocks row by row, a level's edge
    /// blocks whole. Filter a colour image's levels in linear light.
    pub levels: Vec<Vec<u8>>,
}

/// Texels on each side of a block.
const BLOCK: u32 = 4;
/// Bytes of a BC7 block.
const BLOCK_BYTES: usize = 16;

impl Image {
    /// Level 0's width and height in texels.
    pub(crate) fn size(&self) -> [u32; 2] {
        match self {
            Self::Rgba8(image) => [image.width(), image.height()],
            Self::Compressed(image) => [image.width, image.height],
        }
    }

    /// Level 0's texels, decoded from its blocks when it is compressed: what
    /// the decal atlas packs.
    pub(crate) fn texels(&self) -> Cow<'_, image::RgbaImage> {
        match self {
            Self::Rgba8(image) => Cow::Borrowed(image),
            Self::Compressed(image) => Cow::Owned(image.decode_level_0()),
        }
    }
}

impl CompressedImage {
    /// The image a KTX2 file holds: one 2D image of BC7 blocks, either
    /// `VK_FORMAT_BC7_UNORM_BLOCK` or `VK_FORMAT_BC7_SRGB_BLOCK` (the channel
    /// that samples it picks sRGB or linear, as Bevy's loader does), with
    /// its stored levels, uncompressed or supercompressed with Zstandard.
    /// Refuses sides above 16384, invalid block dimensions or mip counts,
    /// and levels whose declared or decoded size differs from their BC7
    /// size. The total decoded chain is budgeted before level allocation.
    pub fn from_ktx2(bytes: &[u8]) -> Result<Self> {
        Self::read_ktx2(bytes, &mut Budget::new())
    }

    fn read_ktx2(bytes: &[u8], budget: &mut Budget) -> Result<Self> {
        let reader =
            ktx2::Reader::new(bytes).map_err(|error| format!("invalid KTX2 file: {error}"))?;
        let header = reader.header();
        let format = match header.format {
            Some(ktx2::Format::BC7_UNORM_BLOCK | ktx2::Format::BC7_SRGB_BLOCK) => {
                CompressedFormat::Bc7
            }
            other => {
                return Err(format!("KTX2 format {other:?} is not supported; store BC7").into());
            }
        };
        if header.pixel_depth > 1 || header.layer_count > 1 || header.face_count != 1 {
            return Err("a KTX2 material image must be one 2D image".into());
        }
        if header.level_count == 0 {
            return Err("the KTX2 file asks for generated mips; store its mip chain".into());
        }
        let mut image = Self {
            format,
            width: header.pixel_width,
            height: header.pixel_height.max(1),
            levels: Vec::new(),
        };
        let full = 32 - image.width.max(image.height).leading_zeros();
        if image.width == 0
            || image.width > MAX_IMAGE_SIDE
            || image.height > MAX_IMAGE_SIDE
            || !image.width.is_multiple_of(BLOCK)
            || !image.height.is_multiple_of(BLOCK)
            || header.level_count > full
        {
            return Err(format!("invalid KTX2 dimensions or mip count; store block-aligned sides no larger than {MAX_IMAGE_SIDE} and at most a full mip chain").into());
        }
        // Validate every declared level and the whole chain before allocating
        // any decoded level. Header bounds above also make level_bytes safe.
        for (index, level) in reader.levels().enumerate() {
            let expected = image.level_bytes(index);
            if level.uncompressed_byte_length != expected as u64 {
                return Err(format!(
                    "KTX2 level {index} declares {} decoded bytes; expected {expected}",
                    level.uncompressed_byte_length
                )
                .into());
            }
            budget.charge(expected as u64)?;
        }
        image.levels = reader
            .levels()
            .enumerate()
            .map(|(index, level)| {
                let expected = image.level_bytes(index);
                let decoded = match header.supercompression_scheme {
                    None => {
                        if level.data.len() != expected {
                            return Err(format!(
                                "KTX2 level {index} has {} bytes; expected {expected}",
                                level.data.len()
                            )
                            .into());
                        }
                        level.data.to_vec()
                    }
                    Some(ktx2::SupercompressionScheme::Zstandard) => {
                        // A single sentinel detects excess output without
                        // collecting it or doubling the vector's capacity.
                        let mut decoded = Vec::with_capacity(expected + 1);
                        ruzstd::decoding::StreamingDecoder::new(level.data)
                            .map_err(|error| error.to_string())
                            .and_then(|decoder| {
                                decoder
                                    .take(expected as u64 + 1)
                                    .read_to_end(&mut decoded)
                                    .map_err(|error| error.to_string())
                            })
                            .map_err(|error| format!("KTX2 level {index}: {error}"))?;
                        decoded
                    }
                    Some(other) => return Err(format!(
                        "KTX2 supercompression {other:?} is not supported; use Zstandard or none"
                    )
                    .into()),
                };
                if decoded.len() != expected {
                    return Err(format!(
                        "KTX2 level {index} decoded to {} bytes; expected {expected}",
                        decoded.len()
                    )
                    .into());
                }
                Ok(decoded)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(image)
    }

    /// The bytes level `level` holds: its blocks, a partial edge block whole.
    pub(crate) fn level_bytes(&self, level: usize) -> usize {
        let side = |size: u32| (size >> level).max(1).div_ceil(BLOCK) as usize;
        side(self.width) * side(self.height) * BLOCK_BYTES
    }

    /// Whether the scene can upload it: sides that are multiples of the
    /// block size (wgpu's rule for a block-compressed level 0), and one to a
    /// full chain of levels, each of its size.
    pub(crate) fn is_valid(&self) -> bool {
        let full = (32 - self.width.max(self.height).leading_zeros()) as usize;
        self.width.is_multiple_of(BLOCK)
            && self.height.is_multiple_of(BLOCK)
            && (1..=full).contains(&self.levels.len())
            && self
                .levels
                .iter()
                .enumerate()
                .all(|(level, bytes)| bytes.len() == self.level_bytes(level))
    }

    /// Level 0's texels, decoded with bcdec (MIT) as the GPU decodes them.
    /// A valid image only.
    fn decode_level_0(&self) -> image::RgbaImage {
        let mut texels = image::RgbaImage::new(self.width, self.height);
        let pitch = self.width as usize * 4;
        let blocks_wide = (self.width / BLOCK) as usize;
        let rows = texels.chunks_exact_mut(pitch * BLOCK as usize);
        for (blocks, row) in self.levels[0]
            .chunks_exact(blocks_wide * BLOCK_BYTES)
            .zip(rows)
        {
            for (column, block) in blocks.chunks_exact(BLOCK_BYTES).enumerate() {
                let at = column * BLOCK as usize * 4;
                match self.format {
                    CompressedFormat::Bc7 => bcdec_rs::bc7(block, &mut row[at..], pitch),
                }
            }
        }
        texels
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
#[allow(dead_code)]
#[path = "../../examples/support/ktx2_writer.rs"]
mod ktx2_writer;

#[cfg(test)]
mod tests {
    #[cfg(target_arch = "wasm32")]
    use super::ktx2_writer::ktx2;
    use super::*;
    #[cfg(not(target_arch = "wasm32"))]
    use crate::test_support::ktx2;
    use wasm_bindgen_test::wasm_bindgen_test;

    // The BC7 format independently requires one 16-byte block for 4x4.
    // A 64 KiB Zstandard payload lies about its decoded size. The error's
    // byte count exposes an unbounded read even if rejection happens later.
    #[wasm_bindgen_test(unsupported = test)]
    fn ktx2_zstandard_output_stops_after_the_expected_size_and_sentinel() {
        let mut file = ktx2(
            ktx2::Format::BC7_UNORM_BLOCK,
            [4; 2],
            &[vec![0; 65536]],
            true,
        );
        // KTX2 level index uncompressedByteLength, independently laid out
        // in KTX 2.0 section 3.9: 80-byte header, then three u64 fields.
        file[96..104].copy_from_slice(&16u64.to_le_bytes());
        let error = CompressedImage::from_ktx2(&file).unwrap_err().to_string();
        assert!(
            error.contains("level 0 decoded to 17 bytes; expected 16"),
            "{error}"
        );
        let short = ktx2(ktx2::Format::BC7_UNORM_BLOCK, [4; 2], &[vec![0; 15]], true);
        let mut short = short;
        short[96..104].copy_from_slice(&16u64.to_le_bytes());
        let error = CompressedImage::from_ktx2(&short).unwrap_err().to_string();
        assert!(
            error.contains("decoded to 15 bytes; expected 16"),
            "{error}"
        );
    }

    // A 4x4 BC7 chain with two levels holds 32 bytes. A budget of 24 must
    // reject the whole chain before it notices deliberately broken level 0.
    #[wasm_bindgen_test(unsupported = test)]
    fn ktx2_charges_the_whole_chain_before_decompressing() {
        let mut file = ktx2(
            ktx2::Format::BC7_UNORM_BLOCK,
            [4; 2],
            &[vec![0; 16], vec![0; 16]],
            true,
        );
        let at = u64::from_le_bytes(file[80..88].try_into().unwrap()) as usize;
        file[at] = 0; // Break the Zstandard magic.
        let error = CompressedImage::read_ktx2(&file, &mut Budget { remaining: 24 })
            .unwrap_err()
            .to_string();
        assert!(error.contains("budget"), "{error}");
    }

    // The loader must reject dimensions/mips before it decodes bad payloads,
    // including oversized sides whose expected sizes could overflow wasm32.
    #[wasm_bindgen_test(unsupported = test)]
    fn ktx2_rejects_invalid_shapes_and_level_sizes_during_load() {
        for size in [[3, 4], [20000, 20000], [u32::MAX; 2]] {
            let file = ktx2(ktx2::Format::BC7_UNORM_BLOCK, size, &[vec![0; 16]], true);
            let error = CompressedImage::from_ktx2(&file).unwrap_err().to_string();
            assert!(error.contains("dimensions"), "{error}");
        }
        let file = ktx2(
            ktx2::Format::BC7_UNORM_BLOCK,
            [4; 2],
            &vec![vec![0; 16]; 4],
            false,
        );
        let error = CompressedImage::from_ktx2(&file).unwrap_err().to_string();
        assert!(error.contains("mip count"), "{error}");
        for zstd in [false, true] {
            let file = ktx2(ktx2::Format::BC7_UNORM_BLOCK, [4; 2], &[vec![0; 15]], zstd);
            let error = CompressedImage::from_ktx2(&file).unwrap_err().to_string();
            assert!(
                error.contains("declares 15 decoded bytes; expected 16"),
                "{error}"
            );
        }
    }
}
