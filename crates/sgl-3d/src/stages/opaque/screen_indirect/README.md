# Screen-space indirect light

The opaque stage's near-field diffuse bounce (the architecture's
[Screen-space indirect light](../../../../../../specs/sgl3d-architecture.md#designs-that-span-stages),
[D-65](../../../../../../specs/decisions.md)): four dispatches at half the
render size, rounded up, each texel's receiver the full-size pixel at its
upper left, in `screen_indirect.wgsl`, and the kept result's layout,
similarity test and read in `src/shading/screen_indirect.wgsl`, which the
pass and the lit programs both compose.

## Sources

- **Gather.** The visibility-bitmask horizon search of Therrien, Levesque and
  Gilet, "Screen Space Indirect Lighting with Visibility Bitmask" (2023),
  ported from CDRIN SSRT3 commit
  [`b38d82140acf646ee485f9ab16a5fa84906f2231`](https://github.com/cdrinmatane/SSRT3/tree/b38d82140acf646ee485f9ab16a5fa84906f2231),
  `HDRP/Shaders/Resources/SSRTCS.compute`: `CSMain`'s slice loop,
  `HorizonSampling` and `ComputeOccludedBitfield`, at `HDRP/Scripts/SSRT_HDRP.cs`'s
  shipped defaults: 12 steps a side (`stepCount`), a constant 1 m thickness
  (`thickness`, `linearThickness` off), 32 sectors (`MAX_RAY`), depth mip
  `min((j + 1) / 2, 4)` (`mipOptimization`), one slice at Low
  (`rotationCount`) and two at High. MIT, copyright 2024 CDRIN
  (`src/LICENSE-ssrt3.txt`).
  `SSRTCS.compute` includes Unity HDRP headers: no helper from them is
  ported, nor SSRT3's denoiser, probe fallback, noise texture or
  `GIIntensity` (HDRP code or its parameters, which RD-2 excludes).
- **WGSL model.** Bevy commit `9d120361303727a66b62f31f0d053793af62417a`,
  `crates/bevy_pbr/src/ssao/ssao.wesl` (Visibility Bitmask Ambient
  Occlusion): the slice setup, `updateSectors` (`insertBits`),
  `processSample`'s front and back horizons and their sector order, and the
  Hilbert-index R2 noise (`load_noise`), whose XeGTAO form SGL3D already
  owns (`stages/opaque/xegtao_kernels.wgsl`). MIT OR Apache-2.0, used under
  MIT (`src/LICENSE-bevy.txt`).
- **Emitter facing.** Godot commit `b13043816a0f234985030ec035363a005bc86c32`,
  `servers/rendering/renderer_rd/shaders/effects/ssil.glsl` 214:
  `smoothstep(0, 0.1, -dot(sample_normal, normalize(hit_delta)))`, from the
  G-buffer normal at the sample. MIT (`src/LICENSE-godot.txt`).
- **Denoise.** XeGTAO a5b1686's edge-aware kernel (`XeGTAO_Denoise`), as
  ambient occlusion ports it, from `xegtao_kernels.wgsl`, over light and
  coverage, with edges from the receivers' depths two pixels apart. MIT,
  Intel (`../ambient_occlusion/reference/LICENSE`).
- **Accumulation.** DiligentFX f26cfe5's history cap of 16
  (`SSAO_MAX_HISTORY_LENGTH`) and its least total weight 0.01, as ambient
  occlusion's (AO-6, AO-7). Apache-2.0 (`src/LICENSE-diligent.txt`).
- **Bounce sources.** SSRTCS.compute's `HorizonSampling` point-samples
  `_LightPyramidTexture` and `_NormalPyramidTexture` at the depth's mip
  (`mipOptimization`); Godot b130438 copies the frame into a mipmapped
  texture for its SSIL (`servers/rendering/renderer_rd/effects/ss_effects.cpp`
  `allocate_last_frame_buffer`, `copy_internal_texture_to_last_frame`) and
  reads it at a coarse mip (`ssil.glsl` 211). The prefilter builds four
  half-size levels of lit colour and view-space normal, each the mean of
  the four below, in one dispatch through workgroup memory as XeGTAO's
  depth prefilter does; colour is packed shared-exponent by Bevy 9d12036's
  `crates/bevy_pbr/src/render/rgb9e5.wesl` (EXT_texture_shared_exponent).
- **Similarity.** Bevy 9d12036 `crates/bevy_solari/src/realtime/gbuffer_utils.wesl`
  `pixel_dissimilar` (38–44), evaluated in the previous view's space.

## Departures

- **Weights** ([D-65](../../../../../../specs/decisions.md)). Not the paper's
  newly covered sectors over 32 times the receiver's and emitter's cosines,
  which SSRT3 makes up for with a `GIIntensity` of 10. A sample's sector
  interval weighs the measure GTAO integrates in a slice,
  `projected_normal_length * |∫ cos(α) sin(α + γ) dα|` over the interval
  (XeGTAO's `iarc`), α the in-slice angle from the projected normal and γ its
  signed angle from the view vector, written in a form that does not cancel
  over a narrow interval (`slice_measure`), by the share of its sectors
  nothing nearer covered. Averaged over slices a hemisphere of uniform
  radiance gathers that radiance with coverage 1; where one slice's coverage
  exceeds 1 the light and coverage are scaled down together, so light never
  exceeds the brightest sample's.
- **Reach.** A sample farther from the receiver than the radius covers
  nothing and gives no light; SSRT3 has no falloff. The search projects the
  radius whole into pixels, as Bevy and XeGTAO do: SSRT3's `_HalfProjScale`
  halves it, so its search spans half its radius across the view. On the
  plane-and-wall fixture at a 1 m radius the halved search misses the wall
  beyond 0.5 m (`gathered_light_matches_an_independent_hemisphere_integral`).
- **Arc cosine.** Horizon and normal angles take WGSL's `acos`. SSRT3's
  `GTAOFastAcos` (Lagarde's polynomial) errs by up to 0.008 rad, which
  leaves a receiver's own tangent plane covering a sliver of its horizon
  off the view axis (`an_open_plane_gathers_nothing`,
  `the_sky_gives_no_light_and_covers_nothing`).
- **Spacing** ([D-91](../../../../../../specs/decisions.md)). SSRT3 steps
  `radius / (steps + 1)` from a jittered start, so no sample lies past
  `steps / (steps + 1)` of the radius. Sample `k` of `n` sits at
  `(k + noise) / n` of the projected radius plus XeGTAO's 1.3-pixel floor
  (`xegtao_sample_s`, XeGTAO a5b1686 `XeGTAO.hlsli` 420–433), which ambient
  occlusion takes too, so the last sample reaches the radius. The spacing is
  linear, as the visibility-bitmask paper's; XeGTAO and Bevy 9d12036
  (`ssao.wesl` 180–181) square it, which measured no quieter here.
- **Noise.** Bevy's Hilbert R2 noise turns the slices (`noise.x`) and jitters
  each slice's samples (`noise.y`, advanced by the golden ratio per slice,
  one value for all of a slice's samples so they stay in order outward) in
  place of SSRT3's spatial offsets, interleaved gradient noise and `rand`.
- **Bounce sources at half size.** SSRT3's light and normal pyramids start
  at the full size; SGL3D's start at half (the pass's own size), each level
  one below the depth mip it pairs with, the full-size depth mip pairing
  with the first. Colour and normal share one texel, so a sample makes one
  load. Level 0 is allocated with each side rounded up to a multiple of
  eight, so every level covers the frame exactly, and the gather and the
  prefilter take each level's size from the texture.
- **Light and coverage are kept, then read one frame late.** SSRT3 composes
  in the same frame; SGL3D has no prepass to gather before shading (D-65).
