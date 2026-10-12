//! Decompression: frames, blocks, and the public `Decompressor`.
mod execute;
mod literals;
mod seqtable;
mod sequences;
mod state;
pub mod tables;

use crate::content::zstandard::dict::Dictionary;
use crate::content::zstandard::error::{Error, Result};
use crate::content::zstandard::frame::{
    BLOCK_MAX, BlockType, FrameFormat, FrameHeader, SKIPPABLE_BASE, SKIPPABLE_MASK, WINDOW_LOG_MAX,
    parse_block_header,
};
use crate::content::zstandard::huf::dtable::HufDTable;
use crate::content::zstandard::xxh64::xxh64;
use execute::Exec;
use seqtable::SeqTable;
use state::{DecState, Predefined};
use std::vec::Vec;

/// A dictionary prepared for decoding (tables built once, shared by any number of frames).
pub struct DecoderDictionary {
    pub(crate) dict: Dictionary,
    pub(crate) huf: Option<HufDTable>,
    pub(crate) seq: Option<[SeqTable; 3]>,
}

/// Reusable decompression context. Holds no configuration: every call states its limit.
pub struct Decompressor {
    st: DecState,
    pre: Predefined,
}

impl Decompressor {
    /// Creates a decompression context (its predefined tables are built here).
    pub fn new() -> Decompressor {
        let pre = match Predefined::new() {
            Ok(p) => p,
            Err(_) => unreachable!("predefined distributions are valid constants"),
        };
        Decompressor {
            st: DecState::new(),
            pre,
        }
    }

    /// Decodes every frame in `src` (skipping skippable frames), appending to `out`.
    /// Fails with `OutputLimit` before `out` would grow past `max_output` bytes in total.
    /// Returns the number of bytes appended. Like the reference, empty input is zero frames.
    pub fn decompress(
        &mut self,
        src: &[u8],
        dict: Option<&DecoderDictionary>,
        max_output: usize,
        out: &mut Vec<u8>,
    ) -> Result<usize> {
        self.decompress_format(FrameFormat::Standard, src, dict, max_output, out)
    }

    /// Like [`Decompressor::decompress`] for frames in `format` (magicless frames
    /// are concatenated without magic numbers and cannot include skippable frames).
    pub fn decompress_format(
        &mut self,
        format: FrameFormat,
        src: &[u8],
        dict: Option<&DecoderDictionary>,
        max_output: usize,
        out: &mut Vec<u8>,
    ) -> Result<usize> {
        let before = out.len();
        let limit = before
            .checked_add(max_output)
            .ok_or(Error::Parameter("output limit overflows"))?;
        let mut at = 0;
        while at < src.len() {
            at += self.frame(format, &src[at..], dict, limit, out)?;
        }
        Ok(out.len() - before)
    }

    fn frame(
        &mut self,
        format: FrameFormat,
        src: &[u8],
        dict: Option<&DecoderDictionary>,
        limit: usize,
        out: &mut Vec<u8>,
    ) -> Result<usize> {
        if format == FrameFormat::Magicless {
            let h = FrameHeader::parse_format(src, format)?;
            return self.frame_with(src, &h, dict, limit, out);
        }
        let magic = u32::from_le_bytes(
            src.get(..4)
                .ok_or(Error::Truncated)?
                .try_into()
                .map_err(|_| Error::Truncated)?,
        );
        if magic & SKIPPABLE_MASK == SKIPPABLE_BASE {
            let len = u32::from_le_bytes(
                src.get(4..8)
                    .ok_or(Error::Truncated)?
                    .try_into()
                    .map_err(|_| Error::Truncated)?,
            );
            let end = 8usize.checked_add(len as usize).ok_or(Error::Truncated)?;
            return if end <= src.len() {
                Ok(end)
            } else {
                Err(Error::Truncated)
            };
        }
        let h = FrameHeader::parse(src)?;
        self.frame_with(src, &h, dict, limit, out)
    }

