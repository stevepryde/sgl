//! Private, decode-only port of meshoptimizer's buffer decoders for
//! `EXT_meshopt_compression` and `KHR_meshopt_compression`. See README.md
//! for provenance; `gltf::meshopt` applies them to buffer views.
#![forbid(unsafe_code)]

pub(super) mod filter;
pub(super) mod index;
#[cfg(test)]
mod tests;
pub(super) mod vertex;

/// A stream that breaks its format: truncated, with an invalid header,
/// mode or version, or with bytes left over. The reason is static text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Error(pub(super) &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

pub(super) type Result<T> = std::result::Result<T, Error>;
