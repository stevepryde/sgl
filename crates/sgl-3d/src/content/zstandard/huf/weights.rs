//! Huffman tree description (RFC 8878 §4.2.1): reading and writing.
use super::{HufWeights, MAX_BITS};
use crate::content::zstandard::bits::{Refill, RevReader};
use crate::content::zstandard::error::{Error, Result};
use crate::content::zstandard::fse::dtable::{self, DState};
use crate::content::zstandard::fse::{highbit, ncount};

/// Reads a tree description; returns the weights and the bytes consumed.
pub fn read(src: &[u8]) -> Result<(HufWeights, usize)> {
    let header = *src.first().ok_or(Error::Truncated)? as usize;
    let mut w = [0u8; 256];
    let (count, used) = if header >= 128 {
        let n = header - 127;
        let bytes = n.div_ceil(2);
        let body = src.get(1..1 + bytes).ok_or(Error::Truncated)?;
        for i in 0..n {
            let b = body[i / 2];
            w[i] = if i % 2 == 0 { b >> 4 } else { b & 15 };
        }
        (n, 1 + bytes)
    } else {
        let body = src.get(1..1 + header).ok_or(Error::Truncated)?;
        (decode_fse_weights(body, &mut w)?, 1 + header)
    };
    Ok((complete(w, count)?, used))
}

fn decode_fse_weights(body: &[u8], w: &mut [u8; 256]) -> Result<usize> {
    if body.is_empty() {
        return Err(Error::Corrupt("empty Huffman weight stream"));
    }
    let (norm, used) = ncount::read(body, 16, 6)?;
    let mut table = [dtable::DEntry::default(); 64];
    let mut n_entries = 0;
    dtable::build_with(&norm, |e| {
        table[n_entries] = e;
        n_entries += 1;
    })?;
    let table = &table[..n_entries];
    let mut r = RevReader::new(&body[used..])?;
    let mut s1 = DState::init(&mut r, norm.log);
    let mut s2 = DState::init(&mut r, norm.log);
    let mut n = 0usize;
    loop {
        if n > 253 {
            return Err(Error::Corrupt("too many Huffman weights"));
        }
        r.refill();
        w[n] = s1.symbol(table);
        n += 1;
        s1.update(table, &mut r);
        if r.refill() == Refill::Overflow {
            w[n] = s2.symbol(table);
            return Ok(n + 1);
        }
        if n > 253 {
            return Err(Error::Corrupt("too many Huffman weights"));
        }
        w[n] = s2.symbol(table);
        n += 1;
        s2.update(table, &mut r);
        if r.refill() == Refill::Overflow {
            w[n] = s1.symbol(table);
            return Ok(n + 1);
        }
    }
}

/// Validates explicit weights and appends the implied last one.
fn complete(mut w: [u8; 256], count: usize) -> Result<HufWeights> {
    let mut total: u32 = 0;
    let mut rank1 = 0;
    for &x in &w[..count] {
        if x as u32 > MAX_BITS {
            return Err(Error::Corrupt("Huffman weight too large"));
        }
        if x > 0 {
            total += 1 << (x - 1);
        }
        rank1 += (x == 1) as u32;
    }
    if total == 0 || count >= 256 {
        return Err(Error::Corrupt("invalid Huffman weights"));
    }
    let max_bits = highbit(total) + 1;
    if max_bits > MAX_BITS {
        return Err(Error::Corrupt("Huffman table too deep"));
    }
    let rest = (1u32 << max_bits) - total;
    if !rest.is_power_of_two() {
        return Err(Error::Corrupt("Huffman weights not completable"));
    }
    let last = highbit(rest) + 1;
    w[count] = last as u8;
    rank1 += (last == 1) as u32;
    if rank1 < 2 || rank1 & 1 == 1 {
        return Err(Error::Corrupt("Huffman rank-1 count invalid"));
    }
    Ok(HufWeights {
        w,
        symbols: count + 1,
        max_bits,
    })
}
