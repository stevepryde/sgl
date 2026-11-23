# Animated Morph Cube

Khronos glTF-Sample-Assets, revision
`e124e8d8d7330a5372bc2b4d00d39a4a9d7997ea`,
`Models/AnimatedMorphCube/glTF/` and `Models/AnimatedMorphCube/glTF-Quantized/`,
unchanged, with that directory's `LICENSE.md`. The model is by Microsoft,
2017, under CC0-1.0; `LICENSE.md` itself is CC-BY-4.0.

`glTF/` is the float export. `glTF-Quantized/` is the same cube written by
gltfpack 0.13 with `KHR_mesh_quantization`: UNSIGNED_SHORT positions under a
leaf node that holds the dequantisation scale and offset, normalized BYTE
normals, SHORT morph position displacements and normalized BYTE morph
normal displacements. The glTF loader's tests compare the two in world space
(`src/content/gltf/attributes_tests.rs`). These files stay in the repository;
the published crate does not include them.
