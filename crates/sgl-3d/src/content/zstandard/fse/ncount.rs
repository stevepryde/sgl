//! FSE table description ("NCount"): reading (RFC 8878 §4.1.1) and writing.
use super::{MAX_SYMBOLS, Norm};
use crate::content::zstandard::error::{Error, Result};

/// Forward little-endian bit cursor over a byte slice; bits past the end read as 0.
struct Fwd<'a> {
    src: &'a [u8],
    pos: usize,
}

impl Fwd<'_> {
    fn peek(&self, nb: u32) -> u32 {
        let byte = self.pos >> 3;
        let mut w = [0u8; 4];
        for (i, slot) in w.iter_mut().enumerate() {
            *slot = self.src.get(byte + i).copied().unwrap_or(0);
        }
        let v = u32::from_le_bytes(w) >> (self.pos & 7);
        v & ((1u32 << nb) - 1)
    }

    fn take(&mut self, nb: u32) -> u32 {
        let v = self.peek(nb);
        self.pos += nb as usize;
        v
    }
}

/// Reads a table description. Returns the distribution and the bytes consumed.
pub fn read(src: &[u8], max_symbols: usize, max_log: u32) -> Result<(Norm, usize)> {
    if src.is_empty() {
        return Err(Error::Truncated);
    }
    let mut r = Fwd { src, pos: 0 };
    let log = r.take(4) + 5;
    if log > max_log {
        return Err(Error::Corrupt("FSE accuracy log too large"));
    }
    let mut norm = Norm {
        counts: [0; MAX_SYMBOLS],
        symbols: 0,
        log,
    };
    let mut remaining: i32 = (1 << log) + 1;
    let mut threshold: i32 = 1 << log;
    let mut nb_bits = log + 1;
    let mut sym = 0usize;
    let mut prev0 = false;
    while remaining > 1 && sym < max_symbols {
        if prev0 {
            loop {
                let rep = r.take(2) as usize;
                sym += rep;
                if rep != 3 {
                    break;
                }
                if sym > max_symbols {
                    return Err(Error::Corrupt("FSE zero run past alphabet"));
                }
            }
            if sym >= max_symbols {
                break;
            }
        }
        let max = (2 * threshold - 1) - remaining;
        let low = r.peek(nb_bits - 1) as i32;
        let mut count = if low < max {
            r.pos += (nb_bits - 1) as usize;
            low
        } else {
            let mut c = r.take(nb_bits) as i32;
            if c >= threshold {
                c -= max;
            }
            c
        };
        count -= 1;
        remaining -= count.abs();
        norm.counts[sym] = count as i16;
        sym += 1;
        prev0 = count == 0;
        if remaining < 1 {
            break;
        }
        while remaining < threshold {
            nb_bits -= 1;
            threshold >>= 1;
        }
    }
    if remaining != 1 {
        return Err(Error::Corrupt("FSE probabilities do not sum to table size"));
    }
    let used = r.pos.div_ceil(8);
    if used > src.len() {
        return Err(Error::Truncated);
    }
    norm.symbols = sym;
    Ok((norm, used))
}
