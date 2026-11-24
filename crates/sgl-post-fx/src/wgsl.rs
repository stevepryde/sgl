//! WGSL as SGL's crates embed it (#895): whole in a build with debug
//! assertions, and in one without them (a release build) with its comments,
//! the indentation that starts a line and the whitespace that ends one
//! removed at compile time, so a browser game's module does not carry them.
//! Every embedded WGSL file of `sgl-post-fx` and `sgl-3d` passes through
//! [`embed_wgsl!`](crate::embed_wgsl), the one owner of the transformation.
//!
//! Nothing else changes: a line keeps its tokens and the whitespace between
//! them, a comment between two tokens on one line becomes a space, and every
//! line stays, emptied where it held only a comment or blankspace, so naga
//! reports a stripped program's errors at the same lines as the file's.
//! Comments end as naga 30's lexer ends them (`front::wgsl::parse::lexer`,
//! `consume_token`): a line comment at the first WGSL line break, a block
//! comment at the `*/` matching its `/*`, nested ones counted. WGSL has no
//! string or character literals, so `//` and `/*` begin a comment wherever
//! they are not already inside one. The files in the repository are
//! untouched: ported shaders stay as their upstream wrote them. A file whose
//! block comment is not closed is refused, at compile time in a release
//! build. Evaluating the stripping costs a release build of `sgl-3d` about
//! 7 s (#895).

/// Embeds the WGSL file at `path` (relative to the invoking file, as
/// `include_str!` takes it) as a `&'static str`: the file as it is.
/// Built without debug assertions, the file's comments, indentation and
/// trailing whitespace are removed at compile time ([`wgsl`](crate::wgsl)).
#[doc(hidden)]
#[cfg(debug_assertions)]
#[macro_export]
macro_rules! embed_wgsl {
    ($path:expr) => {
        include_str!($path)
    };
}

/// Embeds the WGSL file at `path` (relative to the invoking file, as
/// `include_str!` takes it) as a `&'static str`, with its comments,
/// indentation and trailing whitespace removed at compile time
/// ([`wgsl`](crate::wgsl)). The whole text and the padded result are
/// constants only other constants read, so neither reaches the binary.
#[doc(hidden)]
#[cfg(not(debug_assertions))]
#[macro_export]
macro_rules! embed_wgsl {
    ($path:expr) => {{
        const SOURCE: &str = include_str!($path);
        const PADDED: ([u8; SOURCE.len()], usize) = $crate::wgsl::strip_padded(SOURCE);
        const STRIPPED: [u8; PADDED.1] = $crate::wgsl::prefix(&PADDED.0);
        const TEXT: &str = $crate::wgsl::as_str(&STRIPPED);
        TEXT
    }};
}

/// `source` stripped, in an array of `source`'s length `S`, and the
/// stripped length: stripping never lengthens a text. One pass, since the
/// compiler's constant evaluation is slow (about 4 µs a byte).
#[must_use]
pub const fn strip_padded<const S: usize>(source: &str) -> ([u8; S], usize) {
    let mut out = [0; S];
    let len = strip_into(source.as_bytes(), &mut out);
    (out, len)
}

/// The first `N` bytes of `padded`.
#[must_use]
pub const fn prefix<const N: usize, const S: usize>(padded: &[u8; S]) -> [u8; N] {
    let mut out = [0; N];
    let mut at = 0;
    while at < N {
        out[at] = padded[at];
        at += 1;
    }
    out
}

/// Stripped bytes as text. Stripping removes or keeps whole characters, so
/// the bytes are UTF-8.
#[must_use]
pub const fn as_str(bytes: &'static [u8]) -> &'static str {
    match core::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => panic!("stripped WGSL is not UTF-8"),
    }
}

/// `source` stripped, as a release build embeds it: for tests that compare
/// a program with and without stripping.
#[must_use]
pub fn stripped(source: &str) -> String {
    let mut out = vec![0; source.len()];
    let len = strip_into(source.as_bytes(), &mut out);
    out.truncate(len);
    String::from_utf8(out).expect("stripped WGSL is UTF-8")
}

/// Writes `source` stripped into `out`, at least as long, and returns the
/// length written. Written for the compiler's constant evaluation: one
/// pass, no calls, and the next byte read only after a `/` or `*`.
const fn strip_into(source: &[u8], out: &mut [u8]) -> usize {
    let len = source.len();
    let mut written = 0;
    // Nesting depth of the block comment the scan is in.
    let mut depth = 0usize;
    // Whether the line has written a token's byte.
    let mut line_has_content = false;
    // The run of blankspace and comments since the line's last token's byte
    // (or its start): where it began, and whether it holds a comment.
    let mut in_gap = false;
    let mut gap_start = 0;
    let mut gap_comment = false;
    let mut i = 0;
    while i < len {
        let byte = source[i];
        if depth > 0 {
            // naga pairs `/*` and `*/` left to right, each pair consumed
            // whole, so `/*/` opens and `**/` closes once.
            if byte == b'*' {
                if i + 1 < len && source[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                    continue;
                }
            } else if byte == b'/' {
                if i + 1 < len && source[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                    continue;
                }
            } else if byte == b'\n' {
                out[written] = b'\n';
                written += 1;
                line_has_content = false;
                in_gap = false;
            }
            i += 1;
            continue;
        }
        match byte {
            b'\n' => {
                out[written] = b'\n';
                written += 1;
                line_has_content = false;
                in_gap = false;
                i += 1;
            }
            b' ' | b'\t' | b'\r' => {
                if !in_gap {
                    in_gap = true;
                    gap_start = i;
                    gap_comment = false;
                }
                i += 1;
            }
            b'/' if i + 1 < len && (source[i + 1] == b'/' || source[i + 1] == b'*') => {
                if !in_gap {
                    in_gap = true;
                    gap_start = i;
                }
                gap_comment = true;
                if source[i + 1] == b'*' {
                    depth = 1;
                    i += 2;
                } else {
                    // To the first WGSL line break (naga 30's
                    // `is_comment_end`: U+000A to U+000D, U+0085, U+2028,
                    // U+2029), which the scan then reads.
                    i += 2;
                    while i < len {
                        let byte = source[i];
                        if matches!(byte, 0x0a..=0x0d)
                            || (byte == 0xc2 && i + 1 < len && source[i + 1] == 0x85)
                            || (byte == 0xe2
                                && i + 2 < len
                                && source[i + 1] == 0x80
                                && (source[i + 2] == 0xa8 || source[i + 2] == 0xa9))
                        {
                            break;
                        }
                        i += 1;
                    }
                }
            }
            // A token's byte, or blankspace this scan does not trim: kept.
            _ => {
                if in_gap && line_has_content {
                    if gap_comment {
                        out[written] = b' ';
                        written += 1;
                    } else {
                        let mut at = gap_start;
                        while at < i {
                            out[written] = source[at];
                            written += 1;
                            at += 1;
                        }
                    }
                }
                in_gap = false;
                gap_comment = false;
                line_has_content = true;
                out[written] = byte;
                written += 1;
                i += 1;
            }
        }
    }
    // Composed whole, an open comment would run on into the next module;
    // stripped, it would end with its file. Refused, so the two agree.
    assert!(depth == 0, "a WGSL block comment is not closed");
    written
}

#[cfg(test)]
mod tests;
