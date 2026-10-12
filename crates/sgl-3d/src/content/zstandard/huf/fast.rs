//! Branch-free Huffman decoding loop (the reference's "fast" loop, in safe Rust).
//! Each stream keeps a 64-bit window read backwards; its lowest set bit is a
//! sentinel whose position counts the bits consumed since the last reload, so a
//! reload is `ip -= consumed / 8` and a shift, with no end-of-stream branches.
//! It stops while every stream still has 7+ bytes left; the careful decoder in
//! `dtable` finishes the tails and checks exact consumption.
use super::dtable::HEntry;
use crate::content::zstandard::bits::RevReader;

/// Symbols decoded per stream between reloads: 7 stale bits + 5 * 11 < 64.
const PER_RELOAD: usize = 5;

pub struct Stream<'a> {
    src: &'a [u8],
    ip: usize,
    bits: u64,
}

#[inline(always)]
fn load(src: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&src[at..at + 8]);
    u64::from_le_bytes(a)
}

impl<'a> Stream<'a> {
    /// Opens a stream of at least 8 bytes whose last byte holds the end marker.
    pub fn new(src: &'a [u8]) -> Option<Stream<'a>> {
        let last = *src.last()?;
        if src.len() < 8 || last == 0 {
            return None;
        }
        let ip = src.len() - 8;
        Some(Stream {
            src,
            ip,
            bits: (load(src, ip) | 1) << (last.leading_zeros() + 1),
        })
    }

    #[inline(always)]
    fn reload(&mut self) {
        let n = self.bits.trailing_zeros();
        self.ip -= (n >> 3) as usize;
        self.bits = (load(self.src, self.ip) | 1) << (n & 7);
    }

    #[inline(always)]
    fn decode(&mut self, table: &[HEntry; 2048], shift: u32) -> u8 {
        let e = table[(self.bits >> shift) as usize & 2047];
        self.bits <<= e.nb;
        e.sym
    }

    /// True while a full round (decode, then reload) cannot run past the stream start.
    #[inline(always)]
    fn roomy(&self) -> bool {
        self.ip >= 7
    }

    /// Hands the position over to the careful bit reader.
    pub fn into_reader(self) -> RevReader<'a> {
        RevReader::resume(self.src, self.ip, self.bits.trailing_zeros())
    }
}

/// Decodes rounds of `PER_RELOAD` symbols from four streams into the four
/// outputs while every stream and output has room; returns symbols per output done.
pub fn four(
    table: &[HEntry; 2048],
    max_bits: u32,
    s: &mut [Stream<'_>; 4],
    out: [&mut [u8]; 4],
) -> usize {
    let shift = 64 - max_bits;
    let [o0, o1, o2, o3] = out;
    let mut done = 0;
    let rounds = o0
        .chunks_exact_mut(PER_RELOAD)
        .zip(o1.chunks_exact_mut(PER_RELOAD))
        .zip(o2.chunks_exact_mut(PER_RELOAD))
        .zip(o3.chunks_exact_mut(PER_RELOAD));
    for (((a, b), c), d) in rounds {
        if !s.iter().all(Stream::roomy) {
            break;
        }
        for k in 0..PER_RELOAD {
            a[k] = s[0].decode(table, shift);
            b[k] = s[1].decode(table, shift);
            c[k] = s[2].decode(table, shift);
            d[k] = s[3].decode(table, shift);
        }
        for st in s.iter_mut() {
            st.reload();
        }
        done += PER_RELOAD;
    }
    done
}

/// Single-stream version of [`four`].
pub fn one(table: &[HEntry; 2048], max_bits: u32, s: &mut Stream<'_>, out: &mut [u8]) -> usize {
    let shift = 64 - max_bits;
    let mut done = 0;
    for chunk in out.chunks_exact_mut(PER_RELOAD) {
        if !s.roomy() {
            break;
        }
        for b in chunk.iter_mut() {
            *b = s.decode(table, shift);
        }
        s.reload();
        done += PER_RELOAD;
    }
    done
}
