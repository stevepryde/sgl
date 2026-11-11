# Browser performance baseline

Run from the repository root after [browser setup](../../CONTRIBUTING.md#setup):

```sh
bun scripts/tasks.ts measure-browser --baseline --frames 180 --output target/browser-baseline.json
```

This extends the existing `measure-browser` Chromium/WebGPU runner; without
`--baseline`, the voxel streaming workload remains the default. The baseline
is a release WASM build. It renders an offscreen 1280×720 target, not a DOM
canvas, through the ordinary caller-owned `Scene` / `Renderer` workflow.

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
```

`--scaled` selects `SceneResolution::ThreeQuarter` and `Upscaling::On`; this
changes the render-resolution/quality tradeoff, so report it separately from
an identical-settings renderer before/after comparison. Native FSR is not
selected.

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
presentation, input latency, mobile GPUs, or an actual consuming game's art
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
