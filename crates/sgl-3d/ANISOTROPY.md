# Anisotropic material rendering

## Authority and selected model

Material parameters, texture direction/strength decoding and tangent rotation use
[`KHR_materials_anisotropy`, revision
`acfcbe65e40c53d6d3aa55a7299982bf2c01c75d`](https://github.com/KhronosGroup/glTF/blob/acfcbe65e40c53d6d3aa55a7299982bf2c01c75d/extensions/2.0/Khronos/KHR_materials_anisotropy/README.md).
Its required directional roughness is `alpha_t = mix(roughness², 1, strength²)`;
`alpha_b = roughness²`. The distribution is the anisotropic GGX equation in the
normative Anisotropy section. This does not claim that KHR's non-normative
sample lighting algorithm is implemented without changes.

SGL retains its existing Three.js 0.185.1 Fresnel, correlated Smith visibility,
DFG compensation and clearcoat conventions. The exact zero-strength regression
oracle is the immutable `pbr.wgsl` source from SGL commit `73c7508`, embedded in
`src/shading/anisotropy_tests.rs` with function names changed only. That
historical code establishes compatibility, not authority for the anisotropic
equations.

## Difference catalogue

| Boundary | Reference and local source | Consequence and status |
| --- | --- | --- |
| Directional roughness and distribution | KHR Anisotropy section; `pbr.wgsl::pbr_anisotropic_specular` | Required equations retained. Independent f64 oracle evaluates the direct reciprocal-ellipse equation, rather than the shader's rescaled vector form. |
| Smith visibility | KHR Individual lights appendix; `pbr.wgsl::pbr_anisotropic_specular` | The appendix clamps visibility to 1. SGL selects height-correlated Smith with Three's `1e-6` denominator floor and no upper visibility clamp, preserving the isotropic limit at grazing angles. This is an explicit lighting-model selection, not equivalence to the illustrative clamp. |
| Fresnel and secondary lobes | Existing Three lighting model; `pbr.wgsl` and `surface.wgsl::surface_direct_brdf` | Exponential Three Fresnel remains. Only base single-scattering specular becomes anisotropic; isotropic DFG compensation and clearcoat remain approximations/unchanged behavior. |
| Tangent frame | KHR Implementation section; `anisotropy.wgsl` and geometry inputs | The authored tangent, handedness and texture-space rotation determine the axis; the axis is projected onto the shading-normal plane. Degenerate projected directions need a deterministic fallback. BRDF tests below receive resolved axes and do not establish asset/tangent transport correctness. |
| Bent normal | KHR IBL appendix; `anisotropy.wgsl::pbr_anisotropy_bent_normal` | The view is projected perpendicular to the bitangent and blended with the normal using `(1-strength*(1-roughness))⁴`. This is the named single-sample IBL approximation, not exact anisotropic convolution. |
| Reflection roughness mix | KHR IBL appendix; `anisotropy.wgsl::pbr_anisotropy_reflection` | SGL retains Three's `roughness⁴` direction mix. The KHR example uses `roughness²`. This preserves the existing zero-strength environment path and is a documented lighting approximation. |
| Environment/reflection representations | Existing isotropic PMREM, DFG and reflection filtering | A bent reflection direction does not make isotropic prefiltering anisotropic. The diagnostic below isolates a bent-normal isotropic convolution proxy; it does not certify actual PMREM atlas sampling, split-sum, ray filtering or whole-scene output. |

## Numerical conformance checks

The real-GPU check runs in the required check; to run it alone:

```sh
cargo test -p sgl-3d --lib anisotropy_gpu_matches_independent_brdf_and_historical_zero -- --nocapture
```

The test uses 1,152 combinations: roughness 0.15/0.3/0.7, strength
0/0.00001/0.6/1, view inclination 0.12/0.87/1.565 radians, four light/view
azimuths, four anisotropy rotations, and both a canonical and an obliquely
rotated world frame. F0 is `(0.54, 0.49, 0.44)`. It checks production GPU
single-scattering against f64 GGX, nonzero direct-light replacement with/without
clearcoat, exact zero-strength direct lighting against the frozen historical
shader, and the bent-normal vector against a f64 projection.

For nonzero strength, the BRDF tolerance is `0.0005 * |reference| + 2e-6` in BRDF/output units;
the vector distance tolerance is `2e-5`. These permit f32 dot, normalization and
near-peak cancellation while remaining much smaller than changing the selected
roughness axis or clamping grazing Smith visibility. Exact historical direct
lighting has no tolerance. Compilation alone cannot establish these properties.

Observed on Apple M5 / Metal, 2026-09-25: all 1,152 cases passed. Maximum
nonzero BRDF relative error was `3.626571620e-6`; maximum bent-normal vector
distance was `7.858209909e-6`. Every zero-strength direct result matched the
frozen shader exactly. The existing zero-strength isotropic shader differs from
the f64 equation by up to `7.922321537e-4` relative in the rotated narrow-lobe
fixture because of its f32 cancellation near the peak. This existing error is
reported, not silently repaired: historical exactness governs strength zero,
and the f64 tolerance applies to the new nonzero path.

## Approximation diagnostic

The errors below were measured by a CPU diagnostic with no pass/fail
threshold, `anisotropy_rectangle_and_environment_approximation_diagnostic`.
It asserted nothing, so it is no longer a test; its source is in
`src/shading/anisotropy_tests.rs` at commit
`6cc5111358eb14a604989fd8eb9c5ac8071ad50d`. It used the four material
combinations listed below, base color
`(0.54, 0.49, 0.44)`, view inclination 0.87 and azimuth 0.61 radians, and axis
rotations 0, pi/4 and pi/2. Each setup reported 256² and 512² deterministic
midpoint samples, separating quadrature convergence from approximation bias.
All inputs, scalar radiances and outputs are linear. F0 is
`0.04*(1-metallic)+base*metallic`. Measurements isolate single-scattering
specular; unchanged diffuse and multiple-scattering contributions are excluded,
and clearcoat is zero. Thus even the actual titanium parameter row is not a
whole-pixel or whole-scene relative error.

The rectangle has dimensions 1.6 by 0.3, center `(-0.55,-0.4,1)`, faces the
origin and has unit radiance. Integration includes emitter cosine, receiver
cosine, inverse squared distance and differential area. The isotropic comparison
uses the same integration with strength zero. This measures omitted anisotropy
alone.

The environment integrates the full sphere with each lobe's own positive
receiver hemisphere. Broad sky radiance is `0.2 + 0.8 * max(L.z,0)`; the narrow
source is `0.05 + 8 * exp(80*(dot(L,direction(0.9,3.7))-1))`. It compares the
full anisotropic lobe to an exact isotropic convolution around the KHR bent
normal. That convolution is an idealized proxy, not the production PMREM kernel:
it excludes atlas discretization, the direction roughness mix, split-sum/DFG,
and reflection denoising. Its model error cannot bound total production error.

Observed signed RGB error ranges at 512² samples, over the three axis rotations:

| Roughness | Strength | Metallic | Rectangle isotropic omission | Broad sky bent proxy | Narrow source bent proxy |
| --- | --- | --- | --- | --- | --- |
| 0.30 | 0.60 | 1.00 | -2.15% to +59.91% | +17.63% to +23.32% | -95.03% to -94.61% |
| 0.45 | 0.35 | 1.00 | +6.48% to +24.85% | +14.93% to +28.14% | -92.14% to -78.69% |
| 0.55 | 0.30 | 1.00 | +5.35% to +15.46% | +10.85% to +21.63% | -76.78% to -46.12% |
| **0.25** | **0.50** | **0.80** | **-23.74% to +0.31%** | **+16.98% to +19.11%** | **-96.83% to -96.54%** |

The last row uses the selected titanium material parameters. Its rectangle
errors at axes 0/pi/4/pi/2 are approximately -23.74%/-13.92%/+0.31% in red.
The 256²-to-512² change is below 0.0003 percentage points for rectangle error,
and below 0.027 percentage points across all reported environment errors.
These observed resolution changes are convergence evidence for these fixtures,
not a formal quadrature bound or a universal material error bound.

A nearly 97% missed narrow-source specular contribution is a material
approximation limitation. The diagnostic supplies no basis to call that
approximation faithful under concentrated radiance. Reducing the authored
anisotropy to fit an unrequested error budget would change material intent.
A future reference-based anisotropic angular integration/prefilter treatment
would address this boundary; it is not implemented or claimed by these tests.

These isolated fixtures provide mathematical and numerical evidence only. They
do not establish game-material appearance, moving-frame stability, full-scene
acceptance, asset transport, or every renderer integration path; those are
judged in the game.

## Transport and integration evidence

The import tests exercise the actual glTF file/embedded decoder, including
required-extension declarations, core malformed-accessor rejection, texture-to-image
indirection, mirrored/sheared node transforms and missing/invalid tangent frames.
Active anisotropy deliberately requires authored normals and tangents rather than
deriving the glTF normal-map fallback frame. Runtime edits reject invalid values or
missing frame support without changing the observed GPU material uniform. LOD
alternatives retain frame support even before a later strength activation.

`secondary_normal_tests::anisotropic_authored_frames_mirrored_shear_and_back_faces`
executes production raster material evaluation and actual hardware scene hits.
A nonflat normal texture, an independently specified mirrored/sheared world plane,
both UV handednesses, front/back rays and 45-degree anisotropy rotation produce
independently expected mapped normals and projected world axes (maximum observed
vector differences below 2e-7 on Apple M5). Legacy derivative normal-map fixtures
remain passing. This is frame/axis transport evidence; direct BRDF response is
verified separately by the numerical GPU test above.

## Bounded next fidelity mechanism

For sky/probe specular, a [visible-normal BRDF-importance-sampled](https://pbr-book.org/4ed/Reflection_Models/Roughness_Using_Microfacet_Theory) angular integral of the same
anisotropic GGX single-scatter lobe can replace the bent-normal single lookup.
Evaluate Fresnel × visibility × distribution × cosine / PDF directly, rather
than multiplying that integral by the old DFG weight again. Keep multiscattering
and diffuse ownership separate. Sampling unfiltered radiance avoids double
convolution. A small sample count cannot guarantee discovery of narrow bright
sources; combining an environment-radiance proposal with the GGX proposal through
[PBRT fourth-edition multiple importance sampling](https://pbr-book.org/4ed/Monte_Carlo_Integration/Improving_Efficiency) is the next measured step if BRDF-only sampling
fails that diagnostic's measurement. Each sample adds a radiance lookup (and each
connected endpoint may require its own projection). This does not solve angular
screen-space coverage or hidden geometry, and screen-space rays stay isotropic. Sample counts, variance and motion
stability need evidence before choosing a persistent fidelity control. No runtime
angular integrator or new filtering quality tier is implemented in this change.
