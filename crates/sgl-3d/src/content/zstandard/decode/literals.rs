//! Literals section decoding (RFC 8878 §3.1.1.3.1).
use super::DecoderDictionary;
use super::execute::SLACK;
use super::state::{DecState, Src};
use crate::content::zstandard::error::{Error, Result};
use crate::content::zstandard::huf::dtable::HufDTable;
use crate::content::zstandard::huf::weights;

/// Where the decoded literals of a block live.
pub enum Lits {
    /// Raw literals, stored at this range of the block.
    InBlock(usize, usize),
    /// `n` literals at the start of the decoder's literal buffer (which has slack).
    Buffer(usize),
}

/// Parses the literals section at the start of `block`. Returns where the
/// literals are and how many block bytes the section used.
/// `block_max` is the frame's Block_Maximum_Size (min(window, 128 KiB)).
pub fn decode(
    block: &[u8],
    st: &mut DecState,
    dict: Option<&DecoderDictionary>,
    block_max: usize,
) -> Result<(Lits, usize)> {
    let b0 = *block.first().ok_or(Error::Truncated)? as usize;
    let ty = b0 & 3;
    let sf = (b0 >> 2) & 3;
    let byte = |i: usize| {
        block
            .get(i)
            .copied()
            .map(|b| b as usize)
            .ok_or(Error::Truncated)
    };
    if ty < 2 {
        let (size, hlen) = match sf {
            0 | 2 => (b0 >> 3, 1),
            1 => ((b0 >> 4) | (byte(1)? << 4), 2),
            _ => ((b0 >> 4) | (byte(1)? << 4) | (byte(2)? << 12), 3),
        };
        if size > block_max {
            return Err(Error::Corrupt("literals larger than a block"));
        }
        if ty == 0 {
            if block.len() < hlen + size {
                return Err(Error::Truncated);
            }
            return Ok((Lits::InBlock(hlen, hlen + size), hlen + size));
        }
        let v = byte(hlen)? as u8;
        grow(&mut st.lits, size);
        st.lits[..size].fill(v);
        return Ok((Lits::Buffer(size), hlen + 1));
    }
    let hlen = [3, 3, 4, 5][sf];
    let mut v = 0u64;
    for i in (0..hlen).rev() {
        v = (v << 8) | byte(i)? as u64;
    }
    let bits = [10, 10, 14, 18][sf];
    let mask = (1u64 << bits) - 1;
    let (regen, comp, four) = (
        ((v >> 4) & mask) as usize,
        ((v >> (4 + bits)) & mask) as usize,
        sf != 0,
    );
    if regen > block_max {
        return Err(Error::Corrupt("literals larger than a block"));
    }
    let body = block.get(hlen..hlen + comp).ok_or(Error::Truncated)?;
    let streams = if ty == 2 {
        let (hw, used) = weights::read(body)?;
        st.huf_local.rebuild(&hw);
        st.huf_src = Src::Local;
        &body[used..]
    } else {
        body
    };
    let table: &HufDTable = match st.huf_src {
        Src::Local => Some(&st.huf_local),
        Src::Dict => dict.and_then(|d| d.huf.as_ref()),
        _ => None,
    }
    .ok_or(Error::Corrupt("treeless literals without a previous table"))?;
    grow(&mut st.lits, regen);
    if four {
        table.decode_4x(streams, &mut st.lits[..regen])?;
    } else {
        table.decode_1x(streams, &mut st.lits[..regen])?;
    }
    Ok((Lits::Buffer(regen), hlen + comp))
}

/// Makes `lits[..n + SLACK]` addressable without re-zeroing what is already there.
fn grow(lits: &mut std::vec::Vec<u8>, n: usize) {
    if lits.len() < n + SLACK {
        lits.resize(n + SLACK, 0);
    }
}
