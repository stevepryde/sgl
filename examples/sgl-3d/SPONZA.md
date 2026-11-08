# Intel Sponza with Colorful Curtains

This optional, local-only lighting workload uses Intel's modern Sponza base
scene and Colorful Curtains add-on. The repository contains the fetch/export
code and small camera/lighting recipes, never the downloaded scenes, textures,
archives, prepared content or captures. It uses the existing native `bistro`
example and generic embedded-scene browser runner; there is no Sponza path in
the library or a separate renderer.

## Bootstrap another machine

Install the repository's [development tools](../../CONTRIBUTING.md#setup),
Python 3 and curl, then run from the repository root:

```sh
bun scripts/prepare-sponza.ts
```

The script downloads the two official Intel archives, verifies the selected
SHA-256 bytes, safely extracts them and explicitly prepares one core glTF and
embedded GLB. Defaults are `.cache/sponza/` and a 512-pixel texture limit.
`.cache/` is already Git-ignored. Allow about 4.45 GiB for downloads, 7.8 GiB
for their extracted source packages, plus prepared files and build artifacts.
Rendering also needs several GiB of memory; source size is not peak memory.

```sh
# Download and retain original sources/notices, without converting.
bun scripts/prepare-sponza.ts --fetch-only
# Re-export from the downloaded cache without fetching scene archives.
bun scripts/prepare-sponza.ts --offline --max-texture 1024
# A separate local cache/export, for a different texture resolution.
bun scripts/prepare-sponza.ts --cache /absolute/local/path/sponza --max-texture 512
```

Cached archives are verified and reused. Conversion runs only when explicitly
requested; neither renderer checks source hashes nor regenerates assets.
Changing lighting settings does not require re-export. Keep custom cache paths
outside tracked source directories. A changed upstream archive fails the pin
rather than silently selecting different content. Pins identify the bytes
retrieved from Intel, not an Intel signature or a guarantee of availability.

Preparation retains geometry, authored node transforms, UVs, material bindings,
cameras and punctual lights. It combines the two scenes with remapped indices
and one binary buffer. PNG color/emission is resized in linear light with
premultiplied alpha; data maps stay linear and filtered normal vectors are
renormalized. It does not apply Bistro-specific channel corrections or reduce
geometry. The 512-pixel export retains 5,744,384 triangles in a 244.6 MiB GLB,
with 80 MiB of level-zero decoded images. Native CPU inspection reports about
407 MiB of decoded geometry and images together, before transient copies,
GPU mip chains, render targets or optional ray data. `asset-stats.json` records
the exported source/decoded-image sizes.

## Sunlit courtyard recipe

For a visual lighting pass rather than the neutral loader baseline, prepare
1024-pixel textures and the sky already included in the pinned Intel archive:

```sh
bun scripts/prepare-sponza.ts --offline --max-texture 1024
bun scripts/prepare-sponza-lighting.ts
bun scripts/tasks.ts measure-browser \
  --fixture examples/sgl-3d/fixtures/sponza-sunlit.json \
  --asset-dir .cache/sponza/prepared --view hero \
  --size 1920x1080 --frames 60 --warmup 720 \
  --capture .cache/sponza/captures/sunlit/hero.png \
  --output .cache/sponza/captures/sunlit/hero.json
```

Omit `--offline` on the first machine setup. The sky step requires the same
Chromium/WebGPU setup as the browser lane and runs the existing Three common/
WebGPU PMREM exporter. It adds `sky-environment.json`, `sky-panorama.png`,
`sky-pmrem.rgba16`, `SKY-PROCESSING.json` and retained notices, without changing
geometry or the neutral environment. Nothing runs implicitly at load time.
For a separate prepared directory, pass `--prepared /local/prepared`; for a
custom source cache, also pass `--source /local/source/kloppenheim_05_4k.hdr`.
The exporter is calibrated for this named HDR, not a general environment API.

The recipe uses a new ground-floor `hero` camera, Intel's `balcony` and
`curtains` cameras, +2.1 EV exposure, ACES filmic, TAA, high AO, high dynamic GI,
three high-quality sun-shadow cascades and 16x anisotropic filtering. Scene
resolution is full, with no upscaling. There are no added fill or practical
lights, baked lightmaps, fake volumetric shafts or reflection probes. The
1024-pixel export keeps all 5.74 million triangles and uses about 320 MiB of
level-zero decoded images before GPU mips; use 512-pixel assets with the same
recipe on smaller devices. Explicit texture export replaces prepared textures
and GLB, so choose a separate cache if preserving another resolution matters.

