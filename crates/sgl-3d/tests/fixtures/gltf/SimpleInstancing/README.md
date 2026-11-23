# Simple Instancing

Khronos glTF-Sample-Assets, revision
`e124e8d8d7330a5372bc2b4d00d39a4a9d7997ea`, `Models/SimpleInstancing/glTF/`,
unchanged, with that directory's `LICENSE.md`; the other directories are
derived here. The model is by Marco Hutter, 2023, under CC0-1.0;
`LICENSE.md` itself is CC-BY-4.0.

`glTF/` is one unit cube instanced 125 times by `EXT_mesh_gpu_instancing`
(used, not required): FLOAT translations, rotations and scales in a 5×5×5
grid.

- `glTF-Expanded/`: the same scene as ordinary nodes, by Khronos glTF
  Blender I/O in Blender 5.2.1 (`expand.py`): its importer reads each
  instance as an object, and its exporter, with GPU instances off, writes
  each as a node.
- `glTF-Meshopt/`: `glTF/` written by gltfpack 1.3 (npm `gltfpack@1.3.0`,
  meshoptimizer `9e1f07b`):
  `gltfpack -i glTF/SimpleInstancing.gltf -o glTF-Meshopt/SimpleInstancing.gltf -mi -cc`.
  `EXT_meshopt_compression` with the instance accessors in compressed
  views: translations and scales through the EXPONENTIAL filter,
  rotations through the QUATERNION filter as normalized SHORT, and
  `KHR_mesh_quantization` positions whose dequantisation gltfpack folds
  into the instance transforms. The extensions are required.

Regenerate `glTF-Expanded/` from this directory with
`Blender --background --factory-startup --python expand.py`, then run the
gltfpack command. The glTF loader's tests compare each instanced file with
`glTF-Expanded/` (`src/content/gltf/instancing_tests.rs`). These files stay
in the repository; the published crate does not include them.
