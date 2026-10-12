//! Private, decode-only port of zstd-rs 0.1.0. See README.md for provenance.
#![forbid(unsafe_code)]

mod bits;
mod decode;
mod dict;
mod error;
mod frame;
mod fse;
mod huf;
mod xxh64;

pub(super) use error::{Error, Result};

/// Decodes standard Zstandard frames within the caller's output budget.
/// KTX2 images never supply an external dictionary.
pub(super) fn decompress(src: &[u8], max_output: usize, out: &mut Vec<u8>) -> Result<usize> {
    decode::Decompressor::new().decompress(src, None, max_output, out)
}

#[cfg(test)]
mod tests {
    use super::decompress;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Reference C-zstd's compressed literals/sequences must reconstruct its
    // independently supplied HTTP corpus, including the frame checksum.
    #[wasm_bindgen_test(unsupported = test)]
    fn reference_http_entropy_and_matches_decode_to_the_original_content() {
        let expected = include_bytes!("fixtures/http.txt");
        let mut output = Vec::new();
        decompress(
            include_bytes!("fixtures/http.zst"),
            expected.len(),
            &mut output,
        )
        .unwrap();
        assert_eq!(output, expected);
    }
}
