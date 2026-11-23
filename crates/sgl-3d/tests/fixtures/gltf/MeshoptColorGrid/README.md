# Meshopt colour grid

A 3×3 grid of vertices whose positions, normals, colours and triangle-list
indices `encode.mjs` authors, compressed by meshoptimizer's own encoder
(npm `meshoptimizer@1.3.0`, MIT, not bundled) as `KHR_meshopt_compression`:
version 1 attribute streams, colours through the COLOR filter at 8 bits,
indices in the INDICES mode, and a URI-less fallback buffer. It covers what
gltfpack does not emit for a mesh SGL3D loads. Regenerate with
`bun encode.mjs MeshoptColorGrid.glb` where `meshoptimizer` 1.3.0 resolves.
The glTF loader's tests compare it with the same authored values
(`src/content/gltf/meshopt_tests.rs`). SGL's own content, under the
repository's licence; the published crate does not include it.
