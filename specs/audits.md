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

A-1 to A-12 came from the audit's reading. The rest were found afterwards, by
the scenario harness or while fixing, and are kept here because they are the
same history.

| # | Finding | Cause | Fix, and what guards it |
| --- | --- | --- | --- |
| A-1 | Motion-adaptive TAA discards all history on a surface that moves in depth (the craft a chase camera follows; a character walking towards a still camera): it is output as the raw jittered frame. | The adaptive depth test compared the previous depth with a camera-only reprojection. Filament, its source, skips the test on pixels with object motion; the port dropped that guard because SGL3D's motion is total, not object-only. Introduced by #767, unreleased. | The motion input's Z and W carry the surface's depth motion; history is kept when it matches the camera's reprojection or the surface's own previous depth. Guard: the suite's cases of a surface the camera follows and one moving along the view. [#768](https://github.com/stevepryde/sgl/issues/768) |
| A-2 | Switching a material group with `FrameInput::visibility_mask` at rest leaves the vanished geometry fading out over 15 to 25 frames. | The view-wide signal that re-arms TAA's rejection was raised from scene edits only; the mask is frame input. | The camera history holds the last submitted frame's mask, and a frame whose mask differs raises the signal. Guard: appearance and departure by mask at rest. [#769](https://github.com/stevepryde/sgl/issues/769) |
| A-3 | Under ray-traced shadows a thin lit feature against the sky is black at odd pixels, and flickers under jitter. | The half-size upsample gave sky texels, which store visibility 0, a floor weight; a receiver whose taps are all sky took 0. | The upsample weighs only texels that hold a receiver, and a pixel with none takes 1; the temporal clamp box likewise. Guard: thin features against a CPU occlusion oracle. [#771](https://github.com/stevepryde/sgl/issues/771). What taking 1 left wrong is A-25. |
| A-4 | Ray-traced shadow rays self-hit on normal-mapped surfaces under a low light, far from the render origin. | The ray origin was lifted along the mapped normal, flipped to the ray's side; Falcor, the source, lifts along the face normal. | The origin is lifted along whichever recorded normal faces the ray. Guard: a tilted normal-mapped plane under a low light. [#771](https://github.com/stevepryde/sgl/issues/771) |
| A-5 | A spot wider than 45° draws the shadow cube face behind it. | `cone_faces` tested each side plane of a face alone, which the opposite face always passes. | A face is kept when the nearest direction in its pyramid lies within the spot's outer angle. Guard: the faces expected from the faces' own views either side of each threshold, and a brute-force sample. [#772](https://github.com/stevepryde/sgl/issues/772) |
| A-6 | Crystal SSR treats surfaces as sky from a distance set by the near plane (909 m at a 1 mm near plane), not by its 10 km far plane. | DiligentFX's absolute depth epsilon, applied to SGL3D's far projection. | Background is depth at or beyond the far plane, exactly. [#773](https://github.com/stevepryde/sgl/issues/773) |
| A-7 | Crystal SSR's mask and the passes after it disagree on roughness in (0.2, 0.20196). | The mask tested full-precision roughness; later passes re-tested the 8-bit copy. | Every pass after the mask reads the mask (DFX-61, DFX-62). [#773](https://github.com/stevepryde/sgl/issues/773) |
| A-8 | World-space reflections fade in over about a second on opaque pixels a moving blended receiver uncovers. | The temporal pass paired the opaque depth with the surface's motion, which is the receiver's there, and kept history under it. | No history is kept under a receiver, so an uncovered surface fails the disocclusion test and takes that frame's rays; `world_under_receiver` is the one test the classification and the temporal pass share. Guard: uncovered texels equal a run that never had the receiver and restarted. [#774](https://github.com/stevepryde/sgl/issues/774) |
| A-9 | SMAA's two scene-size targets stay allocated in renderers that never run SMAA. | Created in the constructor, against the post stage's rule that full-size targets live only for their active consumer. | Created on the first frame SMAA resolves; released when it stops, and on a resize. Guard: the targets' presence across TAA, SMAA and a resize. [#775](https://github.com/stevepryde/sgl/issues/775) |
| A-10 | With SMAA or no antialiasing, an over-range pixel that a backend stores as infinity presents black, or spreads through bloom. | #464 made TAA, SSR and the reflection resolves safe against an infinite texel; bloom, the tone map, motion blur and heat were not covered. Backend-dependent; not observed on Metal, which saturates. | Every reader after composition takes scene colour through `finite_scene_color` (at most 65504): the Scene colour contract. Guard: raw +infinity bits through each pass. [#776](https://github.com/stevepryde/sgl/issues/776) |
| A-11 | Heat distortion is invisible to FSR: the pixels it displaces keep static motion and empty masks. | The heat pass ran after the masks were written and wrote neither. | It writes its weight, where it lies in front of the opaque depth, into the transparency and composition mask (AMD's for shading that motion vectors do not follow), under the one condition every mask writer reads. Guard: mask regions taken from the scene's geometry. [#777](https://github.com/stevepryde/sgl/issues/777) |
| A-12 | Velvet's roughness cutoff has three unlinked copies (AR-2); nothing rendered wrongly yet. | The trace and the temporal pass hard-coded the value composition takes from a Rust constant. | One WGSL module holds it; the layout test ties it to the Rust constant. [#778](https://github.com/stevepryde/sgl/issues/778) |
| A-13 | Under motion-adaptive TAA every outline against the sky or a far surface flickers at rest, and far sub-pixel detail does not settle. | Filament's sky override gave full rejection to all depth beyond about `near / 0.001` metres, the sky included, which jitter flips at an outline. | Depth's magnitude no longer selects the strength; the sky is a categorical disocclusion where a whole neighbourhood changes to or from it. Guard: resting stability at outlines against the sky and a far wall (0.076 to 0.098, down to 0.012, against a bound of 0.039). [#791](https://github.com/stevepryde/sgl/issues/791) |
| A-14 | A surface a vertex function, skin or morph target starts moving from rest at over about a pixel a frame leaves a ghost on its old outline for several frames. | Its previous depth neighbourhood already holds what it uncovers, and the 3x3 closest-motion search does not reach it; no rigid signal fires. | TAA reads the largest motion in each pixel's tile neighbourhood (motion blur's reduction, run on the frame's motion) and arms the hold where it is a pixel a frame or more. Guard: a morph-target departure at 1.5 px a frame, which marks no mask. [#802](https://github.com/stevepryde/sgl/issues/802), closed by [#795](https://github.com/stevepryde/sgl/issues/795) |
| A-15 | An over-range lit colour that a backend stores as infinity composes to NaN under opaque fog, and to black where occlusion hides an ambient light that is infinite too. | Reflection source completion read lit colour and ambient light unbounded: the fog's `c * T` with T = 0 is NaN, as is the ambient split's `light - light * share`. Backend-dependent. | Completion reads both through `finite_scene_color`. Guard: 18 cases of raw +infinity; finite frames bit-identical. [#801](https://github.com/stevepryde/sgl/issues/801) |
| A-16 | A rough, noisy world-space reflection fades up to 7% toward its fallback over the two opaque pixels beside a blended receiver's edge. | The upsample's blur skipped only neighbours whose opaque surface would not trace; one under a receiver traces but holds no rays. | The blur skips a neighbour under a receiver. Guard: pixels beside a receiver equal the floor without it. [#806](https://github.com/stevepryde/sgl/issues/806) |
| A-17 | A world-space reflection beside an untraced surface or a blended receiver fades toward its fallback along a line one or two pixels wide: 25% of the nearest pixel's reflection on an even edge, 75% and then 25% on an odd one. | The upsample read the half-resolution grid by bilinear filtering, as Wicked's does, so a texel that holds no rays entered at its full weight with its empty colour. | Wicked's hardware read where all four texels hold rays, as the temporal pass records them; otherwise Godot's weighted, renormalised read. Guard: three pass tests; pixels four or more from a boundary bit-identical. [#811](https://github.com/stevepryde/sgl/issues/811) |
| A-18 | Along every silhouette and crease a world-space reflection takes the other surface's: the farther surface's pixels beside a nearer one take up to 72% of its reflection, over one to several pixels, on every frame. | World-space rays trace at half resolution; the resolve's reuse took any neighbour of depth above 0 and the upsample read its four texels by the filter, as Wicked's do, so both mixed texels judged on different surfaces. | The resolve weighs reused rays by Godot's depth term; the upsample reads by Godot's half-resolution weights in the windows the temporal pass marks as holding an edge, where a neighbour falls below one half with depth taken on its departure from a plane, so a plane at any slope keeps the hardware's read; one function owns the weight. Godot weighs every read, which cost 75% here. Guard: each side of a silhouette, a crease and a silhouette at every workgroup phase equals each surface alone; a steep plane reads as a level one. [#815](https://github.com/stevepryde/sgl/issues/815) |
| A-19 | Under FSR2 the frame's bottom-left corner pixel is 14% dark on some jitter phases. | AMD's FSR2 loads its 3x3 taps unclamped and weighs off-screen taps, which load zero, into the rectification box. Found by the harness. | In the port, `sp-fidelity` 0.3.1 (SDK-P35): the box weight takes FSR3's on-screen factor. Guard: FSR2's radiance bound in every scenario; the port's oracle. [#793](https://github.com/stevepryde/sgl/issues/793) |
| A-20 | FSR2 at Ultra Performance writes negative values in the column the camera turns towards. | AMD's FSR2 leaves the upsampled colour unnormalised where the on-screen Lanczos weights sum to almost nothing, and only the kernel's negative lobe remains. Found by the FSR upscaling suite. | `sp-fidelity` 0.3.2 (SDK-P36): the rectification box's centre there, as FSR3 takes. Guard: the bound while turning at Ultra Performance. [#809](https://github.com/stevepryde/sgl/issues/809) |
| A-21 | FSR2's edge pixel dips 11% where a brighter surface meets the frame's edge. | AMD's FSR2 loads taps off the frame as zero, which widens the deringing bounds, so a Lanczos undershoot is not clamped there. | `sp-fidelity` 0.3.2 (SDK-P37): clamped loads, as AMD's Xbox path of the same function. Guard: FSR2's bound as a card leaves the frame at 4 px a frame. [#808](https://github.com/stevepryde/sgl/issues/808) |
| A-22 | TAA writes values outside the scene's range at edges in motion (0.035 where the darkest sample is 0.05). | The history fetch's Catmull-Rom lobes overshoot, and the variance clip box extends past the neighbourhood's minimum and maximum. Found by the harness. | The clip box is intersected with Filament's min/max box, in every TAA mode. Filament's deringing of the fetch is not ported: it breaks invariance under origin moves. Guard: TAA's radiance bound in every scenario; a step edge reprojected by a fraction of a pixel. [#792](https://github.com/stevepryde/sgl/issues/792) |
| A-23 | With the camera at rest, effects, blended surfaces, mist, heat distortion and animated shading converge over tens of frames under TAA, and leave trails. | Motion-adaptive TAA relaxes wherever a pixel's own motion is zero, and such content has none. FSR was told through its masks; TAA was not. | TAA reads the two masks, now written under TAA too: in proportion while a pixel is marked, at full strength for a hold once it is not. Guard: content arriving and departing at rest, scrolling normals, the dissolves, and resting stability beside marked content. [#795](https://github.com/stevepryde/sgl/issues/795) |
| A-24 | A material edit that adds or removes drawn geometry (visibility group, alpha mode, cutoff, faces, shader) fades over 15 to 25 frames at rest. | `Scene::set_material` did not raise the view-wide signal. | It raises it when the edit changes what is drawn, by the one rule the shadow casters' cache also takes (`SurfaceMaterial::draws_other_geometry`). Guard: a state test and material edits at rest. [#798](https://github.com/stevepryde/sgl/issues/798) |
| A-25 | Under ray-traced shadows a feature one pixel wide at an odd pixel takes its neighbours' shadow: a shadowed wire against the sky reads lit. | The trace casts one ray per 2x2 block, always from its even pixel. | A texel stands for its even pixel unless that holds no receiver or the block holds a nearer feature that every even pixel beside it misses; the choice is recorded once and read by every pass, and the upsample searches the 3x3 texels for a pixel its taps do not represent. Guard: bars, walls and slits against the CPU oracle (42 of 42 wrong, down to 0); one-surface scenes byte-identical. [#794](https://github.com/stevepryde/sgl/issues/794) |
| A-26 | With hardware ray tracing, one frame's rays through a model meet nothing: the frame its BLAS is compacted in. Seen as an intermittent test failure under load. | The compacted copy runs in the queue's own command buffer, and Metal does not order it against rays that reach the BLAS through the TLAS. | The TLAS keeps the built BLAS until the GPU reports the copy done. Guard: none deterministic; 15 failures in 1043 runs under load before, 0 in 1000 after. [#790](https://github.com/stevepryde/sgl/issues/790) |
| A-27 | The exponent-bit finiteness test is written out in four shaders (AR-2). | Copied as each stage needed it. | One owner, `shading/finite.wgsl`. [#799](https://github.com/stevepryde/sgl/issues/799) |

Each fix's evidence, with its numbers, is in the commit that landed it. None
of A-1 to A-12 could have been caught by a test of the whole frame as the
tests then stood. The scenario harness ([#770](https://github.com/stevepryde/sgl/issues/770)) changed that: it found
A-13, A-14 and A-19 to A-23, and its suites (`specs/testing.md`) now hold a
scenario for each: temporal antialiasing at rest and in motion, and FSR at
its upscaling qualities, with sharpening and under rotation ([#800](https://github.com/stevepryde/sgl/issues/800)).
[#816](https://github.com/stevepryde/sgl/issues/816) then removed each rule of the motion-adaptive resolve in turn and
added a scenario for every rule nothing caught.

### Records that were wrong

The code was right. Corrected with this log unless noted.

- The README's temporal antialiasing section predated #767 and did not describe the motion-adaptive history; corrected by A-1's fix, not here.
- The Shared contracts' statement that every history "rejects what changed by reprojection and clamping" no longer held for TAA at rest, where a shading-only change converged over tens of frames; A-23 amended it.
- `settings.md` said ambient occlusion runs at scene resolution; it runs at render resolution.
- `AutoExposure`'s documentation still described Bevy's metering of near-black samples, which D-46 replaced.
- The README's receiver limits omitted `keeps_specular` and transmissive receivers; the README and `material.rs` said the unrefracted transmission fallback keeps its tint, where it passes the mean.
- The probe influence ramp was attributed to Godot, whose ramp is squared and clamped to the box.
- FSR1's RCAS comment called 0.2 stops "the FSR sample's 0.8 sharpness"; the SDK maps 0.8 to 0.4 stops, and 0.2 stops is Godot's default.

### Guards that were missing

Invariants the code relied on that no spec clause or test protected. Each is
now stated in `sgl3d-architecture.md`; what tests it is said beside it.

- FSR's `motionVectorScale` is minus the render size because motion is current minus previous UV; the raster jitter is the clip-space translation of the pixel offset reported to the resolver (the Temporal design). Tested: the FSR scenarios fail on a wrong sign, and the jitter case holds it sub-pixel and out of the motion.
- The motion target's UV is texture space, +y down (the Temporal design). Tested: the suite's motion vectors against the script.
- Cascade map sizes are powers of two, which the texel snapping relies on (the Shadows design). Tested at every `ShadowQuality` size.
- Ambient occlusion runs only under a centred reversed-Z perspective projection (Frame item 5). Stated; no test.
- The fog volume is indexed by planar view depth, never distance along the ray (the Fog design). Stated; the one blended-fog test reads the centre pixel, where the two agree.
- Cluster lookups pass render-size pixels and the clustered view's depth (Shared contracts, Lights and shadows). Stated; no test.

### Waiting on the owner

- Whether a baked chart or an ambient cube adds to the frame's ambient or replaces it. The code, the tests and the feature guide are additive; the Shared contracts' determination and its Bevy precedent read as exclusive. [#779](https://github.com/stevepryde/sgl/issues/779).
- Whether to report the ordering gap behind A-26 to wgpu. SGL works around it.

### Left as they are

Design choices and open ends, recorded so a later pass does not rediscover them.

- At rest, motion-adaptive TAA's slower feedback delays shading-only changes that mark no mask (a light switching, a flash). Filament has the same trade-off.
- The directional light's disc sampling is slightly centre-weighted for a large `angular_diameter`, as Wicked Engine's is.
- TAA frames write FSR's two mask targets. One target was measured to recover at most 0.16 ms of the blended pass at 1920x1080, for six more blended pipelines ([#812](https://github.com/stevepryde/sgl/issues/812)).
- A responsive mask's hold stays at full strength for all its frames, where the other holds ramp down: a ramp left partly covered edge pixels short of the departing bar (A-23).
- The categorical sky's arriving half has no scenario: an arriving surface's own motion re-arms the hold first ([#816](https://github.com/stevepryde/sgl/issues/816)).
- A world-reflection texel is judged by its block's first pixel while its ray is cast from the jittered one, as Wicked's is; measured within one rounding step at every phase ([#814](https://github.com/stevepryde/sgl/issues/814)).
- A one-pixel slit showing a far surface, between bars wider than the upsample's search reaches, keeps its neighbours' ray-traced shadow (A-25).
- The largest finite float is a literal in five shaders: a constant of the number format, not a formula SGL owns.
- Open: FSR3 converges on sub-pixel strips more slowly than its sampling allows in its first jitter cycle at Quality and Balanced, possibly AMD's accumulation; its bar is parked on `scenario/issue-810` ([#810](https://github.com/stevepryde/sgl/issues/810)).

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
