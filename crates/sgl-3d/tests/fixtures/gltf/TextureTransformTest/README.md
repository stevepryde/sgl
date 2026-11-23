# Texture Transform Test

Khronos glTF-Sample-Assets, revision
`e124e8d8d7330a5372bc2b4d00d39a4a9d7997ea`,
`Models/TextureTransformTest/glTF/`, unchanged, with that directory's
`LICENSE.md`. The model is under CC0-1.0; `LICENSE.md` itself is CC-BY-4.0.

Six quads draw two textures through `KHR_texture_transform`: three offsets,
a rotation, a scale and all three at once. The rotation, scale and combined
quads carry marker quads where the arrow must point when the transform is
applied correctly, where it points without it and, for the rotation, where
it points when the rotation is turned the wrong way. The glTF loader's tests
read the transforms and check that each one takes its correct marker to the
point the arrow marks (`src/content/gltf/texture_transform_tests.rs`).
These files stay in the repository; the published crate does not include
them.
