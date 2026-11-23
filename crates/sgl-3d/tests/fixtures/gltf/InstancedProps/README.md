# Instanced props

An L-shaped prop of two boxes, asymmetric on every axis, placed nine times
under two empties, beside an ordinary ground plane, built by `export.py` in
Blender 5.2.1 and exported twice by its glTF exporter (Khronos glTF Blender
I/O v5.2.40):

- `glTF/`: with GPU instances, so each empty is one node of
  `EXT_mesh_gpu_instancing` (used, not required) and both share the prop's
  mesh. `Grove` is moved, turned about the up axis and scaled unevenly
  (1.2, 0.9, 1.1), and its six instances are translated, rotated about
  several axes, scaled unevenly and mirrored (a negative scale on one axis,
  and on all three); `Copse` places three, one mirrored.
- `glTF-Expanded/`: without them, so each instance is an ordinary node
  under its empty: the reference.

The materials are single-sided, so a face drawn with the wrong winding is
culled. Regenerate from this directory with
`Blender --background --factory-startup --python export.py`. The glTF
loader's tests (`src/content/gltf/instancing_tests.rs`) and the
`scenario_gltf_instancing` scenarios (`tests/sgl-3d`) compare the two, and
the `gltf_instancing` example loads `glTF/`. SGL's own content, under the
repository's licence; the published crate does not include it.
