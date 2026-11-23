# Draco sphere

A smooth UV sphere (32 segments, 16 rings, with its UV seam along one
meridian and at the poles) and a unit cube with one shape key at weight
0.25, built by `export.py` in Blender 5.2.1 and exported by its glTF
exporter (Khronos glTF Blender I/O v5.2.40), whose Draco compression is
Google's encoder:

- `glTF/`: uncompressed, the reference.
- `glTF-Draco/`: with Draco mesh compression at the exporter's defaults
  (level 6; positions 14 bits, normals 10, UVs 12). `KHR_draco_mesh_compression`
  is required. The sphere is Edgebreaker (valence); the cube, which has a
  morph target, is sequential, the exporter keeping its triangle order.
- `glTF-Draco-Fallback/`: the sphere alone, uncompressed, written by
  gltf-pipeline 4.3.1 (npm `gltf-pipeline@4.3.1`, draco3d 1.5.9):
  `gltf-pipeline -i glTF-Sphere/DracoSphere.gltf -o glTF-Draco-Fallback/DracoSphere.gltf -s -d --draco.compressionLevel 3 --draco.uncompressedFallback`.
  Edgebreaker (standard) at gltf-pipeline's default quantization
  (positions 11 bits, normals 8, UVs 10), with the extension used but not
  required and an uncompressed fallback buffer. The fallback file,
  `DracoSphere-uncompressed.bin`, is deliberately not kept: a loader that
  supports the extension never reads it.

Regenerate from this directory with
`Blender --background --factory-startup --python export.py`, which also
writes `glTF-Sphere/` (the sphere alone, uncompressed), then run the
gltf-pipeline command and delete `glTF-Sphere/` and the fallback file.
Blender's UV sphere and Draco's output may differ by a few bytes between
runs. The glTF loader's tests compare each Draco file with `glTF/`
(`src/content/gltf/draco_tests.rs`). SGL's own content, under the
repository's licence; the published crate does not include it.
