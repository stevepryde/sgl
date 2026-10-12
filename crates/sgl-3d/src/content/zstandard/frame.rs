//! Frame and block headers (RFC 8878 §3.1.1), shared by encoder and decoder.
use crate::content::zstandard::error::{Error, Result};

pub const MAGIC: u32 = 0xFD2F_B528;
pub const SKIPPABLE_MASK: u32 = 0xFFFF_FFF0;
pub const SKIPPABLE_BASE: u32 = 0x184D_2A50;
/// Largest block content (and regenerated size) the format allows.
pub const BLOCK_MAX: usize = 128 * 1024;
pub const WINDOW_LOG_MIN: u32 = 10;
pub const WINDOW_LOG_MAX: u32 = 31;

/// Frame layout: standard (4-byte magic number first) or the reference's
/// "magicless" variant (`ZSTD_f_zstd1_magicless`), which omits the magic number.
/// Magicless frames save 4 bytes per frame when both ends agree on the format out
/// of band; they cannot be mixed with skippable frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameFormat {
    /// RFC 8878 frames, starting with the magic number.
    Standard,
    /// Frames without the magic number.
    Magicless,
}

/// A parsed frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    /// Window size in bytes (equal to the content size for single-segment frames).
    pub window_size: u64,
    /// Declared decompressed size, if recorded.
    pub content_size: Option<u64>,
    /// 0 when the frame names no dictionary.
    pub dict_id: u32,
    /// A 4-byte XXH64-based checksum follows the last block.
    pub checksum: bool,
    /// No window descriptor: the window is the whole content.
    pub single_segment: bool,
    /// Bytes from the magic number through the end of the header.
    pub header_len: usize,
}

fn le(src: &[u8], at: usize, n: usize) -> Result<u64> {
    let b = src.get(at..at + n).ok_or(Error::Truncated)?;
    Ok(b.iter().rev().fold(0u64, |acc, &x| (acc << 8) | x as u64))
}

impl FrameHeader {
    /// Parses the header of a zstd frame starting at `src[0]` (the magic number).
    pub fn parse(src: &[u8]) -> Result<FrameHeader> {
        Self::parse_format(src, FrameFormat::Standard)
    }

    /// Parses a frame header in the given format.
    pub fn parse_format(src: &[u8], format: FrameFormat) -> Result<FrameHeader> {
        let start = match format {
            FrameFormat::Standard => {
                if le(src, 0, 4)? as u32 != MAGIC {
                    return Err(Error::BadMagic);
                }
                4
            }
            FrameFormat::Magicless => 0,
        };
        let fhd = *src.get(start).ok_or(Error::Truncated)?;
        let fcs_flag = fhd >> 6;
        let single_segment = fhd & 0x20 != 0;
        if fhd & 0x08 != 0 {
            return Err(Error::Reserved);
        }
        let checksum = fhd & 0x04 != 0;
        let did_len = [0usize, 1, 2, 4][(fhd & 3) as usize];
        let fcs_len = match fcs_flag {
            0 if single_segment => 1,
            0 => 0,
            1 => 2,
            2 => 4,
            _ => 8,
        };
        let mut at = start + 1;
        let mut window_size = 0u64;
        if !single_segment {
            let wd = *src.get(at).ok_or(Error::Truncated)?;
            let log = WINDOW_LOG_MIN + (wd >> 3) as u32;
            let base = 1u64 << log;
            window_size = base + (base / 8) * (wd & 7) as u64;
            at += 1;
        }
        let dict_id = le(src, at, did_len)? as u32;
        at += did_len;
        let content_size = match fcs_len {
            0 => None,
            2 => Some(le(src, at, 2)? + 256),
            n => Some(le(src, at, n)?),
        };
        at += fcs_len;
        if single_segment {
            window_size = content_size.ok_or(Error::Corrupt("single segment without size"))?;
        }
        Ok(FrameHeader {
            window_size,
            content_size,
            dict_id,
            checksum,
            single_segment,
            header_len: at,
        })
    }
}

/// Block types (RFC 8878 §3.1.1.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockType {
    Raw,
    Rle,
    Compressed,
}

/// Parses a 3-byte block header: (last, type, size).
pub fn parse_block_header(src: &[u8]) -> Result<(bool, BlockType, usize)> {
    let v = le(src, 0, 3)? as u32;
    let ty = match (v >> 1) & 3 {
        0 => BlockType::Raw,
        1 => BlockType::Rle,
        2 => BlockType::Compressed,
        _ => return Err(Error::Reserved),
    };
    Ok((v & 1 == 1, ty, (v >> 3) as usize))
}
