//! Per-frame decoder state: which entropy tables are current, repeat offsets,
//! and reusable buffers.
use super::DecoderDictionary;
use super::seqtable::{Kind, SeqTable};
use super::tables::*;
use crate::content::zstandard::error::Result;
use crate::content::zstandard::fse::Norm;
use crate::content::zstandard::huf::HufWeights;
use crate::content::zstandard::huf::dtable::HufDTable;
use std::vec::Vec;

/// Where the current table for a stream comes from ("Repeat" mode reuses it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Src {
    None,
    Predefined,
    Dict,
    Local,
}

/// Predefined sequence tables, built once per decompressor.
pub struct Predefined {
    pub ll: SeqTable,
    pub of: SeqTable,
    pub ml: SeqTable,
}

impl Predefined {
    pub fn new() -> Result<Predefined> {
        let mut p = Predefined {
            ll: SeqTable::default(),
            of: SeqTable::default(),
            ml: SeqTable::default(),
        };
        p.ll.build(
            &Norm::from_slice(&LL_DEFAULT, LL_DEFAULT_LOG),
            Kind::LiteralLength,
        )?;
        p.of.build(&Norm::from_slice(&OF_DEFAULT, OF_DEFAULT_LOG), Kind::Offset)?;
        p.ml.build(
            &Norm::from_slice(&ML_DEFAULT, ML_DEFAULT_LOG),
            Kind::MatchLength,
        )?;
        Ok(p)
    }
}

pub struct DecState {
    pub huf_src: Src,
    pub huf_local: HufDTable,
    pub seq_src: [Src; 3],
    pub seq_local: [SeqTable; 3],
    pub reps: [u32; 3],
    pub lits: Vec<u8>,
}

/// Index of each stream in `seq_src` / `seq_local`.
pub const LL: usize = 0;
pub const OF: usize = 1;
pub const ML: usize = 2;

impl DecState {
    pub fn new() -> DecState {
        let empty = HufWeights {
            w: [0; 256],
            symbols: 0,
            max_bits: 0,
        };
        DecState {
            huf_src: Src::None,
            huf_local: HufDTable::new(&empty),
            seq_src: [Src::None; 3],
            seq_local: [
                SeqTable::default(),
                SeqTable::default(),
                SeqTable::default(),
            ],
            reps: [1, 4, 8],
            lits: Vec::new(),
        }
    }

    /// Resets for a new frame, adopting the dictionary's tables and offsets if any.
    pub fn reset(&mut self, dict: Option<&DecoderDictionary>) {
        match dict {
            Some(d) if d.huf.is_some() => {
                self.huf_src = Src::Dict;
                self.seq_src = [Src::Dict; 3];
                self.reps = d.dict.reps;
            }
            Some(d) => {
                self.huf_src = Src::None;
                self.seq_src = [Src::None; 3];
                self.reps = d.dict.reps;
            }
            None => {
                self.huf_src = Src::None;
                self.seq_src = [Src::None; 3];
                self.reps = [1, 4, 8];
            }
        }
    }

    /// The table currently selected for stream `i`, if any.
    pub fn seq_table<'a>(
        &'a self,
        i: usize,
        pre: &'a Predefined,
        dict: Option<&'a DecoderDictionary>,
    ) -> Option<&'a SeqTable> {
        match self.seq_src[i] {
            Src::None => None,
            Src::Predefined => Some([&pre.ll, &pre.of, &pre.ml][i]),
            Src::Dict => dict.and_then(|d| d.seq.as_ref()).map(|s| &s[i]),
            Src::Local => Some(&self.seq_local[i]),
        }
    }
}
