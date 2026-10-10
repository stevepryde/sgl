# sgl-post-fx

SGL's wgpu post-processing effects, including screen-space reflections (SSR),
temporal anti-aliasing (TAA) and their shared post-effect context. Implemented
in Rust and WGSL, they were derived from
[DiligentFX](https://github.com/DiligentGraphics/DiligentFX/tree/f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b)
and include subsequent fixes and enhancements. This is not a Diligent Graphics
product.

The library evolves for SGL3D under its [rendering development rules](../../specs/sgl3d.md#rendering-development).
DiligentFX is its origin, not a promise of 1:1 equivalence or a limit on its
features. Improvements may draw on other compatible implementations while
preserving their attribution and licences. This crate stays in the SGL
workspace and owns GPU effects; SGL3D owns scene conventions and frame
integration. It has no dependency on SGL3D.

[PROVENANCE.md](PROVENANCE.md) records the source revisions and useful
algorithm and implementation notes. `vendor/` in the repository retains the
original reference files unedited; the published crate keeps only their
licences, source revisions and the blue-noise table source it reads.

- SSR is AMD's FidelityFX SSSR tracing with a confidence output and its own
  energy-preserving denoiser (spatial reconstruction, temporal accumulation,
  bilateral cleanup); `vendor/DiligentFX/PostProcess/ScreenSpaceReflection/README.md`
  describes the algorithm. Its view-space reconstruction removes a jittered
  projection's offset (DFX-15), and its spatial reconstruction averages
  tone-mapped samples as Wicked Engine's resolve does (DFX-16). Its radiance
  is premultiplied by confidence, to be composited as
  `radiance + (1 - confidence) * environment` (DFX-17). A ray more than the
  depth-buffer thickness behind a surface passes behind it, as Godot's
  hierarchical SSR traces (DFX-18). A ray stops at the viewport edge (DFX-22)
  and at the far plane (DFX-26), as AMD's hybrid traversal stops it. At full
  importance-sample bias a ray follows the mirror direction, as Godot's SSR
  traces (DFX-20). Its temporal
  pass reprojects by the reflection's virtual point as AMD's reflection
  denoiser places it, rejects a surface history far from the current
  neighbourhood as AMD's does, and clamps to Wicked Engine's 2-deviation box
  (DFX-25); a virtual point behind the previous camera finds no history
  (DFX-31), nor does a surface that was behind it, in SSR and TAA alike
  (DFX-32). Its denoiser passes run only on the 8×8 tiles with a confident
  hit within their reach, as AMD's denoiser runs only over its tile list; a
  skipped tile's histories hold zero radiance and DiligentFX's no-history
  variance (DFX-29).
- TAA accumulates a Halton-jittered frame into a history, rejecting by depth
  disocclusion and motion and clipping to the neighbourhood's variance box;
  `vendor/DiligentFX/PostProcess/TemporalAntiAliasing/README.md` describes it.
  As Godot's TAA, it rejects history gradually where motion changes between
  frames, not by speed, and clips it towards the neighbourhood mean within a
  box that narrows with speed (DFX-14). A pixel that has not moved keeps a
  longer history within the same box and is not rejected by depth, as Bevy's
  TAA treats still pixels (DFX-19).
- Host: derived from `ScreenSpaceReflection.cpp`, `TemporalAntiAliasing.cpp`,
  `PostFXContext.cpp` and `PostFXRenderTechnique.cpp`, recording into a
  caller's `wgpu::CommandEncoder` where
  Diligent records into a device context (module table in `src/lib.rs`).
- GPU: `shaders/wgsl/` mirrors the upstream shader files one WGSL module per
  HLSL file, beside SGL's own `SSR_DenoiserTiles` and
  `SSR_ComputeDenoiserTiles` (DFX-29); `src/shaders.rs` assembles a shader
  as Diligent's shader factory compiles one (`HLSLDefinitions.fxh`, the
  macros, the file, its includes).
- Structures: `src/structures.rs` holds the host halves of `CameraAttribs`,
  `ScreenSpaceReflectionAttribs` and `TemporalAntiAliasingAttribs` with the
  headers' defaults.

## Use

Most games use these effects through [SGL3D](../sgl-3d/docs/README.md), which
owns scene conventions, settings, and frame integration. Use this crate
directly only when supplying your own GPU pipeline and effect inputs.
See [Building games with SGL](../../docs/README.md) for dependency setup.

As DiligentFX's README describes, per frame:

```rust,ignore
context.prepare_resources(&device, &FrameDesc { index, width, height, .. }, post_fx_context::FeatureFlags::REVERSED_DEPTH);
ssr.prepare_resources(&device, &mut encoder, &mut context, FeatureFlags::NONE);
context.execute(&mut post_fx_context::RenderAttributes { /* depths, cameras */ });
ssr.execute(&mut screen_space_reflection::RenderAttributes { /* G-buffer, attribs */ });
// ssr.get_ssr_radiance_srv(): rgb reflected radiance, a confidence
```

TAA runs the same way, after the context:

```rust,ignore
taa.prepare_resources(&device, &mut encoder, &context, temporal_anti_aliasing::FeatureFlags::NONE, 0);
// Render the frame with TemporalAntiAliasing::get_jittered_proj_matrix(proj, taa.get_jitter_offset(0)).
taa.execute(&mut temporal_anti_aliasing::RenderAttributes { /* color, depth, motion, attribs */ });
// taa.get_accumulated_frame_srv(false, 0): the anti-aliased frame
```

The caller supplies Diligent's conventions: a left-handed camera (view space
+z forward) with a finite far plane, the depth buffer as a depth texture,
world normals in [-1, 1], NDC motion vectors (current − previous) and the
previous frame's depth. Set each `CameraAttribs`' clip planes with
`set_clip_planes(near, far)`, passing far before near for reversed-Z: SSR's
and TAA's temporal passes read the near and far planes' depths (DFX-32), and
left at `Default`'s 0 they keep no history. The SSR output composites as
`(F0 · LUT.x + LUT.y) · lerp(environment, rgb, a)`.
`RenderAttributes::pass_timestamps` optionally supplies per-pass timestamp
writes by the name of each pass's upstream debug group.
`screen_space_reflection::RenderAttributes::frame_time` is the seconds since
the previous execution, which time the transition fade instead of a clock
(DFX-24): pass the measured frame time, or create the context with
`CreateInfo::transition_duration` 0, as SGL3D does. With the default
duration of 1.0 and `frame_time` left at 0, SSR's output stays zero.
SSR discards its history by the rule DiligentFX's TAA uses:
- on its first execution,
- after skipped frame indices,
- when `RenderAttributes::reset_accumulation` asks.

