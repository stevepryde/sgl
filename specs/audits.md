# Audit log

What each audit and architecture pass found, what was done about it and what
now keeps it from coming back. One terse entry per finding: the defect, its
cause, the fix and why that fix, and its guard (a spec clause, a test or a
scenario). Work is tracked in the linked issues; this file is the history that
outlives them. The [architect skill](../.claude/skills/sgl-architect/SKILL.md)
appends a section for every pass.

Entries are never rewritten once their issue is closed. A later finding about
the same code gets its own entry that refers back.

## 2026-11-17 — rendering audit

Commit `941ec7bd`. Scope: every SGL3D rendering feature and `sgl-post-fx`,
read against the specs and against the engines each was ported from, at the
revisions the ports' headers name. Nine passes: temporal antialiasing; FSR2
and FSR3.1 integration; Crystal SSR and the post-effect context; Velvet,
world-space reflections, the ray scene and probes; shadows; ambient occlusion
and diffuse GI; exposure, bloom, grading, tone mapping, SMAA, FSR1 and motion
blur; volumetric fog and the transparent stage; the shading model, lights,
clusters, decals and GPU culling. Every finding below was confirmed against the
code before it was filed.

### Defects

| # | Finding | Cause | Status |
| --- | --- | --- | --- |
| A-1 | Motion-adaptive TAA discards all history on a surface that moves in depth (the craft a chase camera follows; a character walking towards a still camera): it is output as the raw jittered frame. | The adaptive depth test compares the previous depth with a camera-only reprojection. Filament, its source, skips the test on pixels with object motion; the port dropped that guard because SGL3D's motion is total, not object-only. Introduced by #767, unreleased. | [#768](https://github.com/stevepryde/sgl/issues/768) |
| A-2 | Switching a material group with `FrameInput::visibility_mask` at rest leaves the vanished geometry fading out over 15 to 25 frames. | The view-wide signal that re-arms TAA's rejection is raised from scene edits only; the mask is frame input. | [#769](https://github.com/stevepryde/sgl/issues/769) |
| A-3 | Under ray-traced shadows a thin lit feature against the sky is black at odd pixels, and flickers under jitter. | The half-size upsample gives sky texels, which store visibility 0, a floor weight; a receiver whose taps are all sky takes 0. | [#771](https://github.com/stevepryde/sgl/issues/771) |
| A-4 | Ray-traced shadow rays self-hit on normal-mapped surfaces under a low light, far from the render origin. | The ray origin is lifted along the mapped normal, flipped to the ray's side; Falcor, the source, lifts along the face normal. | [#771](https://github.com/stevepryde/sgl/issues/771) |
| A-5 | A spot wider than 45° draws the shadow cube face behind it. | `cone_faces` tests each side plane of a face alone, which the opposite face always passes. | [#772](https://github.com/stevepryde/sgl/issues/772) |
| A-6 | Crystal SSR treats surfaces as sky from a distance set by the near plane (909 m at a 1 mm near plane), not by its 10 km far plane. | DiligentFX's absolute depth epsilon, applied to SGL3D's far projection. | [#773](https://github.com/stevepryde/sgl/issues/773) |
| A-7 | Crystal SSR's mask and the passes after it disagree on roughness in (0.2, 0.20196). | The mask tests full-precision roughness; later passes re-test the 8-bit copy. | [#773](https://github.com/stevepryde/sgl/issues/773) |
| A-8 | World-space reflections fade in over about a second on opaque pixels a moving blended receiver uncovers. | The temporal pass pairs the opaque depth with the surface's motion, which is the receiver's there, and keeps history under it. | [#774](https://github.com/stevepryde/sgl/issues/774) |
| A-9 | SMAA's two scene-size targets stay allocated in renderers that never run SMAA. | Created in the constructor, against the post stage's rule that full-size targets live only for their active consumer. | [#775](https://github.com/stevepryde/sgl/issues/775) |
| A-10 | With SMAA or no antialiasing, an over-range pixel that a backend stores as infinity presents black, or spreads through bloom. | #464 made TAA, SSR and the reflection resolves safe against an infinite texel; bloom, the tone map, motion blur and heat were not covered. Backend-dependent; not observed on Metal, which saturates. | [#776](https://github.com/stevepryde/sgl/issues/776) |
| A-11 | Heat distortion is invisible to FSR: the pixels it displaces keep static motion and empty masks. | The heat pass runs after the masks are written and writes neither. | [#777](https://github.com/stevepryde/sgl/issues/777) |
| A-12 | Velvet's roughness cutoff has three unlinked copies (AR-2); nothing renders wrongly yet. | The trace and the temporal pass hard-code the value composition takes from a Rust constant. | [#778](https://github.com/stevepryde/sgl/issues/778) |
| A-13 | Under motion-adaptive TAA every outline against the sky or a far surface flickers at rest, and far sub-pixel detail does not settle. | Filament's sky override gave full rejection to all depth beyond about `near / 0.001` metres, the sky included, which jitter flips at an outline. | [#791](https://github.com/stevepryde/sgl/issues/791) |
| A-14 | A surface a vertex function or skin starts moving from rest at over about a pixel a frame leaves a ghost on its old outline for several frames. | Its previous depth neighbourhood already holds what it uncovers, and the 3x3 closest-motion search does not reach it; no rigid signal fires. | [#802](https://github.com/stevepryde/sgl/issues/802) |
| A-15 | An over-range lit colour that a backend stores as infinity composes to NaN under opaque fog, and to black where occlusion hides an ambient light that is infinite too. | Reflection source completion read lit colour and ambient light unbounded: the fog's `c * T` with T = 0 is NaN, as is the ambient split's `light - light * share`, infinity less infinity, a NaN that occlusion's `max(0, ...)` makes 0 on Metal. Backend-dependent. | [#801](https://github.com/stevepryde/sgl/issues/801) |

Each fix, and the test that fails without it, is recorded in its issue and
pull request. No test of the whole frame could catch any of these; the
scenario harness ([#770](https://github.com/stevepryde/sgl/issues/770)) is
meant to change that.

### Records that were wrong

The code was right. Corrected with this log unless noted.

- The README's temporal antialiasing section predated #767 and did not describe the motion-adaptive history; corrected by A-1's fix, not here.
- The Shared contracts' statement that every history "rejects what changed by reprojection and clamping" no longer holds for TAA at rest, where a shading-only change converges over tens of frames.
- `settings.md` said ambient occlusion runs at scene resolution; it runs at render resolution.
- `AutoExposure`'s documentation still described Bevy's metering of near-black samples, which D-46 replaced.
- The README's receiver limits omitted `keeps_specular` and transmissive receivers; the README and `material.rs` said the unrefracted transmission fallback keeps its tint, where it passes the mean.
- The probe influence ramp was attributed to Godot, whose ramp is squared and clamped to the box.
- FSR1's RCAS comment called 0.2 stops "the FSR sample's 0.8 sharpness"; the SDK maps 0.8 to 0.4 stops, and 0.2 stops is Godot's default.

### Guards that were missing

Invariants the code relied on that no spec clause or test protected. Each is
now stated in `sgl3d-architecture.md` unless noted; the tests that would fail
belong to the harness suites that follow #770.

- FSR's `motionVectorScale` is minus the render size because motion is current minus previous UV; the raster jitter is the clip-space translation of the pixel offset reported to the resolver (the Temporal design).
- The motion target's UV is texture space, +y down; no motion blur test fails for a flipped y (the Temporal design).
- Ambient occlusion runs only under a centred reversed-Z perspective projection (Frame item 5).
- The fog volume is indexed by planar view depth, never distance along the ray; the one blended-fog test reads the centre pixel, where the two agree (the Fog design).
- Cluster lookups pass render-size pixels and the clustered view's depth (Shared contracts, Lights and shadows).
- Cascade map sizes must be powers of two for the texel snapping to be exact; not stated here: to be stated and tested with [#772](https://github.com/stevepryde/sgl/issues/772).

### Waiting on the owner

- Whether a baked chart or an ambient cube adds to the frame's ambient or replaces it. The code, the tests and the feature guide are additive; the Shared contracts' determination and its Bevy precedent read as exclusive. [#779](https://github.com/stevepryde/sgl/issues/779).

### Left as they are

Design choices, recorded so a later pass does not rediscover them.

- At rest, motion-adaptive TAA's slower feedback delays shading-only changes (a light switching, a flash). Filament has the same trade-off.
- The directional light's disc sampling is slightly centre-weighted for a large `angular_diameter`, as Wicked Engine's is.

## 2026-11-17 — web target pass

Commit `ca3c65c5`. Scope: every native-only path in the library crates, read
against D-48 (the web supports everything its platform can run): each
`target_arch` gate outside tests, each optional device feature SGL or its FSR
port requests, and each platform exception the guides record. WebGPU's
capabilities are from the specification's feature index (editor's draft of this
date) and wgpu 30.0.1's WebGPU backend (`FEATURES_MAPPING`); browser support is
from MDN's compatibility data. [#782](https://github.com/stevepryde/sgl/issues/782).

### Native-only that the web could run

- **FSR2 and FSR3.** Of the five device features the port requires, the platform lacks one: texture atomics, for which atomics in a storage buffer are an exact substitute (minimum and maximum do not depend on order). Subgroups, 16-bit normalized formats and the storage formats are in WebGPU (`subgroups`, `texture-formats-tier1`, `texture-formats-tier2`) and missing only from wgpu's browser backend; `float32-filterable` is granted. Planned in [#803](https://github.com/stevepryde/sgl/issues/803).
- **Blocking readbacks.** `diagnostics::read`, `Renderer::capture_specular_probe` and `sgl_2d`'s `read_scene` are native-only only because they block; WebGPU maps asynchronously, and the browser example already carries its own awaiting copy of the first. Planned in [#804](https://github.com/stevepryde/sgl/issues/804).
- **Blocking readbacks, closed** by [#804](https://github.com/stevepryde/sgl/issues/804): each is now an asynchronous function, its one implementation, with the blocking name a native wrapper (`diagnostics::read_async`, `Renderer::capture_specular_probe_async`, `read_scene_async`); the browser lane reads its frames through the first and captures a probe through the second.

### Records that were wrong

- The feature guide said FSR needs "native-only device features" without saying which or why, and the README's list omitted subgroups. Both now give the reason, split by whether the platform or wgpu's backend is the obstacle.
- The feature guide gave no reason for hardware ray tracing's absence in the browser (WebGPU has no ray queries).
- `sgl-net`'s README did not say what a browser peer loses by having only the WebSocket.

### Waiting on the owner

- Whether browser peers get unreliable datagrams. WebTransport's are in every major browser and fit `DatagramTransport` as it is; the cost is a QUIC and HTTP/3 server in-process. [#805](https://github.com/stevepryde/sgl/issues/805).

### Left as they are

- Hardware ray tracing: WebGPU has neither acceleration structures nor ray queries. Rays trace the software BVHs, as on any device without them.
- `asset::load` and `capture_scene` take a path: a browser has no file system. `load_slice` and `sgl-2d`'s `fetch_runtime_bundle` are the browser's ways in.
- The 1 GiB default load budget and the 2 GiB decal atlas build limit on `wasm32`: its 4 GiB address space, shared with the game's other content.
- The blocking constructors (`Gpu::headless`, `Context::new`, `try_new`) are native conveniences over asynchronous forms the browser awaits.
- The other way round, nothing was found: every optional WebGPU feature SGL3D uses natively it also requests in the browser (depth clip control, RG11B10 render targets, BC, ETC2 and ASTC, timestamp queries), and the WebGPU features it leaves unused (dual-source blending, clip distances, indirect first instance, blendable 32-bit float) it does not use natively either.
