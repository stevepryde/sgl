//! Load allocation bounds shared by glTF sources, decoded assets and KTX2.
use super::asset::Result;

/// Maximum decoded image side, independent of untrusted file dimensions.
/// The scene still checks the actual device's (possibly lower) limit.
pub(super) const MAX_IMAGE_SIDE: u32 = 16384;

#[cfg(not(target_arch = "wasm32"))]
pub(super) const DEFAULT_LOAD_BYTES: u64 = 4 << 30;
#[cfg(target_arch = "wasm32")]
pub(super) const DEFAULT_LOAD_BYTES: u64 = 1 << 30;

/// Independent safety caps for one asset import's source and decoded bytes.
///
/// Defaults are 4 GiB per cap natively and 1 GiB per cap on wasm32. Both
/// caps must be nonzero and at most [`Self::maximum_bytes`] (which also
/// bounds the default on a smaller-address-space target). Raising them
/// permits larger imports; it does not reserve memory or guarantee a load
/// fits the process, browser's shared WASM heap or GPU. Source bytes, decoded
/// bytes, decoder scratch, batching copies and allocator capacity can coexist.
/// Device limits and image dimensions remain independently enforced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadBudget {
    source_bytes: u64,
    decoded_bytes: u64,
}

impl LoadBudget {
    /// Validate explicit source and decoded caps, in bytes. Invalid caps are
    /// rejected, never clamped; zero does not mean unlimited.
    pub fn new(source_bytes: u64, decoded_bytes: u64) -> Result<Self> {
        for (kind, requested) in [("source", source_bytes), ("decoded", decoded_bytes)] {
            if requested == 0 || requested > Self::maximum_bytes() {
                return Err(format!(
                    "requested {kind} load budget {requested} bytes is invalid: use 1..={} bytes on this target; no budget was applied",
                    Self::maximum_bytes()
                )
                .into());
            }
        }
        Ok(Self {
            source_bytes,
            decoded_bytes,
        })
    }

    /// Conservative per-cap policy ceiling: Rust's `Vec` byte-allocation
    /// representability limit, `isize::MAX`. This is not available memory or
    /// a WebGPU limit; several smaller allocations may also exhaust memory.
    pub const fn maximum_bytes() -> u64 {
        isize::MAX as u64
    }

    /// Source-byte cap, including the document and imported source copies.
    pub const fn source_bytes(self) -> u64 {
        self.source_bytes
    }

    /// Decoded scene/image-byte cap, independent of the source cap.
    pub const fn decoded_bytes(self) -> u64 {
        self.decoded_bytes
    }
}

impl Default for LoadBudget {
    fn default() -> Self {
        Self {
            source_bytes: DEFAULT_LOAD_BYTES.min(Self::maximum_bytes()),
            decoded_bytes: DEFAULT_LOAD_BYTES.min(Self::maximum_bytes()),
        }
    }
}

pub(super) struct Budget {
    limit: u64,
    remaining: u64,
    kind: &'static str,
}

impl Budget {
    pub(super) fn new(limit: u64, kind: &'static str) -> Self {
        Self {
            limit,
            remaining: limit,
            kind,
        }
    }

    pub(super) fn source(budget: LoadBudget) -> Self {
        Self::new(budget.source_bytes(), "source")
    }

    pub(super) fn decoded(budget: LoadBudget) -> Self {
        Self::new(budget.decoded_bytes(), "decoded")
    }

    pub(super) fn remaining(&self) -> u64 {
        self.remaining
    }

    pub(super) fn check(&self, bytes: u64) -> Result<()> {
        if bytes > self.remaining {
            return Err(format!(
                "{} load budget exceeded: requested {bytes} bytes with {} bytes remaining (configured limit {} bytes, used {} bytes); raise the explicit budget or split the asset",
                self.kind, self.remaining, self.limit, self.limit - self.remaining
            )
            .into());
        }
        Ok(())
    }

    pub(super) fn charge(&mut self, bytes: u64) -> Result<()> {
        self.check(bytes)?;
        self.remaining -= bytes;
        Ok(())
    }

    pub(super) fn charge_count(&mut self, count: u64, size: u64) -> Result<()> {
        let bytes = count.checked_mul(size).ok_or_else(|| {
            format!(
                "{} load byte count overflow: requested {count} elements of {size} bytes (configured limit {} bytes)",
                self.kind, self.limit
            )
        })?;
        self.charge(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test(unsupported = test)]
    fn explicit_caps_are_nonzero_bounded_and_never_clamped() {
        for (source, decoded, kind) in [
            (0, 1, "source"),
            (1, 0, "decoded"),
            (u64::MAX, 1, "source"),
            (1, u64::MAX, "decoded"),
        ] {
            let error = LoadBudget::new(source, decoded).unwrap_err().to_string();
            assert!(
                error.contains(kind) && error.contains("requested"),
                "{error}"
            );
        }
        // The representability boundary is accepted without allocating it.
        let maximum = isize::MAX as u64;
        let budget = LoadBudget::new(maximum, maximum).unwrap();
        assert_eq!(budget.source_bytes(), maximum);
        assert_eq!(budget.decoded_bytes(), maximum);
        assert!(LoadBudget::new(maximum + 1, 1).is_err());
        assert!(LoadBudget::new(1, maximum + 1).is_err());
        let small = LoadBudget::new(7, 11).unwrap();
        assert_eq!((small.source_bytes(), small.decoded_bytes()), (7, 11));
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn defaults_remain_platform_specific_and_can_be_raised() {
        let budget = LoadBudget::default();
        #[cfg(target_arch = "wasm32")]
        let expected = 1_073_741_824;
        #[cfg(not(target_arch = "wasm32"))]
        let expected = 4_294_967_296u64.min(isize::MAX as u64);
        assert_eq!(
            (budget.source_bytes(), budget.decoded_bytes()),
            (expected, expected)
        );
        if expected < isize::MAX as u64 {
            assert!(LoadBudget::new(expected + 1, expected + 1).is_ok());
        }
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn charges_are_exact_atomic_and_checked_for_overflow() {
        let mut budget = Budget::new(11, "decoded");
        budget.charge_count(2, 3).unwrap();
        let error = budget.charge(6).unwrap_err().to_string();
        assert!(
            error.contains("requested 6 bytes")
                && error.contains("5 bytes remaining")
                && error.contains("limit 11 bytes"),
            "{error}"
        );
        assert_eq!(budget.remaining(), 5);
        let error = budget.charge_count(u64::MAX, 2).unwrap_err().to_string();
        assert!(
            error.contains("overflow") && error.contains("limit 11 bytes"),
            "{error}"
        );
        assert_eq!(budget.remaining(), 5);
        budget.charge(5).unwrap();
        assert_eq!(budget.remaining(), 0);
        assert!(budget.charge(1).is_err());
    }
}
