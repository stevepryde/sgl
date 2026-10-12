//! Sequence decoding tables: FSE states fused with each code's baseline and extra bits.
use super::tables::{LL_CODES, ML_CODES};
use crate::content::zstandard::error::{Error, Result};
use crate::content::zstandard::fse::{Norm, dtable};
use std::vec::Vec;

/// Which of the three sequence symbol streams a table decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    LiteralLength,
    Offset,
    MatchLength,
}

impl Kind {
    pub fn max_symbols(self) -> usize {
        match self {
            Kind::LiteralLength => 36,
            Kind::Offset => super::tables::MAX_OF_CODE + 1,
            Kind::MatchLength => 53,
        }
    }

    pub fn max_log(self) -> u32 {
        match self {
            Kind::LiteralLength => super::tables::LL_MAX_LOG,
            Kind::Offset => super::tables::OF_MAX_LOG,
            Kind::MatchLength => super::tables::ML_MAX_LOG,
        }
    }

    #[inline(always)]
    fn code(self, c: u8) -> (u32, u8) {
        match self {
            Kind::LiteralLength => LL_CODES[c as usize],
            Kind::MatchLength => ML_CODES[c as usize],
            Kind::Offset => (1u32 << c, c),
        }
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct SeqEntry {
    pub base: u32,
    pub next: u16,
    pub nb_bits: u8,
    pub add_bits: u8,
}

/// A decoding table for one sequence stream (`1 << log` entries).
#[derive(Clone, Debug, Default)]
pub struct SeqTable {
    pub e: Vec<SeqEntry>,
    pub log: u32,
}

impl SeqTable {
    /// Rebuilds `self` from a distribution, reusing the allocation.
    pub fn build(&mut self, norm: &Norm, kind: Kind) -> Result<()> {
        if norm.symbols > kind.max_symbols() || norm.log > kind.max_log() {
            return Err(Error::Corrupt("sequence table out of range"));
        }
        self.e.clear();
        let e = &mut self.e;
        dtable::build_with(norm, |d| {
            let (base, add_bits) = kind.code(d.symbol);
            e.push(SeqEntry {
                base,
                next: d.base,
                nb_bits: d.nb_bits,
                add_bits,
            });
        })?;
        self.log = norm.log;
        Ok(())
    }

    /// Rebuilds `self` as a single-symbol (RLE mode) table.
    pub fn rle(&mut self, symbol: u8, kind: Kind) -> Result<()> {
        if symbol as usize >= kind.max_symbols() {
            return Err(Error::Corrupt("RLE sequence code out of range"));
        }
        let (base, add_bits) = kind.code(symbol);
        self.e.clear();
        self.e.push(SeqEntry {
            base,
            next: 0,
            nb_bits: 0,
            add_bits,
        });
        self.log = 0;
        Ok(())
    }
}
