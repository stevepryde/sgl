# Meshopt multi-block attribute streams

Attribute streams long enough to span several blocks, which the gltfpack
fixtures do not: `stride4-v{0,1}.bin`, 1000 elements of 4 bytes, and
`stride64-v{0,1}.bin`, 300 elements of 64 bytes (128-element blocks).
`encode.mjs` authors the elements and encodes them with meshoptimizer's own
encoder (npm `meshoptimizer@1.3.0`, MIT, not bundled): version 0 at its
default level, version 1 at level 3, whose stride-64 stream uses the XOR
channel mode. Regenerate with `bun encode.mjs` where `meshoptimizer` 1.3.0
resolves. The decoder's tests author the same elements
(`src/content/meshopt/tests.rs`). SGL's own content, under the
repository's licence; the published crate does not include it.
