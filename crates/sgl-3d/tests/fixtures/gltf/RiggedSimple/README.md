# Rigged Simple

Khronos glTF-Sample-Assets, revision
`e124e8d8d7330a5372bc2b4d00d39a4a9d7997ea`, `Models/RiggedSimple/glTF/`,
unchanged in `glTF/`, with that directory's `LICENSE.md`. The model is
© 2017 Cesium, under CC-BY-4.0.

The other directories are that `glTF/` written by gltfpack 1.3 (npm
`gltfpack@1.3.0`, meshoptimizer `9e1f07b`), keeping keyframes as authored:

- `glTF-Meshopt-KHR/`: `gltfpack -i glTF/RiggedSimple.gltf -o glTF-Meshopt-KHR/RiggedSimple.gltf -cc -ce khr -af 0`:
  `KHR_meshopt_compression` with version 1 attribute streams, 8-bit
  OCTAHEDRAL normals and 12-bit QUATERNION rotation keys, over
  `KHR_mesh_quantization`.
- `glTF-Meshopt-Fallback/`: `gltfpack -i glTF/RiggedSimple.gltf -o glTF-Meshopt-Fallback/RiggedSimple.gltf -c -cf -ce ext -af 0`:
  `EXT_meshopt_compression` used but not required, with an uncompressed
  fallback buffer. The fallback file, `RiggedSimple.fallback.bin`, is
  deliberately not kept: a loader that supports the extension never reads it.

The glTF loader's tests compare each with `glTF/`
(`src/content/gltf/meshopt_tests.rs`). These files stay in the repository;
the published crate does not include them.
