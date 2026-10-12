//! Huffman decoding: single-symbol lookup table and 1-/4-stream decoding.
use super::HufWeights;
use super::fast::{self, Stream};
use crate::content::zstandard::bits::RevReader;
use crate::content::zstandard::error::{Error, Result};
use std::boxed::Box;

/// Opens one Huffman stream (a distinct error, so callers can tell it apart).
fn stream(src: &[u8]) -> Result<RevReader<'_>> {
    RevReader::new(src).map_err(|_| Error::Corrupt("Huffman stream missing end marker"))
}

#[derive(Clone, Copy, Default, Debug)]
pub struct HEntry {
    pub sym: u8,
    pub nb: u8,
}

/// Entries in the largest table (11-bit codes); fixed so lookups need no bounds check.
const TABLE: usize = 1 << super::MAX_BITS;

/// Decoding table indexed by the next `max_bits` bits of the stream.
#[derive(Clone, Debug)]
pub struct HufDTable {
    pub entries: Box<[HEntry; TABLE]>,
    pub max_bits: u32,
}

impl HufDTable {
    pub fn new(hw: &HufWeights) -> HufDTable {
        let mut t = HufDTable {
            entries: Box::new([HEntry::default(); TABLE]),
            max_bits: 0,
        };
        t.rebuild(hw);
        t
    }

    /// Rebuilds the table in place, reusing its allocation.
    pub fn rebuild(&mut self, hw: &HufWeights) {
        let mb = hw.max_bits;
        let mut start = [0usize; 13];
        let mut rank = [0usize; 13];
        for &w in &hw.w[..hw.symbols] {
            rank[w as usize] += 1;
        }
        let mut next = 0usize;
        for w in 1..=mb as usize {
            start[w] = next;
            next += rank[w] << (w - 1);
        }
        let entries = &mut self.entries[..];
        for s in 0..hw.symbols {
            let w = hw.w[s] as usize;
            if w == 0 {
                continue;
            }
            let len = 1usize << (w - 1);
            let e = HEntry {
                sym: s as u8,
                nb: (mb + 1 - w as u32) as u8,
            };
            entries[start[w]..start[w] + len].fill(e);
            start[w] += len;
        }
        self.max_bits = mb;
    }

    #[inline(always)]
    fn step(&self, r: &mut RevReader<'_>) -> u8 {
        let e = self.entries[r.peek(self.max_bits) as usize & (TABLE - 1)];
        r.skip(e.nb as u32);
        e.sym
    }

    /// Decodes exactly `out.len()` symbols from one stream that must be consumed exactly.
    pub fn decode_1x(&self, src: &[u8], out: &mut [u8]) -> Result<()> {
        let (mut r, done) = match Stream::new(src) {
            Some(mut s) => {
                let done = fast::one(&self.entries, self.max_bits, &mut s, out);
                (s.into_reader(), done)
            }
            None => (stream(src)?, 0),
        };
        let out = &mut out[done..];
        let mut chunks = out.chunks_exact_mut(4);
        for c in &mut chunks {
            r.refill();
            c[0] = self.step(&mut r);
            c[1] = self.step(&mut r);
            c[2] = self.step(&mut r);
            c[3] = self.step(&mut r);
        }
        for b in chunks.into_remainder() {
            r.refill();
            *b = self.step(&mut r);
        }
        r.refill();
        if !r.finished() {
            return Err(Error::Corrupt("Huffman stream not consumed exactly"));
        }
        Ok(())
    }

    /// Decodes four streams preceded by a 6-byte jump table.
    pub fn decode_4x(&self, src: &[u8], out: &mut [u8]) -> Result<()> {
        if src.len() < 10 || out.len() < 6 {
            return Err(Error::Corrupt("4-stream literals too short"));
        }
        let s1 = u16::from_le_bytes([src[0], src[1]]) as usize;
        let s2 = u16::from_le_bytes([src[2], src[3]]) as usize;
        let s3 = u16::from_le_bytes([src[4], src[5]]) as usize;
        let body = &src[6..];
        if s1 + s2 + s3 >= body.len() {
            return Err(Error::Corrupt("literal jump table out of range"));
        }
        let (a, rest) = body.split_at(s1);
        let (b, rest) = rest.split_at(s2);
        let (c, d) = rest.split_at(s3);
        let seg = out.len().div_ceil(4);
        if seg * 3 > out.len() {
            return Err(Error::Corrupt("literal segments exceed size"));
        }
        let (o1, rest) = out.split_at_mut(seg);
        let (o2, rest) = rest.split_at_mut(seg);
        let (o3, o4) = rest.split_at_mut(seg);
        self.decode_interleaved([a, b, c, d], [o1, o2, o3, o4])
    }

    fn decode_interleaved(&self, src: [&[u8]; 4], out: [&mut [u8]; 4]) -> Result<()> {
        let [o1, o2, o3, o4] = out;
        let (mut r, mut i) = match (
            Stream::new(src[0]),
            Stream::new(src[1]),
            Stream::new(src[2]),
            Stream::new(src[3]),
        ) {
            (Some(a), Some(b), Some(c), Some(d)) => {
                let mut s = [a, b, c, d];
                let done = fast::four(
                    &self.entries,
                    self.max_bits,
                    &mut s,
                    [&mut *o1, &mut *o2, &mut *o3, &mut *o4],
                );
                (s.map(Stream::into_reader), done)
            }
            _ => (
                [
                    stream(src[0])?,
                    stream(src[1])?,
                    stream(src[2])?,
                    stream(src[3])?,
                ],
                0,
            ),
        };
        let common = o4.len().min(o1.len());
        while i + 4 <= common {
            for rr in r.iter_mut() {
                rr.refill();
            }
            for k in 0..4 {
                o1[i + k] = self.step(&mut r[0]);
                o2[i + k] = self.step(&mut r[1]);
                o3[i + k] = self.step(&mut r[2]);
                o4[i + k] = self.step(&mut r[3]);
            }
            i += 4;
        }
        for (rr, o) in r.iter_mut().zip([o1, o2, o3, o4]) {
            for b in &mut o[i..] {
                rr.refill();
                *b = self.step(rr);
            }
            rr.refill();
            if !rr.finished() {
                return Err(Error::Corrupt("Huffman stream not consumed exactly"));
            }
        }
        Ok(())
    }
}
