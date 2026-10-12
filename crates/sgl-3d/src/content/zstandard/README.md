# Private Zstandard decoder

Decode-only port of [zstd-rs 0.1.0](https://github.com/Avarok-Cybersecurity/zstd-rs),
from its published source at commit
`bac4c37d86e5307537c145823a106f7ed8f386d7` (`zstd-rs/` in that repository).
The original MIT and Apache-2.0 licences are retained beside the source.

This module belongs to SGL3D's content layer. Its only entry point decodes
standard Zstandard frames for KTX2 material images, with the level's checked
output budget. It supplies no external dictionary; a frame requiring one
returns the upstream missing-dictionary error. The decoder retains bounded
literal scratch space and writes history into the caller's output rather than
allocating the advertised window.

Adaptations from the published source:

- Keep the decoding dependency closure: frame/block parsing, backward bit
  reader, FSE and Huffman decoding tables, literals, sequences, execution,
  decoder state, and XXH64 frame checksums.
- Omit the compressor, parallel facade, dictionary parser and constructors,
  encoding tables/normalization, forward bit writer, frame/tree/table writers,
  encoder-only code lookup helpers, and unused dictionary error. Internal
  optional dictionary state remains in the decoding bodies; the wrapper
  always supplies `None`.
- Rewrite `crate::` paths beneath this private module and use `std` allocation
  imports in place of the upstream crate's `alloc` imports. Apply SGL's rustfmt.
- Omit upstream unit tests, including roundtrip/constant checks. SGL's content
  tests exercise real KTX2 inputs, interoperability, malformed literals and
  sequences, and refusal before oversized output allocation.

Decoding algorithms and allocation/error checks otherwise retain the upstream
implementation. Updates require code review of the imported change and the
local adaptations. All source and licences live under the packaged crate's
`src/`; published consumers need no Git checkout, path dependency, Cargo patch,
or separate decoder package.

## Reference fixture

`fixtures/http.txt` is the unchanged `tests/golden-compression/http` corpus
from [Facebook Zstandard v1.5.7](https://github.com/facebook/zstd/tree/v1.5.7),
as retained by the published zstd-rs source. It is redistributed under the
original BSD-3-Clause licence in `fixtures/LICENSE-zstd-BSD-3-Clause`.
`fixtures/http.zst` was generated offline from that corpus by the reference
C Zstandard CLI 1.5.7 with:

```sh
zstd -3 http.txt -o http.zst
```

The test compares actual decoding with the original plaintext. The fixture
contains Huffman-compressed literals, sequences with matches, and a frame
checksum; it exercises the imported entropy/execution code without using a
runtime compressor or requiring an exact encoded representation.