TAA finds the closest motion vectors in its resolve, from the depth buffer
and motion vectors it is given (DFX-13). Both are listed in PROVENANCE.md.

## Translation conventions

The original port established these conventions, which the current shaders use:

- An HLSL matrix row is a WGSL matrix column, so constructors and `m[row][col]`
  read the same and HLSL `mul(a, b)` is WGSL `b * a`. Host matrices are glam
  `to_cols_array` of the column-vector matrix.
- WGSL has no overloading; an overload carries a type suffix
  (`IsInsideScreen_f2`, `MatrixFromRows_f3`). Function-like macros are
  functions; `#define NAME VALUE` is a `const`.
- HLSL `Load` outside a texture returns zero; every ported load goes through
  `HlslLoad*` (`DiligentCore/HLSLDefinitions.wgsl`), which does the same.
- Where WGSL forces anything else, a `// WGSL:` comment says what and why.
- Modules keep only the declarations the ported passes use, in upstream order.

[PROVENANCE.md](PROVENANCE.md) explains the existing adaptations and algorithm
changes; it is not an exhaustive conformance ledger.

## Tests

`cargo test -p sgl-post-fx` builds, binds and executes every feature
permutation of each effect (reversed depth; SSR's previous frame and half
resolution; TAA's Gaussian weighting, bicubic filter and YCoCg colour space)
on the device and checks the output stays finite. SGL3D's integration
(`crates/sgl-3d/src/view/post_fx.rs`) is tested in
`crates/sgl-3d/tests/screen_space_reflections.rs`.

## Licence

DiligentFX and DiligentCore are Copyright Diligent Graphics LLC and licensed
under the Apache License, Version 2.0 (`LICENSE.txt`, `vendor/*/License.txt`);
neither ships a NOTICE file. DFX-14 and DFX-18 port Godot Engine code under
the MIT licence (`LICENSE-godot.txt`); DFX-14's comes from Godot's TAA
resolve, based on Spartan Engine's TAA, also MIT (`LICENSE-spartan.txt`).
DFX-19 ports Bevy code under the MIT licence (`LICENSE-bevy.txt`), and
DFX-25 AMD FidelityFX Denoiser code under the MIT licence
(`LICENSE-amd-fidelityfx-denoiser.txt`). Every ported file states its origin
and that it was modified.
