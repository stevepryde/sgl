# Browser performance baseline

Run from the repository root after [browser setup](../../CONTRIBUTING.md#setup):

```sh
bun scripts/tasks.ts measure-browser --baseline --frames 180 --output target/browser-baseline.json
```

This extends the existing `measure-browser` Chromium/WebGPU runner; without
`--baseline`, the voxel streaming workload remains the default. The baseline
is a release WASM build. It renders an offscreen 1280×720 target (`--size WxH`
another), not a DOM canvas, through the ordinary caller-owned `Scene` / `Renderer` workflow.

## Workload

The deterministic textured grid has 257 instances (225 opaque and 32 blended),
197,376 triangles before
culling, 16 materials with distinct 256×256 RGBA8 textures (4 MiB source
pixels), 32 animated poses, eight local point lights, and a sun with four
shadow cascades. It uses full scene resolution, High ambient occlusion,
half-resolution screen-space reflections, TAA, and the preset's bloom and
shadows. Dynamic GI and atmosphere are off. The camera orbits at fixed
simulation time, independent of measured completion time.

After 60 warmup frames, it measures the requested frames (60–3600). It then
uploads eight additional assets in eight separate frames: each is one 256×256
texture, a 768-triangle model and one instance. This is caller-owned bounded
upload work, not a library scheduler. These upload frames do not contaminate
the steady-state distributions. The generated content needs no download,
external assets, or Bistro export.

## Optional first-pass web paths

```sh
# Same output size, 75% render dimensions and portable spatial reconstruction.
bun scripts/tasks.ts measure-browser --baseline --scaled --frames 180 --output target/browser-scaled.json

# Prepare RGBA mip chains outside frame-critical uploads.
bun scripts/tasks.ts measure-browser --baseline --prepared --frames 180 --output target/browser-prepared.json

# The eight point lights cast shadows from the first measured frame.
bun scripts/tasks.ts measure-browser --baseline --local-shadows --frames 180 --output target/browser-local-shadows.json
```

```sh
# Depth of field at a level, through a wide-open large-format lens focused
# 3 m ahead so most of the frame blurs, at another output size.
bun scripts/tasks.ts measure-browser --baseline --depth-of-field high --size 1920x1080 --frames 180 --output target/browser-dof.json
```

`--scaled` selects `SceneResolution::ThreeQuarter` and `Upscaling::On`; this
changes the render-resolution/quality tradeoff, so report it separately from
an identical-settings renderer before/after comparison. Native FSR is not
selected.

`--local-shadows` turns on the point lights' `casts_shadow` at the first
measured frame, so the warmup runs without local shadows and that frame is
the view's first to place one; `local_shadows.activation` reports its CPU
encode, completion and GPU time.

`--prepared` constructs `Image::Rgba8Mips(Rgba8MipChain::new(image)?)` before
upload/measurement. Preparation produces the separate colour/data mip chains
used by the normal upload path. The example measures its CPU cost separately.
It runs preparation on the page's thread before the loop; it does **not**
claim an implemented browser worker or a lower total preparation cost. A game
owns when and where to prepare, its worker message format, and which bounded
batch to upload this frame. Retain one or a few prepared batches rather than
preparing an unbounded world at once.

## Reading the JSON

- `startup` separates device setup, initial content CPU/upload completion,
  renderer construction and time to the first completed frame. Optional
  preparation is included in first-frame latency and reported separately.
- `cpu_encode` is wall CPU time around `Renderer::render` plus encoder finish.
  It excludes simulation, pose updates, queue completion and content uploads.
- `frame_complete_wall` includes pose updates, encoding and serialized queue
  completion. It includes browser IPC/scheduling and is **not GPU time** or
  display frame pacing. It permits one frame at a time rather than saturating
  the queue.
- `streamed_upload` reports source generation, frame-critical
  `add_asset`/`add_instance` CPU time and whole upload-frame completion. In
  prepared mode, generation/preparation happened earlier and the measured
  frame takes the retained prepared asset.
- `gpu.total` and `gpu.passes` are SGL's non-overlapping per-pass timestamp
  observations, median/p95/max in milliseconds with sample counts. Delayed
  maps are drained after rendering. Missing frames and unsupported timestamps
  are explicit; zero pass durations can reflect browser timestamp quantization
  rather than free work. Different pass percentile values are not additive.
- `resources` contains partial scene buffer observations, not total GPU memory.
  Texture and render-target allocations unavailable through this harness are
  `null`, not zero. Source texture bytes are not a VRAM estimate.
- Browser version, adapter features/limits and available adapter metadata are
  recorded. Browsers may redact the adapter name. Source and generated WASM
  hashes establish measurement provenance, not content staleness checks. A saved
  build-sidecar associates a module hash with its actual build source/mode;
  absent matching metadata, those are explicitly unknown.

Percentiles use the nearest-rank p95; median is the lower middle sample for
an even count. No performance threshold or screenshot interpretation is a
pass/fail oracle. The fixture checks that its camera view preserves unit
basis lengths; inspect a real capture before interpreting timings so a
malformed camera or workload does not become misleading benchmark evidence. The browser test fails on WebGPU/browser errors, empty
camera draws, inconsistent frame accounting or invalid timing distributions.

## Reproducible comparisons

Use the same machine, browser, resolution, settings, frame count and route;
close competing GPU work, and repeat each variant at least three times.
Compare run-to-run variation before calling a small difference a speedup.
First-frame timings are process/session observations, not a promise of an
uncached download, cold shader cache, or browser cold-start benchmark.

Keep copied source snapshots outside the repository so recursive test
discovery does not execute their copied tests. Keep a pre-change source
snapshot and use a **separate Cargo target directory**
for it. Reusing a target directory between copied workspaces can reuse stale
local package artifacts. Preserve each generated module directory and JSON.
`SGL_BROWSER_MEASURE_DIR` selects the generated module directory;
`CARGO_TARGET_DIR` selects build artifacts. `--skip-build` repeats a preserved
module without rebuilding; the report records its SHA-256, saved build
provenance where available, and a separately labeled current-checkout hash. Use only when the saved module matches the intended
source snapshot.

For an optional final-frame PNG, add `--capture target/browser-full.png`.
The asynchronous readback happens after all measured frames and GPU timing
collection; the page then copies those pixels to a 2D canvas for capture.
It is a diagnostic artifact, not a screenshot test or measured presentation.
The image shows the final upload frame, not a different hand-picked camera.

This baseline does not measure network delivery, file decoding, canvas
presentation or input latency ([Startup against three.js](#startup-against-threejs)
does), mobile GPUs, or an actual consuming game's art
or simulation. Use the existing streaming example for its much larger world,
and measure a game's own moving route before applying these numbers to it.

Implementation references: [browser example](examples/browser_baseline.rs),
[runner](../../scripts/tasks.ts), [shared browser harness](../../browser/measurement.ts),
[SGL timestamp semantics](../../crates/sgl-3d/src/timing.rs),
[measurement requirements](../../specs/sgl3d.md#rendering-development).

ETC2/ASTC supported-feature upload coverage belongs to the existing
[`browser_smoke` fixture](examples/browser_smoke.rs), not the RGBA performance
workload. Unsupported compression is reported explicitly; these measurements
do not establish mobile-device performance.

## Startup against three.js

A developer measurement of what a player waits for: from navigation to the
first visible world, the response to a camera input, and the first use of a
material and an effect, for SGL and for three.js r187's `WebGPURenderer` on
the same scene, browser and adapter. It is not part of `check` or a release.

```sh
bun scripts/tasks.ts measure-browser --startup --runs 5 --captures target/startup-captures --output target/startup.json
```

`--scene minimal|opening` runs one scene (both by default); `--startup-path
prepared` measures three.js with `renderer.compileAsync(scene, camera)` before
its first frame and skips SGL until #851 adds its preparation to
[`startup-sgl.html`](../../browser/startup-sgl.html); `--skip-build` reuses
the built module. On a shared machine, build first and take timings one
scene and a few runs at a time. The command prints a median
(min–max) table per scene and fails, without judging speed, if a load has a
WebGPU or page error, three.js runs any backend but WebGPU (its WebGL2
fallback would invalidate every number), the libraries report different
adapters, or First Contentful Paint precedes the first world frame.

**How.** One page harness ([`startup-harness.js`](../../browser/startup-harness.js))
calls each library's steps in the same order and timestamps them from
navigation start: SGL through the `Startup` export of this example
([`support/startup.rs`](examples/support/startup.rs)), rendering to the
page's canvas with `View::new_surface`; three.js through
[`startup-three.js`](../../browser/startup-three.js), bundled and minified
with Bun as a production page would ship it. Both draw a 1280×720 canvas at
device pixel ratio 1. The [runner](../../browser/startup.ts) generates one GLB
and layout per scene, which both libraries fetch and load, and alternates
which library goes first.

- *minimal*: one textured cube on a ground plane, sun and hemisphere light,
  no shadows, effects or antialiasing (SGL: antialiasing, bloom, sharpening,
  soft shadows and anisotropic filtering off; three.js renders directly).
- *opening*: the baseline's textured grid (16 materials with 512² PNG
  textures, 257 objects of 768 triangles, 32 blended and 32 moving, eight
  point lights, a sun with four 2048² cascades) at SGL's High defaults: TAA,
  bloom, RCAS sharpening, soft shadows, 8× anisotropic filtering and the
  neutral tone map. three.js: `RenderPipeline` with velocity MRT, `bloom`,
  `traa`, `sharpen` after `renderOutput`, `CSMShadowNode`, `NeutralToneMapping`
  and anisotropy 8.

**Phases.** `document_received`, `code_received` (Resource Timing),
`code_ready` (SGL: `init()` compiled and instantiated the WASM; three.js: its
bundle evaluated) and `first_visible_world` are times since navigation; the
rest are durations: `device` (adapter, device, canvas; three.js
`renderer.init()`), `asset_fetch`, `asset_decode` (SGL `asset::load_slice`
decodes the PNGs in WASM on the page's thread, #886; three.js `GLTFLoader`
uses `createImageBitmap`), `asset_upload` (SGL's scene adds until the queue
completes, split by `asset_upload_cpu_*`, where `materials` is mostly
CPU mip building, #884; three.js uploads inside its first frame, so this
is only its scene graph), `view_setup`,
`prepare` (prepared path only), `first_frame_encode_submit`,
`first_frame_gpu_complete_proxy` and `first_frame_presented_after_submit`.

**Evidence and proxies.** The first visible world is First Contentful Paint,
which Chromium stamps when the compositor frame holding the canvas is
presented; the page paints nothing else. A key's visible response is its
Event Timing duration, which also ends at presentation, but Chromium reports
only interactions of 16 ms or more and rounds to 8 ms: a faster one shows as
`<16`, not a number. `onSubmittedWorkDone` times (`…gpu_complete_proxy`) are
proxies for when a frame could be shown. Presentation is headless Chromium's,
so a real display's latency is not included.

**Inputs and first use.** After 30 frames the runner presses, each waited
out until its frame completes and 30 more follow: a camera turn twice;
spawning an alpha-masked, double-sided object neither scene drew before
(first material use), removing it and spawning it again (warm repeat); and on
the opening, ambient occlusion on (SGL High; three.js `ao()` with the normal
prepass its documentation uses), off and on again. Each reports its response,
event-to-submit time, GPU completion and the longest gap between animation
frames over the next 30: the stall.

**Cache state.** Each library and scene gets a new profile directory (cold:
empty disk HTTP cache, script and WASM code caches and Dawn shader cache),
then a second load in the same browser (warm: files from the disk cache,
which `transfer_bytes` shows as 0, and warm GPU process caches). Each such
browser runs in its own child process with a four-minute deadline and is
measured again, up to three times, if it stops responding. The operating
system's Metal compiler cache is shared by every application on the machine
and is not reset, so "cold" is a cold browser: a shader the machine has
never compiled can cost seconds more on either library's first frame or
first use. `delivered_bytes` is what
Chromium transferred; `gzip_bytes_not_delivered` shows what a compressing
server would send, since the local server sends files uncompressed.

**Differences that remain.** Lighting units and exposure differ, so the
images are not expected to match; light counts, types and shadows do.
three.js has no contact-hardening soft shadows (PCF), a different bloom and
temporal filter, its own GTAO and GPU-generated mips, where SGL makes mips on
the CPU; SGL uses an infinite far plane, three.js 200 m. Each library requests
its device as it normally does. SGL's module is wasm-bindgen output from a
release build without `wasm-opt`.

three.js (MIT) is a development dependency in the root `package.json` used
only by this measurement. Bun fetches it; nothing of it is copied into the
repository or a published crate. Native startup traces, such as the
Steveworld trace in #850, are a separate comparison: this measurement gives
no ratio between native and browser backends.