    /// Decodes the frame whose header `h` was parsed from the start of `src`.
    fn frame_with(
        &mut self,
        src: &[u8],
        h: &FrameHeader,
        dict: Option<&DecoderDictionary>,
        limit: usize,
        out: &mut Vec<u8>,
    ) -> Result<usize> {
        if h.window_size > 1u64 << WINDOW_LOG_MAX {
            return Err(Error::WindowTooLarge);
        }
        let dict = match (h.dict_id, dict) {
            (0, d) => d,
            (id, Some(d)) if d.dict.id == id => Some(d),
            (_, Some(_)) => return Err(Error::WrongDictionary),
            (_, None) => return Err(Error::MissingDictionary),
        };
        if let Some(cs) = h.content_size {
            let room = limit.saturating_sub(out.len()) as u64;
            if cs > room {
                return Err(Error::OutputLimit);
            }
            out.reserve(cs as usize);
        }
        self.st.reset(dict);
        let frame_start = out.len();
        if let Some(cs) = h.content_size {
            out.resize(frame_start + cs as usize + execute::SLACK, 0);
        }
        let mut pos = frame_start;
        let body = self.frame_blocks(src, h, dict, limit, out, &mut pos);
        out.truncate(pos);
        let mut at = body?;
        let produced = (pos - frame_start) as u64;
        if h.content_size.is_some_and(|cs| cs != produced) {
            return Err(Error::ContentSize);
        }
        if h.checksum {
            let want = src.get(at..at + 4).ok_or(Error::Truncated)?;
            let got = (xxh64(&out[frame_start..], 0) as u32).to_le_bytes();
            if want != got {
                return Err(Error::Checksum);
            }
            at += 4;
        }
        Ok(at)
    }

    /// Decodes the blocks of a frame into `out` at `*pos` (advanced as output is
    /// produced, also on error); returns the offset just past the last block.
    fn frame_blocks(
        &mut self,
        src: &[u8],
        h: &FrameHeader,
        dict: Option<&DecoderDictionary>,
        limit: usize,
        out: &mut Vec<u8>,
        pos: &mut usize,
    ) -> Result<usize> {
        let frame_start = *pos;
        let block_max = (h.window_size.min(BLOCK_MAX as u64)) as usize;
        let mut at = h.header_len;
        loop {
            let (last, ty, size) = parse_block_header(src.get(at..).ok_or(Error::Truncated)?)?;
            at += 3;
            if size > block_max {
                return Err(Error::Corrupt("block larger than the block size limit"));
            }
            match ty {
                BlockType::Rle => {
                    let b = *src.get(at).ok_or(Error::Truncated)?;
                    make_room(out, *pos, size, limit)?;
                    out[*pos..*pos + size].fill(b);
                    *pos += size;
                    at += 1;
                }
                BlockType::Raw => {
                    let body = src.get(at..at + size).ok_or(Error::Truncated)?;
                    make_room(out, *pos, size, limit)?;
                    out[*pos..*pos + size].copy_from_slice(body);
                    *pos += size;
                    at += size;
                }
                BlockType::Compressed => {
                    let body = src.get(at..at + size).ok_or(Error::Truncated)?;
                    *pos = self.block(
                        body,
                        dict,
                        &Bounds {
                            frame_start,
                            limit,
                            block_max,
                        },
                        *pos,
                        out,
                    )?;
                    at += size;
                }
            }
            if last {
                return Ok(at);
            }
        }
    }

    /// Decodes one compressed block at `pos`; returns the new logical end.
    fn block(
        &mut self,
        body: &[u8],
        dict: Option<&DecoderDictionary>,
        b: &Bounds,
        pos: usize,
        out: &mut Vec<u8>,
    ) -> Result<usize> {
        let (lits, used) = literals::decode(body, &mut self.st, dict, b.block_max)?;
        let history: &[u8] = dict.map(|d| d.dict.content.as_slice()).unwrap_or(&[]);
        let mut ex = Exec::new(out, pos, b.frame_start, history, b.limit, b.block_max);
        let r = sequences::decode(body, used, lits, &mut self.st, &self.pre, dict, &mut ex);
        let end = ex.pos;
        r.map(|_| end)
    }
}

impl Default for Decompressor {
    fn default() -> Self {
        Self::new()
    }
}

/// Checks the output limit and makes `out[pos..pos + n]` writable.
fn make_room(out: &mut Vec<u8>, pos: usize, n: usize, limit: usize) -> Result<()> {
    let end = pos
        .checked_add(n)
        .filter(|&e| e <= limit)
        .ok_or(Error::OutputLimit)?;
    if out.len() < end {
        out.resize(end, 0);
    }
    Ok(())
}

/// Per-frame bounds every block is checked against.
struct Bounds {
    /// Index in the output where the frame starts.
    frame_start: usize,
    /// Caller's cap on the output length.
    limit: usize,
    /// Block_Maximum_Size: min(window, 128 KiB).
    block_max: usize,
}
