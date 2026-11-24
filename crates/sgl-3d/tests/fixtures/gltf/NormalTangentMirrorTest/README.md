# Normal-Tangent Mirror Test

Khronos glTF-Sample-Assets, revision
`e124e8d8d7330a5372bc2b4d00d39a4a9d7997ea`,
`Models/NormalTangentMirrorTest/glTF/`, © 2017–2018 Analytical Graphics,
Inc., mesh and textures by Ed Mackey, under CC-BY-4.0, with that
directory's `LICENSE.md` (itself CC-BY-4.0, headed by this attribution).

`NormalTangentMirrorTest.bin` and `NormalTangentMirrorTest_Normal.png` are
unchanged. `NormalTangentMirrorTest.gltf` is reduced to what the tests read:
its material keeps its normal texture alone, and the base colour and
occlusion-roughness-metallic textures and their images (2048² PNGs, 1.2 MB)
are left out; everything else is as published, reformatted.

Ten dome-shaped cells of real geometry face thirty flat quads whose normal
map was baked from them, each set of three placed as the original, mirrored
in X and mirrored in Y, with authored tangents whose handedness follows the
mirroring. The shading tests compare the shaded normal at a point of each
quad with a dome's own interpolated vertex normal at the same offset from
its centre, on the derivative frame and on the authored one
(`src/scene/rays/khronos_normal_tests.rs`). These files stay in the
repository; the published crate does not include them.
