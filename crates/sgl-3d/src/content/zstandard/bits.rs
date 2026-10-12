//! Bit streams. zstd entropy streams are written forwards (LSB first) and read
//! backwards starting from the final byte, whose highest set bit is a marker.
use crate::content::zstandard::error::{Error, Result};

/// Outcome of a refill, mirroring the reference decoder's states.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refill {
    Unfinished,
    EndOfBuffer,
    Completed,
    Overflow,
}

/// Backward bit reader. Bits past the start of the stream read as zero; callers
/// that must not rely on that check `finished()` / `overflowed()`.
pub struct RevReader<'a> {
    src: &'a [u8],
    ptr: usize,
    container: u64,
    consumed: u32,
}

impl<'a> RevReader<'a> {
    pub fn new(src: &'a [u8]) -> Result<Self> {
        let last = *src.last().ok_or(Error::Corrupt("empty bitstream"))?;
        if last == 0 {
            return Err(Error::Corrupt("bitstream missing end marker"));
        }
        let skip = last.leading_zeros() + 1;
        if src.len() >= 8 {
            let ptr = src.len() - 8;
            Ok(RevReader {
                src,
                ptr,
                container: load(src, ptr),
                consumed: skip,
            })
        } else {
            let mut a = [0u8; 8];
            a[..src.len()].copy_from_slice(src);
            let pad = (8 - src.len() as u32) * 8;
            Ok(RevReader {
                src,
                ptr: 0,
                container: u64::from_le_bytes(a),
                consumed: skip + pad,
            })
        }
    }

    /// Resumes reading at byte window `ptr` with `consumed` bits of it used
    /// (the state a fast loop hands over; requires `ptr + 8 <= src.len()`).
    pub fn resume(src: &'a [u8], ptr: usize, consumed: u32) -> Self {
        RevReader {
            src,
            ptr,
            container: load(src, ptr),
            consumed,
        }
    }

    /// Reads `nb` bits (0..=57 after a refill); past-the-start bits are zero.
    #[inline(always)]
    pub fn read(&mut self, nb: u32) -> u64 {
        let v = self.peek(nb);
        self.consumed += nb;
        v
    }

    /// Returns the next `nb` bits without consuming them.
    #[inline(always)]
    pub fn peek(&self, nb: u32) -> u64 {
        let shifted = if self.consumed < 64 {
            self.container << self.consumed
        } else {
            0
        };
        (shifted >> 1) >> (63 - nb)
    }

    #[inline(always)]
    pub fn skip(&mut self, nb: u32) {
        self.consumed += nb;
    }

    /// Reloads the container so at least 57 bits are available when possible.
    #[inline(always)]
    pub fn refill(&mut self) -> Refill {
        if self.consumed > 64 {
            return Refill::Overflow;
        }
        let want = (self.consumed >> 3) as usize;
        if self.ptr >= 8 {
            self.ptr -= want;
            self.consumed &= 7;
            self.container = load(self.src, self.ptr);
            return Refill::Unfinished;
        }
        if self.ptr == 0 {
            return if self.consumed == 64 {
                Refill::Completed
            } else {
                Refill::EndOfBuffer
            };
        }
        let step = want.min(self.ptr);
        self.ptr -= step;
        self.consumed -= (step as u32) * 8;
        self.container = load(self.src, self.ptr);
        Refill::EndOfBuffer
    }

    /// True when every bit of the stream was consumed exactly.
    pub fn finished(&self) -> bool {
        self.ptr == 0 && self.consumed == 64
    }
}

#[inline(always)]
fn load(src: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&src[at..at + 8]);
    u64::from_le_bytes(a)
}
