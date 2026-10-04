# SGL3D rendering

SGL3D (`sgl-3d`) is SGL's Rust/wgpu 3D renderer, extracted from Hyperdrive
([D-12](decisions.md)) and imported by games as an ordinary library. Native
and the browser (WASM + WebGPU) are equal, first-class targets
([D-20](decisions.md)).
Games keep composition and art direction; it is not an engine. It aims for a
modern-looking, fast game, not equivalence to a reference
([D-14](decisions.md)).

This spec owns the consumer contract, the rendering rules and the roadmap.
[SGL3D architecture](sgl3d-architecture.md) owns the shape of the code,
[Architecture](architecture.md) the package boundaries, the
[package README](../crates/sgl-3d/README.md) API usage and data formats, and
[`crates/sgl-3d/docs/`](../crates/sgl-3d/docs/README.md) the features and
settings SGL3D has today.

## Requirements

1. **S3D-1 — Game ownership.** The game owns its executable, window/event loop,
   input, simulation, networking, camera policy, interpolation, world and asset
   selection, UI, settings storage and process layout. SGL3D consumes ordinary
   Rust render data on a caller-owned wgpu device and queue: native (Metal,
   Vulkan, DX12) or the browser's WebGPU, requested with the adapter's limits
   (`graphics_device::limits`). The device must support compute shaders,
   eight storage buffers per shader stage (wgpu's default limit) and 17
   sampled textures per shader stage (one above it). That rules out wgpu's
   GL and GLES backend, WebGL2 included, which lacks compute and whose
   wgpu-hal fixes `MAX_TEXTURE_SLOTS` at 16, and a WebGPU device left at the
   default limits; Metal (31 or more, 128 on macOS and Apple6 and later),
   desktop Vulkan and DX12 offer more, and so can WebGPU when the adapter's
   limits are requested ([package README](../crates/sgl-3d/README.md#browser-wasm--webgpu)).
   There is no downlevel path, and anything a device may lack beyond that is
   optional under S3D-6 (`graphics_device::features` lists what SGL3D uses
   where the adapter has it). What the browser cannot do is an input, not a
   platform fork: the game passes bytes rather than paths and the frame
   time rather than a clock, and an operation that must block for readback
   fails with an error. SGL3D must not depend on a game crate,
   require winit or input handling, or introduce an editor, ECS, gameplay
   language or application host.
2. **S3D-2 — Reusable content.** The renderer accepts caller-supplied meshes,
   materials, instances, environment maps, lights, decals and atmosphere
   inputs, and
   never identifies a game's content by a hardcoded name or path. The glTF
   loader reports unsupported visible features with the asset path rather
   than silently returning a partial model; file and embedded-byte imports share
   one decoder, which decodes each image the game does not supply and never
   reads one it does. Explicit node selection preserves ancestor transforms and rejects
   an empty selection. Games keep authored sources, export recipes and gameplay RON.
3. **S3D-3 — Coordinates and state.** The boundary uses metres, +Y up,
   right-handed view/projection matrices, camera-local forward −Z, reversed-Z
   device depth (1 at the near plane, 0 at the far plane; `sgl_3d::perspective`
   builds the infinite-far projection) and column-major matrix arrays. Camera
   and shadow-map depth both clear to 0; nearer surfaces have greater depth.
   Positions are `f32` in the scene's render frame, which the game moves by
   an exact delta (`Scene::move_origin`) to keep what it renders near the
   origin, without a static edit or a history cut
   ([render origin](sgl3d-architecture.md#scene-content)); the game's own
   world coordinates stay its own.
   SGL3D splits and fits a directional light's shadow cascades from the
   camera; the caller supplies only the shadow's distance in metres of view
   depth and its cascade count.
   Point, spot and rectangle
   light intensity is per steradian (candela; a rectangle's along its normal)
   and directional illuminance per square metre (lux), on one scale; a
   light's direction points where it shines.
   Colour inputs state their linear/sRGB interpretation; HDR lighting stays linear
   until presentation.
   Games supply final presentation poses and decide when an instance exists; the
   renderer never repeats gameplay interpolation or alters simulation state.
   Instance transforms, including nonuniform and mirrored scale, keep normals and
   front/back sides correct in every pass.
4. **S3D-4 — Resource and frame lifetime.** Static scene resources persist
   across frames, and library-owned methods keep CPU state and GPU resources in
   sync. A typed camera/frame interface owns GPU-layout packing; the caller owns
   submission and presentation. Previous camera/instance data describes the last
   submitted frame: previous camera data is renderer history, previous instance
   data is scene history. Resize, scene replacement and camera cuts invalidate
   affected history. Device-bound resources are recreated when the caller
   replaces the device, and rendering into a caller-provided texture needs no
   window. Caches become reusable only after the caller submits the frame and
   calls `Renderer::finish_frame`; an abandoned encoder leaves them dirty.
5. **S3D-5 — Coherent shading.** One physically based material and lighting
   model serves primary shading, reflections, probes, bakes and GI. Fix a wrong
   result at its cause (units, double-counted energy, broken normals, ownership
   between passes) instead of masking it with a compensating gain, clamp or
   per-content exception. Artistic parameters such as exposure, bloom, emissive
   strength, fog and reflection intensity are legitimate and game-owned.
6. **S3D-6 — Settings.** SGL3D's settings are what a game specifies about its
   rendering, as one plain value, `settings::Settings`; games persist it, own
   presets and settings UI, and decide which settings, if any, their players
   see. Library preset resolution and capability fallback never rewrite saved
   choices and report the effective result separately. A game controls what
   each image-changing feature does and how much: whether the feature is on,
   and its level: a quality tier, a choice between implementations with a real
   trade-off (AR-3, RD-3) such as the antialiasing method, a strength such as
   bloom intensity or a light's share of the fog, or a reach in metres such as
   the shadow distance. The authored look and content (S3D-2, S3D-5) stay the
   game's. That is a `Settings` field when it selects which mode or
   implementation SGL3D uses, or its quality, performance or comfort level, or
   is a diagnostics switch, otherwise a field of `FrameInput` (the per-frame
   look) or of the scene type it belongs to (content). How a feature is done
   is SGL3D's: its algorithm, kernels, thresholds, history weights and other
   internal parameters are chosen by SGL3D for each level and are never game
   fields. A game need not set any control: a setting defaults as
   `Settings::default()` sets it (the High tier where it follows the tier),
   and any other to the ported engine's as RD-2 or a recorded decision
   adjusted it, or to SGL3D's own where nothing was ported. Each type that
   holds them has `Default`, or a constructor from its required inputs as
   `FrameInput::new` takes the camera, and is not `#[non_exhaustive]`, so code
   that builds one with `..` from that default keeps compiling when a control
   is added. Correctness is not a setting: the shading model and conventions
   (S3D-3, S3D-5) have no controls, and a fix of a wrong result or a
   superseded implementation (RD-3) replaces the old behaviour without a
   control to restore it. Add a setting only for a real trade-off:
   quality/performance, a comfort need, or between implementations or modes
   (AR-3, RD-3); the top tier is the best implemented quality.
   `SceneResolution` Hd/FullHd fit within 1280×720 and 1920×1080 physical
   pixels, preserving aspect ratio without upscaling.
7. **S3D-7 — Existing consumers.** Existing 2D/browser consumers and headless
   core/net builds keep working. SGL3D builds for `wasm32-unknown-unknown` and
   renders on WebGPU in the browser lane, both in the required check
   ([testing](testing.md) 5). SGL3D exposes its math types through
   `sgl_3d::glam`; it shares the workspace glam dependency with `sgl-2d` and
   `sgl-core`, so matching math values cross packages directly. Both renderers
   also use the shared wgpu version so a game can drive them from one device.

## Rendering development

The goal is an image close to the game's concept art within the game's frame
budget. Proving equivalence to a reference implementation is not a goal.
The code's structure follows the
[architecture rules](sgl3d-architecture.md#rules).

1. **RD-1 — Foundations first.** Build in [roadmap](#roadmap) order. Do not start
   a feature before its prerequisites exist (motion vectors, TAA, depth pyramid,
   light culling), and do not build narrow special-purpose mechanisms to
   compensate for a missing foundation.
2. **RD-2 — Port proven code.** Follow what other game engines do. When a
   technique already exists in an engine with a compatible licence, port it
   instead of inventing our own; when several exist, port the best, chosen by
   comparing their quality and fit with SGL3D.
   - Port techniques, not architectures: take the shader math and pass
     structure, and write the orchestration natively in SGL3D.
   - For convention choices such as handedness, depth direction, units and
     material inputs, follow the pattern most common among modern open-source
     engines and change SGL3D to match instead of adapting each port. Adapt any remaining difference once at the port boundary.
   - Change constants, fix upstream bugs and simplify freely when it improves
     the result. Difference catalogues, line-by-line translation rules and
     equivalence proofs are not required.
   - Record provenance in the ported file's header (project, revision, original
     path). Keep the source's licence text beside the code and list it in
     `BUNDLED_RENDERING_NOTICES` in `scripts/license-notices.ts`. MIT requires
     keeping the notice; Apache-2.0 also requires carrying its NOTICE and marking
     changed files.
   - Allowed: Bevy (MIT/Apache-2.0), Wicked Engine (MIT), Godot (MIT), Three.js
     (MIT), Filament and Diligent (Apache-2.0), O3DE (Apache-2.0 or MIT), AMD
     FidelityFX and Intel XeGTAO (MIT), selfshadow/ltc_code (BSD-2-Clause), and
     MIT-licensed Unity samples such as VolumetricLighting.
     Check each file's header: engines embed code from other projects.
   - Prohibited: Unity's Graphics repository (HDRP, URP and SRP Core, under the
     Unity Companion License), Unreal Engine source, GPL/LGPL/AGPL code, The
     Forge's commercial products and unlicensed code. Papers and talks are always
     acceptable as sources of technique.
   - `sgl-post-fx` is SGL's wgpu effects library, initially derived from
     DiligentFX's SSR, TAA and shared context. It follows RD-2 and evolves for
     SGL3D, without an upstream feature-set or 1:1 conformance obligation.
     Its `PROVENANCE.md` retains source references and useful algorithm notes;
     an exhaustive difference catalogue is not required. Scene and frame
     integration belongs in SGL3D.
   - Exception: the external `sp-fidelity` (FSR2) is a 1:1 port. It diverges
     from upstream only
     where wgpu or WGSL force it, to fix a known upstream bug, or by owner
     decision, each recorded in the crate's `CONFORMANCE.md`, and contains
     nothing else that is not upstream. Integration belongs in SGL3D.
3. **RD-3 — Replace, then delete.** When a new implementation is better at
   comparable cost, remove the old path with its settings, tests, docs and
   records in the same change, and update Hyperdrive in a paired change. A
   changed bake data format regenerates the consumer's baked assets in that
   paired change. Keep an alternative only as a setting with a real
   quality/performance trade-off (S3D-6).
4. **RD-4 — Proportionate checks.** `bun scripts/tasks.ts check` stays the
   required check. Add a test only where it can fail at a real boundary: API
   behaviour, resource and history lifetime, coordinate conventions, or frame
   invariants such as finite output. Automated tests must not depend on
   interpreting images.
5. **RD-5 — Judge the look in the game.** Iterate with fixed-camera captures and
   short moving sequences from the real consumer, compared with the previous
   build and the concept art. Agents should look at captures to find and fix
   problems and describe what they see plainly, but must not declare materials,
   reflections or motion correct: the owner judges the look and files issues.
   Check temporal features in motion.
6. **RD-6 — Measure per-pass GPU time.** Performance work uses per-pass GPU
   timestamps from release builds on the consumer's moving route, reported as
   median and p95 with resolution and settings. Wall-clock completion time is
   not GPU time. Frame budgets belong to the game.
7. **RD-7 — Lean records.** Specs hold API contracts and decisions. Do not add
   design records, equations, evidence tables or measurement logs to specs or
   docs; usage belongs in the package README and provenance in source headers.
   Bugs and follow-ups go in GitHub issues. Existing component records may be
   deleted with the mechanism they describe.

## Roadmap

The status below reflects the `0.1.0` public baseline. Parenthesized numbers
are stable roadmap labels, not GitHub issue numbers. Keep this status current
when a roadmap feature lands. The
[SGL project](https://github.com/users/stevepryde/projects/12) owns priority
and status; link implementation work and follow-ups from
[public issues](https://github.com/stevepryde/sgl/issues). The
[architecture](sgl3d-architecture.md#designs-that-span-stages) owns the designs
that span stages; the [feature guide](../crates/sgl-3d/docs/features.md) owns
current capabilities and limits.

### Implemented

- **Structure (23):** retained `Scene`, frame-owning `Renderer`, and the
  [layered stage architecture](sgl3d-architecture.md#shape).
- **Lighting and shadows:** clustered point and spot lights (5), cascaded
  directional shadows (15), cached local-light shadow atlas (14), and
  rectangular area lights (16). See [lights](../crates/sgl-3d/README.md#point-spot-and-rectangle-lights)
  and [shadows](../crates/sgl-3d/README.md#local-light-shadows).
- **Image (6):** tone mapping, exposure, colour grading, and bloom.
  See [image controls](../crates/sgl-3d/README.md#exposure-bloom-and-colour-grading).
- **Content:** alpha-masked and blended materials (17), skinned meshes and
  morph targets (18), instanced draws (19), compressed material textures (20),
  and decals (21). See [content support and limits](../crates/sgl-3d/docs/features.md#content).
- **Effects:** motion blur (9) and volumetric fog with light shafts (10).
  See [motion blur](../crates/sgl-3d/README.md#motion-blur) and
  [fog](../crates/sgl-3d/README.md#volumetric-fog).

### Planned

Remaining work, in the existing roadmap order:

1. **Dynamic diffuse GI (11,
   [#21](https://github.com/stevepryde/sgl/issues/21)).** Current diffuse GI
   uses game-authored baked lightmaps, irradiance atlases, and ambient cubes.
2. **DLSS and MetalFX upscaling (12,
   [#22](https://github.com/stevepryde/sgl/issues/22)).** Current antialiasing
   choices are TAA, SMAA, and FSR2; FSR2 requires native device features and
   falls back to TAA in the browser.
3. **Hardware ray-traced reflections and shadows (13,
   [#23](https://github.com/stevepryde/sgl/issues/23)).** Current world-space
   reflections traverse a software BVH; they do not use hardware ray tracing.
4. **GPU-driven culling and occlusion culling (22,
   [#24](https://github.com/stevepryde/sgl/issues/24)).** Current visibility
   uses CPU frustum/mesh-section culling, authored mesh LOD, and instanced
   draws.

These are planned capabilities, not APIs a game can depend on yet. Implement
them under RD-1 and the architecture rules, retaining native and browser
support with explicit capability fallbacks where required.

## Acceptance boundaries

- A game and the examples render caller-created content through `sgl-3d`
  without game-specific code or a duplicated renderer, natively and on the
  browser's WebGPU (`browser_smoke`).
- The required check passes, and the real consumer exercises creation, updates,
  resize, submission and presentation after each integrated change.
