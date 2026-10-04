//! Material images: decoded RGBA8 texels, or a block-compressed mip chain as
//! a game's export step stores it, read from KTX2 as Bevy 9d12036 reads it
//! (`crates/bevy_image/src/ktx2.rs`, `ktx2_buffer_to_image`: its stored
//! levels, Zstandard supercompression, and a BC7 block format sampled as sRGB
//! or linear by the channel that uses it). MIT OR Apache-2.0
//! (src/LICENSE-bevy.txt).
use std::borrow::Cow;
use std::io::Read;

use super::asset::Result;

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
    /// the ray source holds.
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
    pub fn from_ktx2(bytes: &[u8]) -> Result<Self> {
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
        let levels = reader
            .levels()
            .enumerate()
            .map(|(index, level)| match header.supercompression_scheme {
                None => Ok(level.data.to_vec()),
                Some(ktx2::SupercompressionScheme::Zstandard) => {
                    let mut decoded = Vec::new();
                    ruzstd::decoding::StreamingDecoder::new(level.data)
                        .map_err(|error| error.to_string())
                        .and_then(|mut decoder| {
                            decoder
                                .read_to_end(&mut decoded)
                                .map_err(|error| error.to_string())
                        })
                        .map_err(|error| format!("KTX2 level {index}: {error}"))?;
                    Ok(decoded)
                }
                Some(other) => Err(format!(
                    "KTX2 supercompression {other:?} is not supported; use Zstandard or none"
                )
                .into()),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            format,
            width: header.pixel_width,
            height: header.pixel_height.max(1),
            levels,
        })
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
