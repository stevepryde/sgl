//! Sequences section (RFC 8878 §3.1.1.3.2): header, table modes, and the
//! decode-and-execute loop.
use super::DecoderDictionary;
use super::execute::Exec;
use super::literals::Lits;
use super::seqtable::{Kind, SeqEntry, SeqTable};
use super::state::{DecState, LL, ML, OF, Predefined, Src};
use crate::content::zstandard::bits::RevReader;
use crate::content::zstandard::error::{Error, Result};
use crate::content::zstandard::fse::ncount;

/// Parses the sequence count; returns (count, header bytes).
fn count(src: &[u8]) -> Result<(usize, usize)> {
    let b0 = *src.first().ok_or(Error::Truncated)? as usize;
    let byte = |i: usize| {
        src.get(i)
            .copied()
            .map(|b| b as usize)
            .ok_or(Error::Truncated)
    };
    Ok(match b0 {
        0..=127 => (b0, 1),
        128..=254 => (((b0 - 128) << 8) + byte(1)?, 2),
        _ => (byte(1)? + (byte(2)? << 8) + 0x7F00, 3),
    })
}

fn load_table(src: &[u8], mode: u8, i: usize, st: &mut DecState) -> Result<usize> {
    let kind = [Kind::LiteralLength, Kind::Offset, Kind::MatchLength][i];
    match mode {
        0 => {
            st.seq_src[i] = Src::Predefined;
            Ok(0)
        }
        1 => {
            let s = *src.first().ok_or(Error::Truncated)?;
            st.seq_local[i].rle(s, kind)?;
            st.seq_src[i] = Src::Local;
            Ok(1)
        }
        2 => {
            let (norm, used) = ncount::read(src, kind.max_symbols(), kind.max_log())?;
            st.seq_local[i].build(&norm, kind)?;
            st.seq_src[i] = Src::Local;
            Ok(used)
        }
        _ => {
            if st.seq_src[i] == Src::None {
                return Err(Error::Corrupt("repeat mode without a previous table"));
            }
            Ok(0)
        }
    }
}

#[inline(always)]
fn state_update(e: &SeqEntry, r: &mut RevReader<'_>) -> usize {
    e.next as usize + r.read(e.nb_bits as u32) as usize
}

/// Decodes the sequences section in `src` and executes it into `ex`.
pub fn decode(
    block: &[u8],
    seq_at: usize,
    lits: Lits,
    st: &mut DecState,
    pre: &Predefined,
    dict: Option<&DecoderDictionary>,
    ex: &mut Exec<'_>,
) -> Result<()> {
    let src = &block[seq_at..];
    let (n, mut at) = count(src)?;
    if n == 0 {
        if at != src.len() {
            return Err(Error::Corrupt("bytes after an empty sequences section"));
        }
        return match lits {
            Lits::InBlock(a, b) => ex.finish(&block[a..], b - a),
            Lits::Buffer(n) => ex.finish(&st.lits, n),
        };
    }
    let modes = *src.get(at).ok_or(Error::Truncated)?;
    at += 1;
    if modes & 3 != 0 {
        return Err(Error::Reserved);
    }
    for (i, shift) in [(LL, 6), (OF, 4), (ML, 2)] {
        at += load_table(&src[at..], (modes >> shift) & 3, i, st)?;
    }
    let missing = Error::Corrupt("missing sequence table");
    let ll_t: &SeqTable = st.seq_table(LL, pre, dict).ok_or(missing)?;
    let of_t: &SeqTable = st.seq_table(OF, pre, dict).ok_or(missing)?;
    let ml_t: &SeqTable = st.seq_table(ML, pre, dict).ok_or(missing)?;
    let mut r = RevReader::new(&src[at..])?;
    let mut ll_s = r.read(ll_t.log) as usize;
    let mut of_s = r.read(of_t.log) as usize;
    let mut ml_s = r.read(ml_t.log) as usize;
    let mut reps = st.reps;
    let (lits, lits_len): (&[u8], usize) = match lits {
        Lits::InBlock(a, b) => (&block[a..], b - a),
        Lits::Buffer(n) => (&st.lits, n),
    };
    for i in 0..n {
        let (lle, ofe, mle) = (ll_t.e[ll_s], of_t.e[of_s], ml_t.e[ml_s]);
        // Two refills per sequence: offset + match length take at most 31 + 16
        // bits, literal length + the three state updates at most 16 + 26 (<= 57).
        r.refill();
        let of_value = ofe.base + r.read(ofe.add_bits as u32) as u32;
        let ml = mle.base + r.read(mle.add_bits as u32) as u32;
        r.refill();
        let ll = lle.base + r.read(lle.add_bits as u32) as u32;
        let offset = if of_value > 3 {
            let o = of_value - 3;
            reps = [o, reps[0], reps[1]];
            o
        } else {
            let idx = of_value as usize - 1 + (ll == 0) as usize;
            match idx {
                0 => reps[0],
                1 => {
                    let o = reps[1];
                    reps = [o, reps[0], reps[2]];
                    o
                }
                2 => {
                    let o = reps[2];
                    reps = [o, reps[0], reps[1]];
                    o
                }
                _ => {
                    let o = reps[0].wrapping_sub(1);
                    if o == 0 {
                        return Err(Error::Corrupt("zero offset"));
                    }
                    reps = [o, reps[0], reps[1]];
                    o
                }
            }
        };
        ex.sequence(lits, lits_len, ll as usize, ml as usize, offset as usize)?;
        if i + 1 < n {
            ll_s = state_update(&lle, &mut r);
            ml_s = state_update(&mle, &mut r);
            of_s = state_update(&ofe, &mut r);
        }
    }
    r.refill();
    if !r.finished() {
        return Err(Error::Corrupt("sequence bitstream not consumed exactly"));
    }
    let done = ex.finish(lits, lits_len);
    st.reps = reps;
    done
}
