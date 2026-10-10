//! Frozen `SplitMix64` stream.

/// The frozen `SplitMix64` pseudo-random generator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitMix64 {
    stream_seed: u64,
    cursor: u64,
}

impl SplitMix64 {
    /// Starts a stream from its immutable seed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            stream_seed: seed,
            cursor: seed,
        }
    }

    /// Returns the immutable stream seed, unaffected by draws.
    #[must_use]
    pub const fn stream_seed(self) -> u64 {
        self.stream_seed
    }

    /// Returns the mutable generator cursor.
    #[must_use]
    pub const fn cursor(self) -> u64 {
        self.cursor
    }

    /// Produces the next frozen `SplitMix64` word.
    pub fn next_u64(&mut self) -> u64 {
        self.cursor = self.cursor.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.cursor;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    /// Samples the inclusive range using frozen modulo reduction.
    ///
    /// # Panics
    ///
    /// Panics when `min > max`.
    #[must_use]
    pub fn range_inclusive(&mut self, min: u32, max: u32) -> u32 {
        assert!(min <= max, "range_inclusive requires min <= max");
        let width = u64::from(max - min) + 1;
        min + u32::try_from(self.next_u64() % width)
            .expect("modulo result fits within the requested u32 range")
    }
}

/// Derives a deterministic stream seed from a game-owned base seed and three
/// caller-supplied domain components.
///
/// Each component is absorbed in turn and mixed by one `SplitMix64` step, a
/// bijection, so changing any single component always changes the seed and
/// no two components can cancel. Inputs that differ in several components
/// share a seed only by 64-bit chance.
#[must_use]
pub fn derive_stream_seed(base: u64, domain: u64, a: u32, b: u64) -> u64 {
    let mix = |state: u64| SplitMix64::new(state).next_u64();
    [domain, u64::from(a), b]
        .into_iter()
        .fold(mix(base), |state, component| mix(state ^ component))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #249: a fresh generator exposes its seed as both the stream seed and
    /// the cursor; a draw moves the cursor by the `SplitMix64` increment.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_fresh_generator_exposes_its_seed_and_cursor() {
        let mut rng = SplitMix64::new(42);
        assert_eq!(rng.stream_seed(), 42);
        assert_eq!(rng.cursor(), 42);
        rng.next_u64();
        assert_eq!(rng.cursor(), 42u64.wrapping_add(0x9e37_79b9_7f4a_7c15));
    }

    /// #312: components whose bits once overlapped after rotation (and
    /// cancelled under XOR) give different seeds: chunk `(65536, 0)` versus
    /// `(0, 1)`, and a domain bit versus a `b` bit.
    #[wasm_bindgen_test(unsupported = test)]
    fn overlapping_components_do_not_cancel() {
        for (left, right) in [
            ((9, 3, 1 << 16, 0), (9, 3, 0, 1)),
            ((9, 1, 5, 0), (9, 0, 5, 1 << 34)),
        ] {
            assert_ne!(
                derive_stream_seed(left.0, left.1, left.2, left.3),
                derive_stream_seed(right.0, right.1, right.2, right.3),
                "{left:?} vs {right:?}"
            );
        }
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn splitmix64_sequence_is_frozen() {
        let mut rng = SplitMix64::new(0);
        assert_eq!(rng.next_u64(), 0xe220_a839_7b1d_cdaf);
        assert_eq!(rng.next_u64(), 0x6e78_9e6a_a1b9_65f4);
        assert_eq!(rng.next_u64(), 0x06c4_5d18_8009_454f);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn inclusive_range_sampling_is_stable() {
        let mut rng = SplitMix64::new(7);
        assert_eq!(rng.range_inclusive(3, 7), 5);
        assert_eq!(rng.range_inclusive(3, 7), 7);
        assert_eq!(rng.range_inclusive(3, 7), 4);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn derived_seed_is_frozen() {
        assert_eq!(derive_stream_seed(17, 29, 41, 53), 0xfa0d_d366_259b_7519);
    }
}
