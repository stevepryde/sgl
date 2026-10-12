//! Finite State Entropy (tANS) as used by zstd: table descriptions, decoding
//! tables, encoding tables and count normalization.
pub mod dtable;
pub mod ncount;

/// Largest alphabet any zstd FSE table uses (match length codes: 53; weights: 13).
pub const MAX_SYMBOLS: usize = 256;

/// A normalized distribution: `counts[s]` sums (with -1 counted as 1) to `1 << log`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Norm {
    pub counts: [i16; MAX_SYMBOLS],
    /// Number of meaningful entries (highest symbol + 1).
    pub symbols: usize,
    pub log: u32,
}

impl Norm {
    pub fn from_slice(counts: &[i16], log: u32) -> Self {
        let mut n = Norm {
            counts: [0; MAX_SYMBOLS],
            symbols: counts.len(),
            log,
        };
        n.counts[..counts.len()].copy_from_slice(counts);
        n
    }
}

/// Index of the highest set bit (`v` > 0).
#[inline(always)]
pub fn highbit(v: u32) -> u32 {
    31 - v.leading_zeros()
}

/// Spreads symbols over a table exactly as the reference does; `table[i]` is the
/// symbol owning state `i`. Returns an error if the distribution is inconsistent.
pub fn spread(norm: &Norm, table: &mut [u8]) -> crate::content::zstandard::Result<()> {
    let size = 1usize << norm.log;
    debug_assert_eq!(table.len(), size);
    let mut high = size - 1;
    for s in 0..norm.symbols {
        if norm.counts[s] == -1 {
            table[high] = s as u8;
            high = high.wrapping_sub(1);
        }
    }
    let step = (size >> 1) + (size >> 3) + 3;
    let mask = size - 1;
    let mut pos = 0usize;
    for s in 0..norm.symbols {
        for _ in 0..norm.counts[s].max(0) {
            table[pos] = s as u8;
            pos = (pos + step) & mask;
            while pos > high {
                pos = (pos + step) & mask;
            }
        }
    }
    if pos != 0 {
        return Err(crate::content::zstandard::Error::Corrupt(
            "FSE distribution does not fill its table",
        ));
    }
    Ok(())
}
