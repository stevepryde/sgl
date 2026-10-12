//! Load allocation bounds shared by glTF sources, decoded assets and KTX2.
use super::asset::Result;

/// Maximum decoded image side, independent of untrusted file dimensions.
/// The scene still checks the actual device's (possibly lower) limit.
pub(super) const MAX_IMAGE_SIDE: u32 = 16384;

/// Native scene budget: 4 GiB accommodates the largest supported public
/// scenes (Lucy is about 1.46 GiB of vertices and indices).
#[cfg(not(target_arch = "wasm32"))]
pub(super) const MAX_LOAD_BYTES: u64 = 4 << 30;
/// Browser: a quarter of wasm32's address space, leaving room for source
/// bytes, decode/batching working copies and the game's other content.
#[cfg(target_arch = "wasm32")]
pub(super) const MAX_LOAD_BYTES: u64 = 1 << 30;

pub(super) struct Budget {
    pub(super) remaining: u64,
}

impl Budget {
    pub(super) fn new() -> Self {
        Self {
            remaining: MAX_LOAD_BYTES,
        }
    }

    pub(super) fn remaining(&self) -> u64 {
        self.remaining
    }

    pub(super) fn charge(&mut self, bytes: u64) -> Result<()> {
        self.remaining = self.remaining.checked_sub(bytes).ok_or_else(|| {
            format!("data exceeds the {MAX_LOAD_BYTES} byte load budget; split the asset")
        })?;
        Ok(())
    }
}
