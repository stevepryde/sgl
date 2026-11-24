# Avocado

Khronos glTF-Sample-Assets, revision
`e124e8d8d7330a5372bc2b4d00d39a4a9d7997ea`, `Models/Avocado/glTF/`, with
that directory's `LICENSE.md` (headed by this attribution): the model is
under CC0-1.0; `LICENSE.md` itself is CC-BY-4.0.

`Avocado.gltf` and `Avocado.bin` are unchanged. `Avocado_normal.png` is the
published 2048² normal map resized to 512² (macOS `sips -Z 512`).
`Avocado-NoTangents.gltf` is `Avocado.gltf` with its primitive's `TANGENT`
attribute removed, reformatted. The base colour and roughness-metallic
images are left out; the tests supply them.

The mesh carries the tangents its normal map was baked on (Unity's). The
shading test loads it with them, and the stripped copy with the loader's
MikkTSpace tangents, and compares both, and the frame of each triangle's
coordinates, at ray hits on every triangle
(`src/scene/rays/khronos_normal_tests.rs`). These files stay in the
repository; the published crate does not include them.
