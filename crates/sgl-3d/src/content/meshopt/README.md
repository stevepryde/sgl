# Private meshoptimizer decoders

Decode-only port of [meshoptimizer](https://github.com/zeux/meshoptimizer)
v1.3, revision `9e1f07b159d3cb777f1c67ed31fc11fd117986f4`, by Arseny
Kapoulkine, under the MIT licence retained beside the source in
`LICENSE.md`. It is the decoder three.js r187's `GLTFLoader` uses
(`examples/jsm/libs/meshopt_decoder.module.js`, built from meshoptimizer).

This module belongs to SGL3D's content layer. `content::gltf::meshopt`
applies it to `EXT_meshopt_compression` and `KHR_meshopt_compression`
buffer views; it knows nothing of glTF.

| File | Upstream (`src/`) |
| --- | --- |
| `vertex.rs` | `vertexcodec.cpp`: `getVertexBlockSize`, `decodeBytesGroup`, `decodeBytes`, `decodeDeltas1`, `decodeVertexBlock`, `meshopt_decodeVertexBuffer` (scalar path) |
| `index.rs` | `indexcodec.cpp`: `decodeVByte`, `decodeIndex`, `meshopt_decodeIndexBuffer`, `meshopt_decodeIndexSequence` |
| `filter.rs` | `vertexfilter.cpp`: scalar `decodeFilterOct`, `decodeFilterQuat`, `decodeFilterExp`, `decodeFilterColor` |
| `tests.rs` | test vectors from `demo/tests.cpp`, and multi-block streams from upstream's encoder (`tests/fixtures/meshopt/`) |

Adaptations:

- Safe Rust (`#![forbid(unsafe_code)]`): every read of a stream is a
  checked slice access, so a malformed stream is an `Error`, never a panic
  or an out-of-bounds read. Acceptance otherwise follows upstream: the same
  header, minimum size, per-group read limit, tail and leftover-byte checks.
- Scalar only, deliberately. Upstream's SSE, AVX, NEON and Wasm SIMD paths
  need `unsafe` intrinsics (or a `simd128` browser build), and `sgl-3d`
  denies `unsafe` code. The scalar path decodes the same bytes (octahedral
  outputs may differ from upstream's SIMD filters by one unit, which both
  specifications allow) at roughly a third of upstream's SIMD speed: about
  1 GB/s of decoded data, a few milliseconds for a Sponza-sized scene, which
  is not worth `unsafe` here. The gap is a choice, not a defect.
- Attribute blocks decode straight into the output (upstream's
  `MESHOPTIMIZER_VERTEXCODEC_ZEROCOPY` layout) rather than through a
  transposed scratch block.
- Stricter than upstream where the extension specifications are: the
  triangle and sequence streams accept only version 1 (`0xe1`, `0xd1`); a
  caller caps the attribute stream's version (0 for EXT, 1 for KHR); a
  channel mode byte with mode 3 or above, or a high nibble on modes 0 and 1,
  is invalid (upstream masks it); a varint longer than five bytes is
  invalid (upstream stops reading after five); a triangle stream that reads
  an edge or vertex FIFO entry never written, or whose lookup table has
  non-zero last bytes or a 0xf nibble, is invalid (upstream decodes the
  first as its all-ones fill and does not check the table).
- Encoders, the meshlet codec, version selection and the C API are left
  out.

Updates require reviewing the imported change and these adaptations, and
updating the revision here and in `licenses/upstreams.json`.
