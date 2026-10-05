# Camera and cascade culling

The camera's opaque and masked surfaces and each directional shadow cascade
draw from lists the GPU builds every frame (the architecture's GPU draw
lists): the cull stage (`src/stages/cull.rs`) tests the scene's draw
candidates, one per instance and mesh, for the view's population and level
of detail and against its clip volume, then each of the chosen mesh's
sections, and appends those that pass to their set's draw. Every camera
geometry pass (the G-buffer, its anisotropy pass, lighting, or the fused
pass) draws the camera's one list, so they share the cull. While
`Settings::occlusion_culling` runs, the camera's cull has two phases: the
early phase also sets aside what lies behind the last submitted frame's
depth pyramid at its previous pose, and the late phase, between the
G-buffer passes over the early and late sets, tests it again against this
frame's early depth; lighting draws both sets. The blended list
is built on the CPU, culled per instance by `Frustum` and walking each
mesh's range hierarchy (`MeshRanges::visible`). Camera rejection does not
remove geometry from scene-ray resources or shadow-caster populations.

Meshes retain their original vertex, index and primitive order. A retained
hierarchy bounds consecutive groups of at most 128 triangles; its leaves are
the mesh's sections, whose bounds, first index and triangle count its
section table in the ray source holds. A section is the GPU's unit: it
draws as one instance of its set's indirect draw, of 384 vertices: three
for each of its triangles, then, past them, a dummy point outside the clip
volume. The CPU traversal
accepts fully inside subtrees at once and merges adjacent visible ranges
into a draw. Either permits rejection inside large batched meshes without
renumbering source primitives. Assets whose triangle order jumps across the
whole model can yield loose bounds and less rejection; the renderer does
not reorder them to improve that case.

Single-sided materials use hardware Back culling for ordinary poses and Front
culling for mirrored poses. Double-sided materials use None. CCW raster-front
semantics remain unchanged so existing shader sidedness, normal maps and inverse
transpose normals preserve their authored meaning. Runtime material edits select
the corresponding pipeline on the next draw.

`Renderer::geometry_stats` reports the camera's submitted sections and
triangles counted on the GPU, read back a few frames later, and its blended
draws, after visibility and culling, before fixed-function backface
rejection. In a diagnostics build, turning the culling layer off in
`Settings::diagnostics` makes the camera's GPU cull accept every candidate
and section and the blended list submit complete mesh ranges, for
pixel/timing comparisons; hardware face culling stays as above.

## Reference comparison

Authority: Three.js r185, commit
`2431a09f46f34c560bc8e44b33be0e567723d5b9`,
[`Frustum.setFromProjectionMatrix` / `intersectsBox`](https://github.com/mrdoob/three.js/blob/2431a09f46f34c560bc8e44b33be0e567723d5b9/src/math/Frustum.js)
and
[`WebGPUPipelineUtils._getPrimitiveState`](https://github.com/mrdoob/three.js/blob/2431a09f46f34c560bc8e44b33be0e567723d5b9/src/renderers/webgpu/utils/WebGPUPipelineUtils.js);
the GPU's draw form and culls are Bevy 9d12036's meshlet raster
(`crates/bevy_pbr/src/meshlet/`, see `src/stages/cull.wgsl`).

| Boundary | Native implementation and status |
| --- | --- |
| Clip inequalities | Matches WebGPU `-w <= x,y <= w`, `0 <= z <= w`; planes need no normalization because only signed half-space classification is used. A cascade has no near plane. |
| Box test | Center plus projected radius equals Three's furthest box corner. Minimum signed distance also permits accepting an entire inside subtree on the CPU. |
| Coordinate transform | Planes include the object pose, testing original local bounds. This handles mirrored/nonuniform transforms without a world-axis expansion. The GPU takes the view's planes built pose-free on the CPU and applies the pose itself. |
| Precision | CPU composition uses f64. A conservative f32 error allowance derives from absolute model/view/projection products, covering the shader's separate transforms and raster jitter. The GPU, which applies the pose in f32, allows twice it. Borderline sections remain submitted. |
| Submission granularity | Native extension uses consecutive triangle ranges (sections on the GPU) instead of Three's whole-object bound. Index order and vertex-pulled source IDs are unchanged. |
| Mirrored sides | Three reverses frontFace and culls Back. Native keeps CCW and culls Front; the accepted primitives agree while the existing shader corrects front-facing semantics. |
| Double sides | Both retain both sides. |

## Evidence and limits

The CPU tests independently clip actual triangles against the homogeneous clip
volume across camera/affine-transform cases. They also exercise clip-boundary
crossings and verify removal of distant sections from one large mesh. The
cull stage's GPU tests (`src/stages/cull/tests.rs`) read back what the cull
appended: no triangle the same clip oracle keeps is dropped, at the origin
and a million metres from it; no level of detail is coarser than the CPU's
bound admits; and the views' populations match the CPU builder's. The
occlusion tests (`src/stages/cull/occlusion_tests.rs`) render whole frames:
a box behind a wall is culled and one in front kept, a first frame or a
camera cut culls nothing by occlusion, an object the wall stops hiding is
drawn (and shows) in that same frame, sections hidden inside a visible mesh
are culled and drawn again when the wall moves, and every pyramid texel lies
between the farthest depth under it and the farthest within reach of it.
They do not establish GPU pixel identity or a game frame-time improvement.

Run the existing mirrored-instance numerical GPU fixture and compare native
captures with the diagnostic control for the rendering boundary. Whole-game
performance remains workload-specific; report the game, build, adapter,
resolution, settings, pacing and measured times separately.
