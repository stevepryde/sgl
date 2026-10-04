//! Seedable RNG — a thin wrapper over `fastrand::Rng` (stack.md).
//!
//! The wrapper is the seam: sim code takes `&mut Rng` so tests can seed it for
//! determinism (AR-7 INV-5), and the backend stays swappable in one place.
//! Never use thread-local/global randomness in deterministic simulation code.

/// A deterministic, seedable random number generator.
#[derive(Debug, Clone)]
pub struct Rng {
    inner: fastrand::Rng,
}

impl Rng {
    /// A generator with an explicit seed: the same seed always yields the same
    /// sequence.
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        Self {
            inner: fastrand::Rng::with_seed(seed),
        }
    }

    /// A `f32` uniformly distributed in `[0, 1)`.
    pub fn f32(&mut self) -> f32 {
        self.inner.f32()
    }

    /// A `u32` uniformly distributed in `range`.
    pub fn u32(&mut self, range: std::ops::Range<u32>) -> u32 {
        self.inner.u32(range)
    }

    /// An `i32` uniformly distributed in `range`.
    pub fn i32(&mut self, range: std::ops::Range<i32>) -> i32 {
        self.inner.i32(range)
    }

    /// A `usize` uniformly distributed in `range` (e.g. an index).
    pub fn usize(&mut self, range: std::ops::Range<usize>) -> usize {
        self.inner.usize(range)
    }

    /// A fair coin flip.
    pub fn bool(&mut self) -> bool {
        self.inner.bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Same seed, same sequence — the determinism contract sim tests rely on.
    #[wasm_bindgen_test(unsupported = test)]
    fn same_seed_same_sequence() {
        let mut a = Rng::from_seed(42);
        let mut b = Rng::from_seed(42);
        for _ in 0..100 {
            assert_eq!(a.u32(0..1000), b.u32(0..1000));
            assert_eq!(a.f32().to_bits(), b.f32().to_bits());
            assert_eq!(a.bool(), b.bool());
        }
    }

    /// Different seeds diverge (not a proof, just a sanity check).
    #[wasm_bindgen_test(unsupported = test)]
    fn different_seeds_diverge() {
        let mut a = Rng::from_seed(1);
        let mut b = Rng::from_seed(2);
        let sa: Vec<u32> = (0..16).map(|_| a.u32(0..u32::MAX)).collect();
        let sb: Vec<u32> = (0..16).map(|_| b.u32(0..u32::MAX)).collect();
        assert_ne!(sa, sb);
    }

    /// Ranges are respected.
    #[wasm_bindgen_test(unsupported = test)]
    fn values_stay_in_range() {
        let mut rng = Rng::from_seed(7);
        for _ in 0..1000 {
            let v = rng.usize(3..9);
            assert!((3..9).contains(&v));
            let f = rng.f32();
            assert!((0.0..1.0).contains(&f));
        }
    }
}
