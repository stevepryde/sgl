//! The single error type returned by every fallible operation in the crate.
use core::fmt;

/// What went wrong. Decoding never panics on malformed input; it returns one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The input ended before a complete structure could be read.
    Truncated,
    /// The input does not start with a zstd or skippable frame magic number.
    BadMagic,
    /// A reserved bit or field was set.
    Reserved,
    /// The frame's window is larger than the decoder accepts.
    WindowTooLarge,
    /// The frame requires a dictionary but none was supplied.
    MissingDictionary,
    /// The frame names a dictionary ID different from the supplied dictionary's.
    WrongDictionary,
    /// Decoding would exceed the caller's output limit (decompression-bomb guard).
    OutputLimit,
    /// The frame's XXH64 checksum does not match the decoded content.
    Checksum,
    /// The decoded size disagrees with the frame's declared content size.
    ContentSize,
    /// The compressed data is internally inconsistent; the reason is static text.
    Corrupt(&'static str),
    /// A caller-supplied parameter is out of range; the reason is static text.
    Parameter(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("input truncated"),
            Error::BadMagic => f.write_str("unknown frame magic"),
            Error::Reserved => f.write_str("reserved bit or field set"),
            Error::WindowTooLarge => f.write_str("window size exceeds decoder limit"),
            Error::MissingDictionary => f.write_str("frame needs a dictionary"),
            Error::WrongDictionary => f.write_str("frame dictionary ID does not match"),
            Error::OutputLimit => f.write_str("output limit exceeded"),
            Error::Checksum => f.write_str("content checksum mismatch"),
            Error::ContentSize => f.write_str("content size mismatch"),
            Error::Corrupt(why) => write!(f, "corrupt data: {why}"),
            Error::Parameter(why) => write!(f, "invalid parameter: {why}"),
        }
    }
}

impl core::error::Error for Error {}

/// Crate-wide result alias.
pub type Result<T> = core::result::Result<T, Error>;
