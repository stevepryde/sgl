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
#[must_use]
pub fn derive_stream_seed(base: u64, domain: u64, a: u32, b: u64) -> u64 {
    let mut rng = SplitMix64::new(base ^ domain.rotate_left(17));
    rng.cursor ^= u64::from(a).rotate_left(31);
    rng.cursor ^= b.rotate_left(47);
    rng.next_u64()
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

    /// #249: stream derivation mixes with XOR, so components whose rotated
    /// bits overlap cancel rather than accumulate. Frozen values for a base
    /// equal to its domain and for all-ones components.
    #[wasm_bindgen_test(unsupported = test)]
    fn derived_seeds_with_overlapping_components_are_frozen() {
        assert_eq!(
            (
                derive_stream_seed(7, 7, 7, 7),
                derive_stream_seed(u64::MAX, u64::MAX, u32::MAX, u64::MAX),
                derive_stream_seed(1 << 47, 1 << 30, 1 << 16, 1),
            ),
            (
                1_085_486_224_634_377_940,
                1_817_513_978_328_618_816,
                16_294_208_416_658_607_535
            )
        );
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
        assert_eq!(derive_stream_seed(17, 29, 41, 53), 0x002e_c4c0_58ec_97c1);
    }
}
