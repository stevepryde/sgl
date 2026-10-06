# XeGTAO FP32 current-frame visibility

Authority: Intel GameTechDev/XeGTAO commit
[`a5b1686c7ea37788eeb3576b5be47f7c03db532c`](https://github.com/GameTechDev/XeGTAO/tree/a5b1686c7ea37788eeb3576b5be47f7c03db532c).
The original `XeGTAO.h`, `XeGTAO.hlsli`, `vaGTAO.hlsl` and `vaGTAO.cpp`
are preserved under `reference/`. They are MIT licensed, copyright 2016–2021
Intel Corporation. The WGSL adaptation retains that license.

Configuration is full-resolution FP32 (`XE_GTAO_FP32_DEPTHS`,
`XE_GTAO_USE_HALF_FLOAT_PRECISION=0`), scalar visibility, supplied normals,
NoiseIndex zero, the Hilbert lookup texture (`XE_GTAO_HILBERT_LUT_AVAILABLE`,
vaGTAO's 64×64 R16Uint table of `HilbertIndex`), and the upstream default
one final denoise pass (beta 1.2).
There is no history or temporal accumulation. Low/Medium/High/Ultra use the
upstream 1×2, 2×2, 3×3, and 9×3 slice/step counts. The physical radius is
clamped to 0.01–10000 m and multiplied by 1.457 internally.
`GTAOImGuiSettings` clamps it to at most 10000 and expects at least 0.01 (its
clamp's zero divides by zero in the falloff and sample spacing). The falloff
fraction is 0.615, sample distribution power is 2, thin occluder compensation
is zero, final power is 2.2, minimum visibility is 0.03, mip sampling offset
is 3.30, and working scale is 1.5.
The default small-radius fade, projected-normal 0.05 adjustment, pixel minimum
1.3, and denoiser leak threshold/strength 2.5/0.5 are preserved upstream choices.

## Platform difference catalogue

- HLSL becomes WGSL; FP32 precision is retained, including the upstream bitwise
  fast-square-root approximation and fast acos. Shader compiler transcendental
  rounding can differ between drivers; bitwise whole-image equality is not claimed.
- The mapped world-space base normal arrives as signed octahedral coordinates in RG of
  the RGBA16Float normal target, with the geometry coat normal in BA. The
  wrapper decodes the base normal before world-to-view rotation and
  RH-to-positive-depth Z reflection rather than loading R11G11B10 packed integers.
- Depth is reversed-Z infinite device depth (1 at near, 0 at sky). Constants
  follow `GTAOUpdateConstants`, including its handedness correction. Sky texels
  are excluded before unpack. Jitter-free depth and matching stable matrices are
  the caller's responsibility.
- `XeGTAO_PrefilterDepths16x16` writes mips 0–3 in one dispatch, an 8×8 group
  to a 16×16 tile through workgroup memory, as upstream. Mip 4 would be a fifth
  storage texture, past WebGPU's default four per stage, so a second, small
  dispatch filters it from mip 3. Each output uses the identical four children
  and weighted filter, except where a child mip is one texel wide (a target
  under 16 pixels on that axis): upstream's tile then reads scratch holding the
  filter of edge-clamped pixels past that mip, where the port clamps onto the
  child mip's last texel, as its loads clamp. There is no intermediate FP16
  conversion. Point-clamp loads replace GatherRed. Full mip dimensions use
  floor division, exactly as texture mip extents do.
- Tiny targets allocate at least 16×16 backing depth storage so all five mips
  exist; the valid viewport and sample clamp remain the actual mip dimensions.
- Explicit point-mip selection and clamped integer texture loads replace the
  point/point/point sampler. Positions still use the original snapped UVs;
  depth coordinate clamping does not change reconstructed sample XY.
- Each denoise invocation filters two horizontally adjacent pixels, as
  upstream, from twelve clamped integer loads of the 4×3 neighbourhood the
  pair shares in place of its seven gathers; cardinal/diagonal neighbors,
  symmetric edges, leak correction and summation order remain the original
  denoiser.
- One R32Uint working word holds the same 8-bit packed working visibility
  (bits 0–7) and the same packed R8 UNORM edge value (its 8-bit integer, bits
  8–15) that upstream writes to two R8 targets, so the main pass makes one
  store per pixel and the denoiser reads one word per texel it reads, where
  it read two. Main output quantizes before denoising. The final R32Uint
  output holds the visibility alone and explicitly saturates the integer to
  255, matching R8Uint typed-UAV
  conversion ([Direct3D 11.3 §3.2.3.13](https://microsoft.github.io/DirectX-Specs/d3d/archive/D3D11_3_FunctionalSpec.htm#3.2.3.13)). Consumers divide the integer by 255.
- `XeGTAO_ClampDepth` uses `#ifdef XE_GTAO_USE_HALF_FLOAT_PRECISION` even when the
  selected mode defines it as zero. Its resulting 65504 clamp is preserved.
- Optional bent normals, generated normals, debug visualizations and TAA are
  not enabled configurations; no replacement AO algorithm is introduced.

## Cost decisions

The prefilter's two dispatches (upstream's tile filter, where the port had
five full-image passes), the Hilbert lookup texture, the shared working word
and the two-pixel denoise were each measured on Hyperdrive's route against
the five-pass port (sgl#234) and kept for a lower GPU frame time with every
visibility value unchanged. Not taken:

- One prefilter dispatch for all five mips where the adapter offers five
  storage textures per stage (an AR-3 specialisation): the mip-4 dispatch
  costs next to nothing.
- Half-precision math (`XE_GTAO_USE_HALF_FLOAT_PRECISION`): upstream has no
  half path with FP32 depths, and Apple GPUs run f16 FMA at the f32 rate, so
  the gain would be registers alone, for `SHADER_F16` and a specialised
  pipeline.
- `s*s` for the sample distribution power 2 and ∓sin(n) for the low-horizon
  cosines: they changed a few pixels by 1/255 without a measurable saving.

## Predeclared numerical acceptance

These thresholds were written before candidate GPU execution. For an open plane,
mean absolute visibility error must be ≤0.025 against unoccluded visibility 1.
For a visible right-angle plane/wall junction, Ultra's mean absolute error must
be ≤0.16 and maximum absolute error ≤0.30 against a separately implemented f64
cosine-weighted hemisphere ray integral with the requested finite radius. The
oracle intersects actual planes and uses no horizon scan, depth pyramid,
XeGTAO constants, or shader formula. Ray integration uses stratified 256×256
samples. Samples omit the screen border, geometry discontinuity and exact contact
singularity. This is product-behavior evidence, not proof of HLSL conformance.
A nonmultiple-of-16 target must also execute, including its last row and column,
without invalid/omitted writes. Any failed physical tolerance must be escalated;
thresholds and algorithm constants must not be widened to pass. The same receiver
set must reject an always-unoccluded negative control against both physical
error budgets, so a missing AO pass cannot satisfy this acceptance test.

## Evidence boundary

CPU WGSL parsing/validation checks language and resource legality. Source review
compares the active authority branches and all constants above. No DXC is present
in the initial environment, so no original-HLSL executable comparison is claimed.
Independent physical tests cannot substitute for that stronger future conformance
evidence. Numerical test results are reported with implementation delivery.

## Active-branch source audit

`GTAOUpdateConstants` maps to Rust `Params` construction. `HilbertIndex` maps
to Rust `hilbert_index`, which fills the lookup texture, and
`SpatioTemporalNoise` to `noise`; `ComputeViewspacePosition` maps to
`position`; `ScreenSpaceToViewSpaceDepth` and `ClampDepth` map to
`view_depth`, `DepthMIPFilter` to `filter_depth`, and `PrefilterDepths16x16`
to `prefilter_depths` and `prefilter_depth4`. The active scalar,
supplied-normal `MainPass` branches map to `main_pass`, including
`CalculateEdges`, `PackEdges`, `FastSqrt`/`FastACos`, and `OutputWorkingTerm`,
which stores through `pack_working`.
`UnpackEdges` maps to `unpack_edges`; `AddSample` and final `Output` map to
`denoise_pixel`, which `denoise` runs for each pixel of its pair.
The signed arithmetic right shift in `FastSqrt` is retained exactly.
No active-branch mathematical deviations are known after this audit; the
intentional storage/dispatch/normal-coordinate differences are catalogued above.

The numerical GPU fixture passed with fixed thresholds: open-plane mean error
0.009444 at 129×113 and 0.005403 at 17×19; junction final mean/max error
0.089380/0.170699 and raw mean/max error 0.096570/0.237365. The always-unoccluded
negative control failed both budgets at 0.197409/0.430237. These measurements use
30 receiver points and do not establish original-HLSL executable equivalence.
