//! Material images: decoded RGBA8 texels, or a block-compressed mip chain as
//! a game's export step stores it, read from KTX2 as Bevy 9d12036 reads it
//! (`crates/bevy_image/src/ktx2.rs`, `ktx2_buffer_to_image`: its stored
//! levels, Zstandard supercompression, and a BC7 block format sampled as sRGB
//! or linear by the channel that uses it). MIT OR Apache-2.0
//! (src/LICENSE-bevy.txt).
use std::borrow::Cow;

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
/// multiples of the block size and no larger than 16384, and it holds one
/// to a full chain of levels, each of the size its level needs; otherwise
/// it refuses it with
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

/// ktx2 0.5.0 eagerly parses the DFD, incrementing dimension bytes in u8.
/// Check its block bounds and unsupported 256-texel dimensions before that
/// parser. KTX 2.0 section 3.10 / Khronos Data Format 1.4 define this layout.
fn check_ktx2_dfd(bytes: &[u8]) -> Result<()> {
    let raw = bytes
        .get(..ktx2::Header::LENGTH)
        .ok_or("invalid KTX2 file: truncated header")?;
    let header = ktx2::Header::from_bytes(raw.try_into()?)
        .map_err(|error| format!("invalid KTX2 file: {error}"))?;
    let index = header.index;
    let end = index
        .dfd_byte_offset
        .checked_add(index.dfd_byte_length)
        .ok_or("invalid KTX2 data format descriptor: range overflow")?;
    let descriptor = bytes
        .get(index.dfd_byte_offset as usize..end as usize)
        .ok_or("invalid KTX2 data format descriptor: truncated section")?;
    let mut blocks = descriptor
        .get(4..)
        .ok_or("invalid KTX2 data format descriptor: truncated size")?;
    while !blocks.is_empty() {
        let header = blocks
            .get(..ktx2::dfd::BlockHeader::LENGTH)
            .ok_or("invalid KTX2 data format descriptor: truncated block header")?;
        let size = u16::from_le_bytes(header[6..8].try_into()?) as usize;
        if size < ktx2::dfd::BlockHeader::LENGTH {
            return Err(
                "invalid KTX2 data format descriptor: block shorter than its header".into(),
            );
        }
        let block = blocks
            .get(..size)
            .ok_or("invalid KTX2 data format descriptor: truncated block")?;
        // Match the same vendor/type/version as ktx2's basic-block parser;
        // other descriptor blocks have different layouts.
        if header[..6] == ktx2::dfd::BlockHeader::BASIC.as_bytes(0)[..6] {
            let dimensions = block
                .get(12..16)
                .ok_or("invalid KTX2 data format descriptor: truncated dimensions")?;
            if dimensions.contains(&0xff) {
                return Err(
                    "invalid KTX2 data format descriptor: unsupported texel-block dimension".into(),
                );
            }
        }
        blocks = &blocks[size..];
    }
    Ok(())
}

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
    /// Refuses malformed data format descriptors, sides above 16384,
    /// invalid block dimensions or mip counts,
    /// and levels whose declared or decoded size differs from their BC7
    /// size. The total decoded chain is budgeted before level allocation;
    /// the decoder bounds regenerated blocks before buffer growth.
    pub fn from_ktx2(bytes: &[u8]) -> Result<Self> {
        Self::read_ktx2(bytes, &mut Budget::new())
    }

    fn read_ktx2(bytes: &[u8], budget: &mut Budget) -> Result<Self> {
        check_ktx2_dfd(bytes)?;
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
                        // Unlike a streaming history buffer, this one-shot
                        // decoder bounds growth before literals/sequences are
                        // written and never allocates the advertised window.
                        let mut decoded = Vec::new();
                        crate::content::zstandard::decompress(
                            level.data,
                            expected + 1,
                            &mut decoded,
                        )
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
    /// block size (wgpu's rule for a block-compressed level 0) no larger
    /// than KTX2 loading allows, which also keeps `level_bytes` within a
    /// 32-bit `usize`, and one to a full chain of levels, each of its size.
    pub(crate) fn is_valid(&self) -> bool {
        let full = (32 - self.width.max(self.height).leading_zeros()) as usize;
        self.width.is_multiple_of(BLOCK)
            && self.height.is_multiple_of(BLOCK)
            && self.width <= MAX_IMAGE_SIDE
            && self.height <= MAX_IMAGE_SIDE
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

    // A DFD stores each block dimension minus one. 0xff makes ktx2's
    // u8 increment overflow; malformed bytes must fail the public loader.
    #[wasm_bindgen_test(unsupported = test)]
    fn ktx2_invalid_descriptor_dimensions_return_an_error() {
        let valid = ktx2(ktx2::Format::BC7_UNORM_BLOCK, [4; 2], &[vec![0; 16]], false);
        assert!(CompressedImage::from_ktx2(&valid).is_ok());
        let dfd = u32::from_le_bytes(valid[48..52].try_into().unwrap()) as usize;
        for axis in 0..4 {
            let mut file = valid.clone();
            file[dfd + 16 + axis] = 0xff;
            let error = CompressedImage::from_ktx2(&file).unwrap_err().to_string();
            assert!(error.contains("data format descriptor"), "{error}");
        }
    }

    // The BC7 format independently requires one 16-byte block for 4x4.
    // A 64 KiB Zstandard payload lies about its decoded size. Its own frame
    // header declares that expansion, so it must fail at the decoder cap
    // before any oversized block or output is allocated.
    #[wasm_bindgen_test(unsupported = test)]
    fn ktx2_zstandard_output_is_bounded_by_the_expected_size() {
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
        assert!(error.contains("level 0: output limit exceeded"), "{error}");
        let short = ktx2(ktx2::Format::BC7_UNORM_BLOCK, [4; 2], &[vec![0; 15]], true);
        let mut short = short;
        short[96..104].copy_from_slice(&16u64.to_le_bytes());
        let error = CompressedImage::from_ktx2(&short).unwrap_err().to_string();
        assert!(
            error.contains("decoded to 15 bytes; expected 16"),
            "{error}"
        );
    }

    // A 1 GiB advertised window must not force a history allocation before
    // a 16-byte level can refuse a 128 KiB RLE block. The whole source is
    // ten bytes; RFC 8878's window/block headers independently define it.
    #[wasm_bindgen_test(unsupported = test)]
    fn ktx2_large_zstandard_window_does_not_allocate_before_output_refusal() {
        let frame = [0x28, 0xb5, 0x2f, 0xfd, 0x00, 0xa0, 0x03, 0x00, 0x10, 42];
        let file = with_frame([4; 2], 16, &frame);
        let error = CompressedImage::from_ktx2(&file).unwrap_err().to_string();
        assert!(
            error.contains("level 0") && error.contains("output limit exceeded"),
            "{error}"
        );
        // Observe the API's actual output allocation as well as its error:
        // rejecting after regenerating then truncating this block would keep
        // the oversized capacity and fail this assertion.
        let mut output = Vec::new();
        assert_eq!(
            crate::content::zstandard::decompress(&frame, 17, &mut output),
            Err(crate::content::zstandard::Error::OutputLimit)
        );
        assert_eq!(output.capacity(), 0);
    }

    // A compressed block encodes two one-byte literals and two matches of
    // 131074 bytes each (ML code52 +16 extra bits). Its 262150 regenerated
    // bytes exceed RFC 8878's 128 KiB block maximum even though the level's
    // own 262144-byte budget is large enough for either first sequence.
    // Checking only the frame window or collecting output cannot enforce it.
    #[wasm_bindgen_test(unsupported = test)]
    fn ktx2_malformed_sequence_expansion_is_refused_before_output() {
        let frame = [
            0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x38, // 128 KiB window, no size
            0x6d, 0x00, 0x00, // Last compressed block: 13 stored bytes
            0x10, b'A', b'A', // Two raw literals
            2, 0x54, 1, 0, 52, // Two sequences; RLE LL=1, OF=0, ML=52
            0xff, 0xff, 0xff, 0xff, 1, // Two 16-bit extras + end marker
        ];
        let file = with_frame([512; 2], 262144, &frame);
        let error = CompressedImage::from_ktx2(&file).unwrap_err().to_string();
        assert!(error.contains("Block_Maximum_Size"), "{error}");
        let mut output = Vec::new();
        assert!(matches!(
            crate::content::zstandard::decompress(&frame, 262144, &mut output),
            Err(crate::content::zstandard::Error::Corrupt(
                "block regenerates more than Block_Maximum_Size"
            ))
        ));
        assert_eq!(output.capacity(), 0);
    }

    // RLE literals have their own scratch buffer: their regenerated header
    // must be checked against the block bound before allocating that buffer.
    #[wasm_bindgen_test(unsupported = test)]
    fn ktx2_malformed_literal_expansion_is_refused_before_scratch_growth() {
        let frame = [
            0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x38, 0x2d, 0,
            0, // Last compressed block: 5 stored bytes
            0xfd, 0xff, 0x3f, b'A', 0, // RLE literals: 262143 bytes; no sequences
        ];
        let file = with_frame([512; 2], 262144, &frame);
        let error = CompressedImage::from_ktx2(&file).unwrap_err().to_string();
        assert!(error.contains("literals larger than a block"), "{error}");
    }

    fn with_frame(size: [u32; 2], expected: u64, frame: &[u8]) -> Vec<u8> {
        let mut file = ktx2(ktx2::Format::BC7_UNORM_BLOCK, size, &[vec![0; 16]], true);
        let at = u64::from_le_bytes(file[80..88].try_into().unwrap()) as usize;
        file[88..96].copy_from_slice(&(frame.len() as u64).to_le_bytes());
        file[96..104].copy_from_slice(&expected.to_le_bytes());
        file.truncate(at);
        file.extend_from_slice(frame);
        file
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

    // A game-built image skips `from_ktx2`'s side bound. On wasm32 the 4 GiB
    // level 0 of a 65536-texel square passes `usize`, so sizing it to check
    // its level overflowed (a panic) instead of refusing the image.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_game_built_image_past_the_side_bound_is_invalid() {
        let image = CompressedImage {
            format: CompressedFormat::Bc7,
            width: 65536,
            height: 65536,
            levels: vec![vec![0; 16]],
        };
        assert!(!image.is_valid());
    }
}