The sky is [Kloppenheim 05](https://polyhaven.com/a/kloppenheim_05) by Greg Zaal,
CC0, from `main_sponza/textures/kloppenheim_05_4k.hdr`. Its bright sun lobe is
removed from lighting before filtering and replaced by a shadowed directional
light. The original display panorama retains the sun. Sky and sun rotate
together by 3.58 radians; sun elevation, color and energy follow the source.
The exporter finds the source peak, caps its one-degree neighbourhood against
the surrounding sky and integrates the removed RGB over solid angle. This
estimates the hidden sky under the sun, rather than claiming an exact physical
separation. `SKY-PROCESSING.json` records the extraction, direction, integrated
energy, linear solid-angle-weighted reduction, RGBE quantization and actual
WebGPU bake metadata. The recipe's sun uses the recorded Three HDR-decoding
scale; do not add a second unshadowed sun or divide its illuminance by pi.

Sunlit and lamps use a 4,095-probe volume, from `[-18,-2,-11]` to
`[22,22,17]`, with two-metre spacing. This encloses the full source building,
including the back door at x≈19.5 and the roof at y≈18.8. An atrium-only volume
ending at x=18 leaves that covered door in the transition to unoccluded sky
fill. Coverage is therefore content correctness, not an exposure adjustment.
The extra probes increase memory and convergence work; the renderer's per-frame
ray budget remains capped. The smaller `sponza-gi.json` below is deliberately
retained as a limited-coverage baseline, not the recommended visual recipe.

The display sky is an 8-bit, clamped panorama; the lighting atlas retains HDR.
It is not an HDR sky-display pipeline or reference-renderer match. This is
static-camera evaluation: settled probe updates can be much cheaper than
cold or changing-light GI. Use a separate `--observe-gi` diagnostic run for
trace counts, and do not label its overhead as clean frame timing.

For warmer side-gallery pools, substitute `sponza-lamps.json`. It adds 22
shadow-casting point lights at the source's named lamp positions, using
explicitly authored RGB `[1, 0.6, 0.3]`, intensity 16, range 6 metres and radius
0.07 metres. These strengths are not imported source values: the source lights
all have zero intensity. The curtains and columns still occlude the light.
Local-shadow population and steady-state atlas work appear in the reports.
This adds lighting cost even once static shadow faces are cached; compare it
with the sun-only recipe at the same camera and resolution.

For native verification, use the existing `bistro` command below with
`sponza-sunlit.json`, `--view hero`, `--size 1920x1080`, and
`--frames 780 --warmup 720`. Native `--frames` counts total frames; the browser
argument counts measured frames. Both use the same fixture and asset.

## Covered-gallery reflections

GI replaces indirect diffuse light, not the outdoor environment's specular
reflection. For the deep covered doorway, explicitly capture the local room
with the existing native reflection-probe authoring path:

```sh
# Run after scene and source-sky preparation. Requires native GPU access.
bun scripts/prepare-sponza-gallery.ts
bun scripts/tasks.ts measure-browser \
  --fixture .cache/sponza/prepared/sponza-gallery.json \
  --asset-dir .cache/sponza/prepared --view hero \
  --size 1920x1080 --frames 60 --warmup 720 \
  --capture .cache/sponza/captures/gallery/hero.png \
  --output .cache/sponza/captures/gallery/hero.json
```

Pass `--prepared /local/prepared` to use another prepared directory. The script
captures a 64-pixel-per-face HDR probe at `[16.5,2,0]`, writes its 262,128-byte
filtered payload and capture metadata into the ignored cache, and emits the
small derived `sponza-gallery.json` recipe. It leaves source assets, the sky,
and checked-in recipes unchanged. The source placement is
[`sponza-gallery-probe.json`](fixtures/sponza-gallery-probe.json). Its influence
fully covers the back door, with a blend toward outdoor reflections at the
entrance. The proxy box approximates that gallery's enclosure, not the whole
building. No materials, exposure or light intensities are changed.

This is authored static reflection content, not live ray-traced specular
occlusion. Re-run the command deliberately after relevant scene or lighting
changes; no runtime fingerprint, staleness rejection or automatic rebake is
added. The capture uses the sunlit recipe, so do not silently reuse it as a
calibrated bake for a different lamp rig. Check other receiver views and blend
boundaries when changing its placement. The sunlit recipe without this probe
remains the comparison control, and neither recipe claims an exact match to
Intel's reference renderer, camera, exposure or color grading.

## Neutral native and browser baselines

The fixed `atrium`, `balcony` and `curtains` views use Intel's first, second
and fourth camera positions, forward directions and vertical field of view.
The harness uses world-up Y. Daylight follows the source SUN orientation with
an explicitly chosen warm intensity of 3, three shadow cascades, TAA and high
AO. All source glTF lights have zero intensity, so their intensities are not
claimed as a usable source lighting rig.

The generated environment is a uniform neutral linear radiance of 0.125,
with an exact constant PMREM and matching display panorama. This reproducible
fill needs no HDR download, GPU bake or unrelated Bistro files. It is not a
physical outdoor sky or Intel reference-renderer calibration. No baked GI or
local reflection probes are invented.

```sh
# Native: 60 warm-up + 60 measured frames per camera.
cargo run --release -p sgl-3d-examples --example bistro -- \
  --asset .cache/sponza/prepared/Sponza.glb \
  --fixture examples/sgl-3d/fixtures/sponza-daylight.json \
  --view all --size 1280x720 --frames 120 --warmup 60 \
  --output .cache/sponza/captures/native-daylight

# Browser: the same asset, camera and lighting, on Chromium WebGPU.
bun scripts/tasks.ts measure-browser \
  --fixture examples/sgl-3d/fixtures/sponza-daylight.json \
  --asset-dir .cache/sponza/prepared --view atrium \
  --size 1280x720 --frames 60 --warmup 60 \
  --capture .cache/sponza/captures/browser-daylight/atrium.png \
  --output .cache/sponza/captures/browser-daylight/atrium.json
```

Daylight explicitly omits ray-only scene data. The optional `sponza-gi.json`
fixture retains it and adds high dynamic GI with a 952-probe, 2-metre-spaced
volume over the atrium and galleries. The upper roof/exterior is outside that
volume. To compare GI, substitute this fixture in either command and use 180
warm-up frames. Native `--frames` is total, so use `--frames 240 --warmup 180`;
browser `--frames` is measured, so use `--frames 60 --warmup 180`. A warm-up
count does not establish convergence. Use `--observe-gi` in a separate browser
diagnostic run to verify active tracing without mixing its overhead into the
clean performance run. Full geometry and ray buffers can exceed a browser or
device limit even when source/decoded load budgets fit; an error is not a pass.

Native and browser reports record requested/effective settings, source/load
counts, CPU startup and per-pass GPU timestamps. Captures and original notices
are copied beside the reports. Inspect the actual images; screenshots are not
an aesthetic pass/fail test. Compare the same view, settings, frame range,
resolution and machine, without competing GPU work. GPU time and serialized
frame-completion wall time mean different things.

For opaque-pass diagnosis, both native `bistro` and `measure-browser` accept
`--split`. It forces the existing depth/lighting two-pass path instead of the
fused opaque path, and records `split_opaque` in reports. It is a diagnostic,
not a changed default or a saved quality setting. Compare its combined depth
and opaque timings, along with total GPU time, against the same camera, asset,
warmup and settings without the flag.

Fast preparation checks do not download the scene or require a GPU:

```sh
bun test scripts/prepare-sponza.test.ts scripts/prepare-sponza-lighting.test.ts scripts/prepare-sponza-gallery.test.ts
cargo test -p sgl-3d-examples --example sponza_prepare --example sponza_environment
```

The repository's required final check remains `bun scripts/tasks.ts check`.
It does not implicitly download these optional multi-gigabyte assets.

## Sources and license caution

Intel's [sample library](https://www.intel.com/content/www/us/en/developer/topic-technology/graphics-research/samples.html)
links the [base scene](https://cdrdv2.intel.com/v1/dl/getContent/830833) and
[Colorful Curtains](https://cdrdv2.intel.com/v1/dl/getContent/726650).
The exact archive byte counts and SHA-256 values are in
[`sponza-sources.json`](../../scripts/sponza-sources.json).

The archive credits identify Frank Meinl, Anton Kaplanyan and the Sponza Addon
Package Crew. Both original `credits_license.txt` files are preserved verbatim
as `BASE-LICENSE.txt` and `CURTAINS-LICENSE.txt` in the prepared output, alongside
`SOURCE.json` and `SGL-ATTRIBUTION.txt`.

The official page describes Creative Commons Attribution and the archives
include CC BY 4.0, but their preamble also restricts personal/educational and
commercial use. This workflow is for local evaluation, not asset publication.
Preserve both texts and clarify the conflicting preamble with Intel before
redistribution or other commercial use. Do not put source or prepared assets
in Git, published SGL packages or a hosted demo.
