//! Explicit, target-independent BLAKE3 state hashing.

use core::fmt;

/// A 32-byte canonical BLAKE3 digest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    /// Computes the exact, unframed BLAKE3 digest of `bytes`.
    ///
    /// Use this for authored content hashes. Deterministic simulation state should
    /// continue to use [`StateHasher`] so its canonical encoding remains explicit.
    #[must_use]
    pub fn hash_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Returns lower-case hexadecimal with exactly 64 characters.
    #[must_use]
    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut result = String::with_capacity(64);
        for byte in self.0 {
            result.push(char::from(HEX[usize::from(byte >> 4)]));
            result.push(char::from(HEX[usize::from(byte & 15)]));
        }
        result
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Digest({self})")
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// Values with one frozen canonical byte representation.
pub trait CanonicalWrite {
    /// Writes this value using its documented canonical byte representation.
    fn canonical_write(&self, out: &mut StateHasher);
}

/// An explicit BLAKE3 writer for deterministic state.
///
/// The encoding is schema-driven: each write appends its fixed-width
/// little-endian bytes without a type tag, and [`bytes`](Self::bytes) and
/// [`sequence`](Self::sequence) prefix a `u32` length. Write sequences with
/// the same schema (the same methods in the same order) hash differently
/// whenever their values differ. Different schemas may encode alike —
/// `u16(0x1234)` equals `u8(0x34); u8(0x12)` — so a caller that hashes several
/// kinds of state in one stream, or changes what it writes, separates them
/// itself with a leading tag or version (for example `u32(KIND)`).
#[derive(Default)]
pub struct StateHasher {
    inner: blake3::Hasher,
}

impl StateHasher {
    /// Creates an empty state hasher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one byte.
    pub fn u8(&mut self, value: u8) {
        self.inner.update(&[value]);
    }
    /// Appends a two-byte little-endian integer.
    pub fn u16(&mut self, value: u16) {
        self.inner.update(&value.to_le_bytes());
    }
    /// Appends a four-byte little-endian integer.
    pub fn u32(&mut self, value: u32) {
        self.inner.update(&value.to_le_bytes());
    }
    /// Appends an eight-byte little-endian integer.
    pub fn u64(&mut self, value: u64) {
        self.inner.update(&value.to_le_bytes());
    }
    /// Appends a signed four-byte little-endian integer.
    pub fn i32(&mut self, value: i32) {
        self.inner.update(&value.to_le_bytes());
    }
    /// Appends a signed eight-byte little-endian integer.
    pub fn i64(&mut self, value: i64) {
        self.inner.update(&value.to_le_bytes());
    }
    /// Appends a boolean as exactly `0` or `1`.
    pub fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    /// Appends bytes preceded by their checked `u32` length.
    pub fn bytes(&mut self, bytes: &[u8]) {
        self.length(bytes.len());
        self.inner.update(bytes);
    }

    /// Appends a collection length as `u32`.
    ///
    /// # Panics
    ///
    /// Panics when `length` cannot be represented in the frozen format.
    pub fn length(&mut self, length: usize) {
        self.u32(u32::try_from(length).expect("canonical collection length exceeds u32"));
    }

    /// Appends a canonical value.
    pub fn write<T: CanonicalWrite + ?Sized>(&mut self, value: &T) {
        value.canonical_write(self);
    }

    /// Appends an ordered sequence with a `u32` length prefix.
    pub fn sequence<T: CanonicalWrite>(&mut self, values: &[T]) {
        self.length(values.len());
        for value in values {
            self.write(value);
        }
    }

    /// Finalizes the digest.
    #[must_use]
    pub fn finish(self) -> Digest {
        Digest(*self.inner.finalize().as_bytes())
    }
}

macro_rules! canonical_integer {
    ($type:ty, $method:ident) => {
        impl CanonicalWrite for $type {
            fn canonical_write(&self, out: &mut StateHasher) {
                out.$method(*self);
            }
        }
    };
}

canonical_integer!(u8, u8);
canonical_integer!(u16, u16);
canonical_integer!(u32, u32);
canonical_integer!(u64, u64);
canonical_integer!(i32, i32);
canonical_integer!(i64, i64);

impl CanonicalWrite for bool {
    fn canonical_write(&self, out: &mut StateHasher) {
        out.bool(*self);
    }
}

/// Computes the canonical digest of one value.
#[must_use]
pub fn digest_of<T: CanonicalWrite + ?Sized>(value: &T) -> Digest {
    let mut hasher = StateHasher::new();
    hasher.write(value);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #249: `bool` writes through the `CanonicalWrite` trait are the typed
    /// bool encoding, so `true` and `false` hash differently and match the
    /// direct method.
    #[wasm_bindgen_test(unsupported = test)]
    fn bool_writes_through_the_trait_are_canonical() {
        let mut direct = StateHasher::new();
        direct.bool(true);
        assert_eq!(digest_of(&true), direct.finish());
        assert_ne!(digest_of(&true), digest_of(&false));
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn length_prefixes_and_order_are_unambiguous() {
        let mut split_a = StateHasher::new();
        split_a.bytes(b"ab");
        split_a.bytes(b"c");
        let mut split_b = StateHasher::new();
        split_b.bytes(b"a");
        split_b.bytes(b"bc");
        assert_ne!(split_a.finish(), split_b.finish());
        let mut first = StateHasher::new();
        first.sequence(&[1_u32, 2]);
        let mut second = StateHasher::new();
        second.sequence(&[2_u32, 1]);
        assert_ne!(first.finish(), second.finish());
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn canonical_digest_is_frozen() {
        let mut hasher = StateHasher::new();
        hasher.u16(0x1234);
        hasher.i32(-2);
        hasher.bool(true);
        hasher.bytes(b"sgl");
        assert_eq!(
            hasher.finish().to_hex(),
            "e18ef0865fe504e976275cc32bb1b8bbcbf812fd92b8e1e21be43c29a979444a"
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn raw_content_digest_is_exact_blake3() {
        assert_eq!(
            Digest::hash_bytes(b"abc").to_hex(),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
    }
}
