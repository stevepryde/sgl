# Normal-Tangent Test

Khronos glTF-Sample-Assets, revision
`e124e8d8d7330a5372bc2b4d00d39a4a9d7997ea`, `Models/NormalTangentTest/glTF/`,
© 2017 Analytical Graphics, Inc., mesh and textures by Ed Mackey, with that
directory's `LICENSE.md` (headed by this attribution): the model is under CC0-1.0 (its file's own
`asset.copyright` names CC-BY-4.0, which this attribution also satisfies);
`LICENSE.md` itself is CC-BY-4.0.

`NormalTangentTest0.bin` and `NormalTangentTest_Normal.png` are unchanged.
`NormalTangentTest.gltf` is reduced to what the tests read: its material
keeps its normal texture alone, and the base colour and
occlusion-roughness-metallic textures and their images (2048² PNGs, 1.4 MB)
are left out; everything else is as published, reformatted.

Each of fifteen dome-shaped cells of real geometry has a flat quad beside
it whose normal map was baked from that dome, under rotated UVs, and the
file supplies no tangents. The shading tests take the shaded normal at a
point of each quad and compare it with the dome's own interpolated vertex
normal at the same offset from its centre
(`src/scene/rays/khronos_normal_tests.rs`). These files stay in the
repository; the published crate does not include them.
