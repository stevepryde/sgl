# Animated Morph Cube

Khronos glTF-Sample-Assets, revision
`e124e8d8d7330a5372bc2b4d00d39a4a9d7997ea`,
`Models/AnimatedMorphCube/glTF/` and `Models/AnimatedMorphCube/glTF-Quantized/`,
unchanged, with that directory's `LICENSE.md`; `glTF-Meshopt*/` are derived
here. The model is by Microsoft,
2017, under CC0-1.0; `LICENSE.md` itself is CC-BY-4.0.

`glTF/` is the float export. `glTF-Quantized/` is the same cube written by
gltfpack 0.13 with `KHR_mesh_quantization`: UNSIGNED_SHORT positions under a
leaf node that holds the dequantisation scale and offset, normalized BYTE
normals, SHORT morph position displacements and normalized BYTE morph
normal displacements. The glTF loader's tests compare the two in world space
(`src/content/gltf/attributes_tests.rs`).

`glTF-Meshopt/` and `glTF-Meshopt-KHR/` are `glTF/` written by gltfpack 1.3
(npm `gltfpack@1.3.0`, meshoptimizer `9e1f07b`), keeping keyframes as
authored:

- `glTF-Meshopt/`: `gltfpack -i glTF/AnimatedMorphCube.gltf -o glTF-Meshopt/AnimatedMorphCube.gltf -c -ce ext -af 0`:
  gltfpack's default output, `KHR_mesh_quantization` (the same quantization
  as `glTF-Quantized/`) compressed with `EXT_meshopt_compression`, unfiltered.
- `glTF-Meshopt-KHR/`: `gltfpack -i glTF/AnimatedMorphCube.gltf -o glTF-Meshopt-KHR/AnimatedMorphCube.gltf -cc -ce khr -vpf -af 0`:
  `KHR_meshopt_compression` with version 1 attribute streams, float
  positions and morph displacements through the EXPONENTIAL filter and
  8-bit OCTAHEDRAL normals.

The tests compare both with `glTF/` (`src/content/gltf/meshopt_tests.rs`).
These files stay in the repository; the published crate does not include
them.
