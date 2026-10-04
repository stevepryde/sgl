# Soft additive intersections

Authority: Iain Cantlay, NVIDIA **GPU Gems 3 (2007), chapter 23,
section 23.4**, [Examples 23-1 and 23-2](https://developer.nvidia.com/gpugems/gpugems3/part-iv-image-effects/chapter-23-high-speed-screen-particles).
The selected technique fades source alpha from the difference between particle
and opaque depths. This implements only soft intersections, not the chapter's
low-resolution rendering system.

## Conformance and adaptations

- The prose requires zero contribution for occluded particles. Example 23-1
  identifies larger particle depth as occluded, while Example 23-2 subtracts
  scene depth from particle depth without defining the scale sign. SGL uses
  positive camera distance and `(scene - particle) / soft_distance`, clamped to
  0..1, explicitly resolving that sign ambiguity in favor of the prose.
- Depth separation is in metres along camera -Z, reconstructed from the actual
  projection. Both ordinary perspective and orthographic cameras are supported;
  arbitrary oblique near-plane projections and reversed-Z are outside this contract.
- The original depth-buffer downsampling is omitted. SGL samples the matching
  full-resolution stable primary depth with integer texel loads. Hardware depth
  testing additionally rejects hidden fragments. The sampled attachment is
  read-only; its binding is retained with the targets and recreated on resize.
- Clear depth (1) is background, so it never introduces a fictitious far-plane
  intersection. A nonpositive fade distance selects the existing hard response.
- Caller-generated geometry/profile shading and additive blending are unchanged.
  The fade multiplies alpha once.
- Existing ScreenSpace beauty/source composition and later effect composition
  retain their ownership.

## Numerical evidence

`effects_tests::soft_intersection_metric_depth_and_clear_background` reads GPU
HDR radiance for independently positioned opaque/effect planes at 3 m and 30 m,
perspective and orthographic cameras, two target sizes, occlusion, contact,
quarter/half/full fade, explicit hard response and sky. Expected values come
from authored plane separation and source radiance, not shader reconstruction.
These fixtures do not establish whole-game moving-frame correctness or cost.
