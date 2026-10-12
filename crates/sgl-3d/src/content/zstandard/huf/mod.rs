//! Huffman coding of literals: tree descriptions, decoding and encoding.
pub mod dtable;
mod fast;
pub mod weights;

/// zstd limits Huffman codes to 11 bits.
pub const MAX_BITS: u32 = 11;

/// A Huffman tree as zstd describes it: per-symbol weights, where weight `w > 0`
/// means a code of `max_bits + 1 - w` bits and weight 0 means "absent".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HufWeights {
    pub w: [u8; 256],
    /// Highest present symbol + 1 (its weight is implied on the wire).
    pub symbols: usize,
    pub max_bits: u32,
}
