# Camera geometry culling

Camera raster passes omit hidden material groups and conservatively reject
triangle ranges outside their own camera clip volume. Every camera geometry
pass (the G-buffer, its anisotropy pass, lighting, or the fused pass) draws the
camera's one draw list, so they share the range traversal. Camera rejection does not
remove geometry from scene-ray resources or shadow-caster populations.

Meshes retain their original vertex, index and primitive order. A retained
hierarchy bounds consecutive groups of at most 128 triangles. Traversal accepts
fully inside subtrees at once and merges adjacent visible ranges into a draw.
This permits section rejection inside large batched meshes without renumbering
source primitives or changing equal-depth draw ownership. Assets whose triangle
order jumps across the whole model can yield loose bounds and less rejection;
the renderer does not reorder them to improve that case.

Single-sided materials use hardware Back culling for ordinary poses and Front
culling for mirrored poses. Double-sided materials use None. CCW raster-front
semantics remain unchanged so existing shader sidedness, normal maps and inverse
transpose normals preserve their authored meaning. Runtime material edits select
the corresponding pipeline on the next draw.

`Renderer::geometry_stats` reports primary-camera submitted draw ranges and
triangles after CPU visibility, before fixed-function backface rejection. In a
diagnostics build, turning the culling layer off in `Settings::diagnostics`
restores complete mesh ranges for pixel/timing comparisons; hardware face
culling stays as above.

## Reference comparison

Authority: Three.js r185, commit
`2431a09f46f34c560bc8e44b33be0e567723d5b9`,
[`Frustum.setFromProjectionMatrix` / `intersectsBox`](https://github.com/mrdoob/three.js/blob/2431a09f46f34c560bc8e44b33be0e567723d5b9/src/math/Frustum.js)
and
[`WebGPUPipelineUtils._getPrimitiveState`](https://github.com/mrdoob/three.js/blob/2431a09f46f34c560bc8e44b33be0e567723d5b9/src/renderers/webgpu/utils/WebGPUPipelineUtils.js).

| Boundary | Native implementation and status |
| --- | --- |
| Clip inequalities | Matches WebGPU `-w <= x,y <= w`, `0 <= z <= w`; planes need no normalization because only signed half-space classification is used. |
| Box test | Center plus projected radius equals Three's furthest box corner. Minimum signed distance also permits accepting an entire inside subtree. |
| Coordinate transform | Planes include the object pose, testing original local bounds. This handles mirrored/nonuniform transforms without a world-axis expansion. |
| Precision | CPU composition uses f64. A conservative f32 error allowance derives from absolute model/view/projection products, covering the shader's separate transforms and raster jitter. Borderline ranges remain submitted. |
| Submission granularity | Native extension uses consecutive triangle ranges instead of Three's whole-object bound. Index order and vertex-pulled source IDs are unchanged. |
| Mirrored sides | Three reverses frontFace and culls Back. Native keeps CCW and culls Front; the accepted primitives agree while the existing shader corrects front-facing semantics. |
| Double sides | Both retain both sides. |

## Evidence and limits

The CPU tests independently clip actual triangles against the homogeneous clip
volume across camera/affine-transform cases. They also exercise clip-boundary
crossings and verify removal of distant sections from one large mesh. They do
not establish GPU pixel identity or a game frame-time improvement.

Run the existing mirrored-instance numerical GPU fixture and compare native
captures with the diagnostic control for the rendering boundary. Whole-game
performance remains workload-specific; report the game, build, adapter,
resolution, settings, pacing and measured times separately.
