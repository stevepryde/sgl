# Amazon Lumberyard Bistro in SGL

From the SGL root, install the repository's [development tools](../../CONTRIBUTING.md#setup)
and the official [KTX-Software](https://github.com/KhronosGroup/KTX-Software/releases)
`ktx` command (tested with 4.4.2). The export also uses Playwright Chromium
and downloads Three.js 0.185.1 into an ignored tool cache. No global Node
package or runtime game dependency is added.

```sh
bun scripts/prepare-bistro.ts
cargo run --release -p sgl-3d-examples --example bistro
```

If `ktx` is not on PATH, pass its executable:

```sh
bun scripts/prepare-bistro.ts --ktx /absolute/path/to/ktx
```

This downloads the exterior from
[qian-o/GLTF-Assets](https://github.com/qian-o/GLTF-Assets/tree/816a1d6533c55787b3fabc9b75660c0d0c57ebc6/Bistro),
including its Git-LFS geometry, referenced textures, licence and supplied HDRI.
It explicitly converts content to `target/bistro/prepared/`, then the native
example imports it through ordinary `asset::load` and `Scene::add_asset`.
There is no special Bistro path in the library.

The source download is roughly 545 MiB, the default prepared content roughly
545 MiB, with another 96 MiB already included there for the HDR lighting atlas.
The default 1024-pixel texture export decodes about 896 MiB of images at load;
geometry, prepared GPU data and transient builds need additional memory.
Allow several GiB of RAM. Everything downloaded/generated stays in ignored
`target/`; do not add these assets to Git or SGL's published crates.

## What the conversion does

- Khronos `ktx extract` decodes the published Basis/UASTC textures into PNG.
  The export writes core glTF texture sources and removes the no-longer-used
  `KHR_texture_basisu` requirement; the runtime still does not implement Basis.
- The source's packed `*_Specular` textures are stored in the wrong glTF
  `KHR_materials_specular` slot. Their documented green roughness and blue
  metalness channels become `metallicRoughnessTexture`, with unit factors.
  No baked occlusion binding is inferred: the source glTF binds none, and
  inspected scooter, concrete and cobblestone red channels are identically
  zero. Using them as glTF ambient visibility would remove all indirect
  light. Red is preserved unused, not inverted or assigned a guessed meaning.
- This conversion's XY-only normal images have a constant zero blue channel;
  their positive Z is reconstructed from RG before filtering. Full RGB normal
  images keep their Z. DirectX tangent-space normals have green inverted for
  glTF's +Y convention. Resized normals are renormalized; data textures are filtered as linear data.
  Base colour and emission resize in linear light with premultiplied alpha.
- Textures are limited to 1024 pixels by default, with no geometry reduction.
  Authored geometry, transforms, UVs, alpha and transmission are retained.
- The supplied `san_giuseppe_bridge_4k.hdr` is filtered by Three's actual
  common/WebGPU `PMREMGenerator`, whose inverted-Y render-target convention
  SGL's PMREMNode-based sampler already implements. The WebGL generator
  cannot be substituted merely because its atlas has the same dimensions. The RGBA16F atlas retains
  HDR lighting; only the separate RGBA8 display panorama is clipped to its
  storage range. A browser is needed for this explicit export, never at runtime.

Choose another export resolution/directory explicitly:

```sh
bun scripts/prepare-bistro.ts --max-texture 2048 --output target/bistro-2048
cargo run --release -p sgl-3d-examples --example bistro -- --asset target/bistro-2048/prepared/BistroExterior.gltf --output target/bistro-2048/captures
```

Source files already present are reused. Re-running preparation explicitly
regenerates all converted content and lighting. There are no fingerprints,
automatic staleness gates or runtime regeneration. To choose new source
content, use a separate output directory or explicitly remove that source
cache before preparing again.

## Rendering and comparison

The default run writes `street.png`, `bike.png`, `overview.png` and
`measurements.json` under `target/bistro/captures/`. Cameras are fixed and
recorded in JSON, with 48 frames each and eight warm-up frames excluded from
per-pass GPU timing. CPU glTF load and model preparation/upload times are
reported separately. PNGs use the renderer's display output, not a diagnostic
reshading of an analytic scene.

```sh
# One camera; use the same camera/exposure when comparing changes.
cargo run --release -p sgl-3d-examples --example bistro -- --view street --size 1920x1080 --frames 90
# Optional authored rectangle fill near the scooter, using production LTC.
cargo run --release -p sgl-3d-examples --example bistro -- --view bike --rect-light --output target/bistro/ltc
# Isolate normal-map contribution through the existing diagnostics control.
cargo run --release -p sgl-3d-examples --example bistro -- --view street --no-normal-maps --output target/bistro/no-normal-maps
```

Other controls: `--asset FILE`, `--output DIR`, `--view all|street|bike|overview|bevy`,
`--size WxH`, `--frames N` (greater than 8), `--exposure STOPS` (default 0).
The scene uses its supplied HDRI and an explicitly authored directional sun;
`--rect-light` adds an authored 3×2 m warm rectangle near the scooter. That
rectangle is an experiment, not an imported source light. It uses the current
production LTC integral with shadow-map visibility (or ray-traced visibility
in max mode on supported hardware); it does not enable the
matched-sample estimator from the separate area-light comparison experiment.

This is a geometry/material workload with reproducible chosen lighting, not
an exact recreation of the original Falcor scene. There is no imported
analytic-light setup, baked GI, camera path or reference-renderer parity claim.
The default uses TAA, bloom, high shadow maps and environment reflections.
Ambient occlusion, screen-space reflections, world-space reflections and
hardware ray tracing are off. Dynamic GI's High setting does nothing without
an authored volume. Judge captures visually; numerical metrics are not an
aesthetic acceptance test.

### Maximum-quality comparison

```sh
cargo run --release -p sgl-3d-examples --example bistro -- --max-quality --rect-light --size 3840x2160 --frames 300 --output target/bistro/max
```

`--max-quality` is an example-owned bundle, not a new library default. It sets
full scene resolution, FSR2 Native AA (no upscaling), 16× anisotropic filtering,
Ultra AO, full-resolution Velvet SSR, all-scene world-space reflections,
hardware ray tracing, High ray-traced shadows, four high-quality shadow
cascades, bloom, and High dynamic GI. It installs an authored volume of 2,584
probes at 2 m spacing, spanning x[-22,14], y[-1,13], z[-18,14], over the primary
street/courtyard region, not the full asset. Use at least 300 frames to let
histories develop; this is not a guarantee that every probe has converged.
Half the frames (at most 120) are excluded from its timings. Textures retain
the chosen preparation resolution; `--max-quality` never re-exports content.
The original textures top out at 2048; that export decodes about 3.5 GiB of
images before geometry/GPU data and needs substantially more RAM than 1024.

The JSON records the requested settings, actual AA, hardware RT state/errors,
ray-source coverage and GI-volume presence. FSR2 can fall back to TAA; absent
hardware ray tracing uses portable rays for GI/reflections and shadow maps.
WSR and ray shadows internally trace at half resolution even at maximum
supported quality. `--aa taa|fsr2|smaa|off` isolates AA, `--no-gi` leaves out the
volume, and `--no-ray-shadows` leaves shadows on maps. `--settings FILE` loads
a complete serialized `Settings` bundle after the max/default bundle; the
AA override then applies. Device features are selected from these resolved
settings, including FSR2 or hardware rays requested by a fixture. Use an
explicit volume or `--max-quality` when its GI volume is required, or `--no-gi`
to omit that volume. This supports
controlled single-feature comparisons without changing production defaults.

Fog, heat shimmer and motion blur remain off: they require authored content
or motion and are not a universal quality increase. Tone mapping and exposure
stay unchanged for comparisons. No baked lightmaps or reflection probes are
invented. LTC is already the normal production rectangle-light shading path;
the experimental matched estimator is not enabled by this bundle.

## Browser WebGPU benchmark

The browser consumer renders the real exterior through the same public
`Scene` / `Renderer` API and shares the native camera, sun and baseline
settings recipe in `examples/support/bistro_scene.rs`. Export smaller textures
and embed the prepared glTF before running it. These preparation commands keep
the library's default 1 GiB browser source and decoded-scene safety caps;
explicit fixture overrides are described below:

```sh
bun scripts/prepare-bistro.ts --max-texture 512 --output target/bistro-512
bun scripts/package-bistro-browser.ts --prepared target/bistro-512/prepared --environment target/bistro-512/prepared --output target/bistro-browser-512
bun scripts/tasks.ts measure-browser --bistro --frames 180 --view street --capture target/bistro-web/street.png --output target/bistro-web/street.json
```

If the source and HDRI are already prepared, avoid downloading/exporting them
again: run `bistro_prepare` with `--source target/bistro/source --output
target/bistro-512/prepared --max-texture 512`, then package with the existing
`--environment target/bistro/prepared`. Packaging retains all geometry,
corrected material bindings and authored transforms. `asset-stats.json`
reports source and decoded budgets separately, not a process-memory estimate.
The 512 export uses roughly 250 MiB GLB and 224 MiB level-zero decoded images
before optional deduplication. The separate HDR PMREM is 96 MiB. Actual
allocations, transient copies, mip chains and ray-scene storage are larger.
A device's storage-buffer binding limit can reject a scene that fits the
loader budget; the JSON records adapter and requested device limits.

The harness fetches/decodes/uploads the GLB, releases its source, and only
then fetches the HDR environment. Its default is 1280×720, 60 warm-up frames,
TAA, high three-cascade shadow maps, bloom and the supplied environment;
AO, SSR, world-space reflections, ray shadows and dynamic GI are off.
It requests no hardware RT or native FSR2 features. Actual AA, ray-tracing
state, GI-volume absence, sizes, binding tier and errors are recorded, so a
requested unavailable feature is not mistaken for active rendering.

Controls include `--asset-dir DIR`, `--view street|bike|overview|bevy`,
`--size WxH`, `--warmup N`, `--exposure STOPS`, `--cascades 1..4`,
`--aa taa|smaa|fsr2|off`, `--rect-light`, and `--settings FILE` for a complete
serialized settings bundle. `--skip-build` repeats the saved release module;
the module hash and its saved build provenance distinguish it from current
checkout source. As with the native example, lighting changes are explicit
art-direction experiments, not proof of renderer accuracy.

Optional `dynamic_gi_volume` in the fixture explicitly supplies `origin`,
`spacing` and `probes`; settings select its quality. Native `--gi-volume FILE`
accepts the same volume object, independently of the maximum-quality bundle.
`--observe-gi` records asynchronous per-frame probe/ray/visibility/budget and
convergence observations. This instrumentation adds GPU work: inspect it in a
separate diagnostic run, never treat its timings as clean performance.
For an active-convergence cost case, use the same fixture/settings with
`--warmup 0 --frames 120`, once clean and once with `--observe-gi`; compare
against GI Off with the same frame range. The diagnostic reports establish
whether tracing was active. A long-warmup static result measures converged
sampling/paused overhead, not the cost of tracing a changing game scene.

`--no-normal-maps` is a diagnostic raster-normal isolation switch and is
recorded separately from serializable settings. It is not a material repair.
`sun.shadow: false` in a camera removes its directional shadow maps without
removing direct sunlight, useful for separating direct shadows from AO/GI.
Each camera can set `ambient_occlusion_radius` in metres (default 0.5),
independently of quality/sample count; reports retain its actual value.

Native `--fixture FILE` applies the same camera/light/settings/GI and environment
recipe, positive instance scale and named camera; `--settings`, `--exposure`
and `--cascades` explicitly override it. `--view CAMERA_ID` selects any camera
in that fixture. Auxiliary assets must be regular files confined to its asset
directory; parent paths, absolute paths and symlink escapes are rejected.
Native `--frames` remains total frames; `--warmup` selects the excluded prefix,
whereas browser `--frames` selects measured frames in addition to `--warmup`.
For a matched 360+120-frame comparison, use native `--frames 480 --warmup 360`.

One optional `specular_probe` loads an explicitly authored outdoor local
reflection: schema 1, encoding `rgba16f-le`, seven levels, face_size 64/128/256,
center, world-space influence min/max, blend vector, optional proxy min/max,
and `rgba16` payload basename. Payload size/version are validated before u16
expansion; the public scene API checks placement/radiance. Native
`--capture-probe FILE` accepts that placement descriptor plus exact
`exclude_materials` names, captures after the measured frames, and writes
the payload and capture recipe beside its output. Capture-only exclusions
are restored on success or failure. The sidecar separates the requested fixture
from applied frame lights/environments/mask and records exact CLI arguments
and the normal-map diagnostic override. The browser only uploads the authored
result; capture is blocking native asset authoring, never a runtime loop.
Regenerate deliberately when its scene or lighting changes. Inspect influence
boundaries, motion and self-reflection; a local probe is not correct for every
receiver in an open street.

The PNG contains exact asynchronous display-output readback, not browser
chrome. It is read after measured frames and timestamp draining. `--motion`
uses fixed 1/60 s simulation and a shared eye/target X translation of
`sin(t*0.7)*0.35` metres. Add `--sequence DIR` to render 192 additional frames
after timing and save 32 PNGs at six-frame intervals (3.2 seconds of simulation). The JSON records
indices, simulation times, actual eye positions and exclusions. Compare the same route/indices for TAA/SMAA shimmer
or ghosting; a final still alone cannot establish temporal quality. This small
translation checks local shimmer, not fast turns, broad disocclusion or a
multi-metre reflection-probe boundary crossing. Use additional explicit views
inside, at and outside that boundary when evaluating an authored probe.

CPU encoding and serialized queue-completion wall times are distinct from
GPU timestamp distributions. Per-pass medians/p95/max include sample counts
and missing/unsupported timing reasons. Startup stages, transfer sizes,
browser version, requested/effective settings, content provenance and errors
are retained. Resource values are partial; WASM linear-memory capacity is
neither peak memory nor total CPU/GPU memory. This offscreen benchmark does
not measure presentation, display pacing, input latency or an uncached WAN
download. Close other GPU/build/export work and repeat finalists at least
three times before drawing performance conclusions. No numerical or image
similarity threshold judges aesthetics.

### Reusing the fixture

`--fixture FILE` selects the generic scene route and supplies the small schema-1 descriptor consumed by this same
browser renderer and measurement runner: a stable scene id, embedded GLB
basename, explicit environment basenames, source metadata and notice files,
fixed cameras and authored lights, settings, output size and warm-up count.
Optional positive `scale` is an explicit instance transform, never inferred
from content.
Optional top-level `point_lights` registers ordinary `Scene::add_light` point
sources in native and browser runs. Each entry requires `position` (world
metres), nonnegative linear-RGB `color`, nonnegative `intensity` (candela),
and positive `range` (metres). Optional nonnegative `radius` defaults to
0.025 m and boolean `casts_shadow` defaults to false. These world-space
placements are independent of model `scale`. Missing arrays preserve fixtures
without practical lights; malformed/nonfinite values fail rather than silently
falling back. The descriptive `name` is authoring metadata only. Served asset paths are basenames within `--asset-dir`, optionally
under its fixed `Compressed/` companion folder.
The resolved descriptor is embedded in each report; notices are copied beside
requested reports/captures. This is example-owned content, not a scene
registry, game framework or renderer special case.

Optional `compressed_images: "compressed-images.json"` supplies an offline
schema-1 image manifest. `--compressed-manifest NAME` overrides it. Each
entry names its glTF image `index`, `ktx2` path, `width` and `height`, and
optionally an exact decoded `decoded_level_0` PNG for ETC2/ASTC material rays.
The manifest `format` is `bc7`, `etc2` or `astc4x4`; the browser requests that
specific adapter feature and refuses an unsupported format, with no runtime
transcode. Omitted indices decode their embedded PNGs. Original alpha-sensitive
base maps can remain RGBA while other maps are compressed.

Optional `ray_data: "disabled"` (or `--ray-data disabled`) creates a raster-only
scene and skips decoded ETC2/ASTC companions. The default remains `"enabled"`;
GI, world-space reflections, material rays and decals need that source data.
This is an immutable scene choice, not a runtime quality switch. Reports
include requested/effective choices and partial source-buffer allocation,
used and live bytes, so a capacity increase is not mistaken for live content.

Optional `load_budget: {"source_bytes": 1073741824, "decoded_bytes": 1073741824}`
passes checked explicit caps to both KTX2 parsing and glTF import. Omit it to
keep the library's platform defaults. Invalid/zero caps fail rather than being
clamped. Caps are safety policy, not heap reservations, available memory,
network-transfer totals or GPU limits; raising them cannot guarantee a scene
fits a browser. Supplied KTX chains and optional decoded companions share a
caller-side retained-image allowance checked before the next expansion; the
importer still charges geometry and all supplied images together afterward.
This does not bound transient decoder memory or aggregate network transfer.
A failed attempt should be followed by a fresh page/scene.

The checked-in `fixtures/metal-rough-spheres.json` is a second, small content
recipe for Khronos' CC0 **MetalRoughSpheresNoTextures**. Prepare its glTF/BIN
with the same `embedPreparedGltf` helper used by Bistro packaging, retain its
source notices/metadata, and supply the explicit CC0 environment files and
Three notice in its asset directory:

```sh
bun scripts/tasks.ts measure-browser --asset-dir target/metal-rough-spheres/prepared --fixture examples/sgl-3d/fixtures/metal-rough-spheres.json --frames 60 --capture target/materials/capture.png --output target/materials/measurement.json
```

The fixture's `SOURCE.json` records its download source, content hashes,
authorship, licence and packaging change. The shared runner validates actual
draws, frame/sample accounting, timing distributions, readback dimensions and
WebGPU/browser errors. Always inspect the capture too: draws alone do not
prove a camera frames the subject at a useful scale.

### Replaying the authored Bistro profiles

`fixtures/bistro-quality.json` is an explicit 1080p recipe with TAA, 16×
anisotropy, AO Medium at 0.5 m, four High shadow cascades, ACES filmic, and
a bounded 2,592-probe Low diffuse-GI volume. SSR, world-space reflections, ray shadows
and hardware RT remain off. `fixtures/bistro-lean.json` uses 720p and immutable
raster-only source data with GI off. These are example-owned choices, not
library defaults or a phone recommendation. The mesh still has 2.83 million
triangles and its asset transfer is large. Keep measured desktop results
separate from untested mobile expectations.

The lighting recipe rotates the sky and sun azimuth together, then explicitly
raises the analytical sun to 70 degrees so adjacent buildings no longer cover
the terrace and most of the road. Environment yaw is 3.625858159 radians;
light travel direction is `[0.338582668, -0.939692621, 0.048368951]`. Exposure remains +0.6,
and sun color/normal-incidence strength remain unchanged. ACES filmic is the
selected existing tone mapper; no custom grading or material changes apply.
The sun elevation is art directed independently of the HDR capture, not a calibrated match to
its display panorama. Directional shadows and contact AO remain enabled.

The lighting HDR caps only a small authored sun lobe before PMREM generation;
the original display panorama is retained. The cap is a documented
approximation, not recovered hidden sky or a general renderer algorithm.
The preprocessing report records the original calibration, source and derived
hashes, tools and credits. Fixture `lighting_authoring` records the subsequent
sun placement and practical lights. These are provenance, not runtime
staleness gates. Preserve original roots and use separate outputs.

The quality GI volume spans `[-18,-1,-16]` to `[33,20,35]`, with 3 m spacing
and `[18,8,18]` probes. It covers the visible right façades as well as the
courtyard, not the full asset. The previous 840-probe volume ended at z=10;
right façade receivers at z≈19–27 fell back to unoccluded sky diffuse, making
them look pale. GI replaces that fallback rather than adding another ambient
term. Keep the sky intensity at 0.7; broad coverage supplies geometry-aware
indirect visibility without globally darkening the scene. The larger volume
uses 720 warm-up frames for captures; inspect convergence separately before
using its static timings. Three-metre spacing trades some spatial detail for
coverage. The lean profile keeps GI off and therefore has no such visibility.

Both profiles now author 111 actual point lights at source bulb/lamp
geometry centers:
33 street/hanging fixtures at 4 cd, 14 wall/building fixtures at 0.8 cd, and
64 individual colored string bulbs at 0.08 cd. Main ranges are 5 m with 0.07 m
source radius; string ranges are 1.25 m with 0.025 m radius. Positions include
the source glTF node/root transforms; string bulbs use separate connected
components, not one light at the center of the entire string. These are
explicit fixture choices rather than an automatic emissive-to-light import.
They are unshadowed short-range approximations: the source's opaque emissive
enclosures would otherwise self-occlude point lights inside them. Nearby
surfaces can receive light through walls; they do not substitute for the
sun's shadows. Both reports read registered/active/shadow-casting counts back
from retained scene light state. Native per-view `local_shadows` and browser
`lighting.local_shadows` additionally report actual atlas occupancy, overflow,
static-layer draws, face draws and total draw operations. The first frame is
reported separately from measured-frame min/max/totals, so warm-cache timings
do not hide cache population. Counts cover visible shadow-casting lights, not
all authored lights; atlas overflow remains lit without shadows. They use the
existing `Renderer::local_shadow_stats` accessor, without enabling GPU
readbacks or changing the renderer.

For a matched shadow-cost comparison, copy the descriptor and set each
point light's `casts_shadow` to true, keeping positions, intensity, range and
other settings unchanged. Inspect lamp self-occlusion as well as wall leakage:
this is a diagnostic comparison, not a recommendation to change the maintained
profiles. Use an unobstructed source or author its emitting enclosure correctly
before relying on practical shadows. Static cache reuse and screen-coverage
allocation already apply to all eligible lights; a small manual light cap is
not required. Repeat camera motion and changing casters separately from a
static view, and retain the first-frame counts with the warm timings.

Remove `point_lights` for a matched lights-off
comparison without changing emission, sun, exposure, or ambient settings.

From the repository root, prepare one quality package explicitly:

```sh
bun scripts/prepare-bistro.ts --max-texture 1024 --output target/bistro-1024
python3 -m venv target/bistro-export-tools
target/bistro-export-tools/bin/python -m pip install numpy pillow opencv-python texture2ddecoder==1.0.6
target/bistro-export-tools/bin/python scripts/bistro-sun-reduced-environment.py --source target/bistro-1024/source/san_giuseppe_bridge_4k.hdr --original-environment target/bistro-1024/prepared --output target/bistro-derived-environment
bun scripts/bistro-environment.ts target/bistro-derived-environment/sun-reduced/san_giuseppe_bridge_4k-sun-reduced.hdr target/bistro-derived-environment/sun-reduced/pmrem-bake
target/bistro-export-tools/bin/python scripts/bistro-sun-reduced-environment.py --output target/bistro-derived-environment --finalize-pmrem
bun scripts/package-bistro-browser.ts --prepared target/bistro-1024/prepared --environment target/bistro-derived-environment/sun-reduced --output target/bistro-1024-rgba --deduplicate-images
cargo build --release -p sgl-3d-examples --example bistro_compress_mips
bun scripts/compress-bistro-browser.ts --prepared target/bistro-1024/prepared --rgba-package target/bistro-1024-rgba --output target/bistro-quality --ktx "$(command -v ktx)" --mips-tool target/release/examples/bistro_compress_mips --python target/bistro-export-tools/bin/python --format bc7 --threads 2
bun scripts/tasks.ts measure-browser --fixture examples/sgl-3d/fixtures/bistro-quality.json --asset-dir target/bistro-quality --view bevy --frames 120 --capture target/bistro-quality-runs/hero.png --output target/bistro-quality-runs/hero.json
```

The exporter keeps alpha-sensitive base maps RGBA and delivers final supported
blocks, not Basis data requiring a runtime transcoder. BC7 requires the
adapter's BC feature. For an ASTC or ETC2 target, choose `--format astc4x4` or
`--format etc2` in a separate output, check actual support and inspect the
lossy normal/color result. An encoding format is not a quality guarantee.

For the lean package, reuse the source and derived environment deliberately:

```sh
cargo run --release -p sgl-3d-examples --example bistro_prepare -- --source target/bistro-1024/source --output target/bistro-512/prepared --max-texture 512 --ktx "$(command -v ktx)"
bun scripts/package-bistro-browser.ts --prepared target/bistro-512/prepared --environment target/bistro-derived-environment/sun-reduced --output target/bistro-512-rgba --deduplicate-images
bun scripts/compress-bistro-browser.ts --prepared target/bistro-512/prepared --rgba-package target/bistro-512-rgba --output target/bistro-lean --ktx "$(command -v ktx)" --mips-tool target/release/examples/bistro_compress_mips --python target/bistro-export-tools/bin/python --format bc7 --threads 2
bun scripts/tasks.ts measure-browser --fixture examples/sgl-3d/fixtures/bistro-lean.json --asset-dir target/bistro-lean --view bevy --frames 120 --capture target/bistro-lean-runs/hero.png --output target/bistro-lean-runs/hero.json
```

One optional, deliberately local reflection experiment can be authored from
the quality lighting after the GI has settled. The placement descriptor
excludes only the scooter's four named materials during capture and restores
them afterward. The probe affects specular visibility, not diffuse GI or
contact occlusion, and can also be uploaded by a raster-only scene:

```sh
cargo run --release -p sgl-3d-examples --example bistro -- --asset target/bistro-quality/BistroExterior.glb --fixture examples/sgl-3d/fixtures/bistro-quality.json --view bevy --size 1920x1080 --frames 840 --warmup 720 --capture-probe examples/sgl-3d/fixtures/bistro-local-probe.json --output target/bistro-probe-authoring
cp target/bistro-probe-authoring/bistro-local-probe.rgba16 target/bistro-quality/
bun -e 'const f=await Bun.file("examples/sgl-3d/fixtures/bistro-quality.json").json(); f.specular_probe=await Bun.file("examples/sgl-3d/fixtures/bistro-local-probe.json").json(); await Bun.write("target/bistro-quality-with-probe.json",JSON.stringify(f,null,2));'
bun scripts/tasks.ts measure-browser --fixture target/bistro-quality-with-probe.json --asset-dir target/bistro-quality --view bike --frames 120 --capture target/bistro-probe-runs/bike.png --output target/bistro-probe-runs/bike.json
```

Use the unmodified quality descriptor for the probe-off control. Check the
same views, GI convergence, probe boundaries and a short motion sequence
before choosing it. `--observe-gi` is a separate instrumented run; do not
infer convergence just from a chosen warm-up count. Neutral tone mapping can
be compared by copying the fixture and changing only `settings.tone_mapping`
to `KhronosPbrNeutral`, preserving exposure and lighting.

## Attribution and distribution

**Amazon Lumberyard Bistro**, Amazon Lumberyard, July 2017,
[NVIDIA ORCA](https://developer.nvidia.com/orca/amazon-lumberyard-bistro),
**CC BY 4.0**. The preparation retains `LICENSE.txt` and the conversion's
`README.md`, writes source attribution/revision and a list of modifications
to `SGL-ATTRIBUTION.txt`, and adds attribution to glTF's `asset.copyright`.
The glTF conversion is credited to qian-o/GLTF-Assets at the revision above.
Redistributing prepared content requires carrying these notices, credit and
change information; it does not become MIT/Apache merely by using SGL.

**San Giuseppe Bridge**, Greg Zaal, HDRIHaven / Poly Haven,
[CC0](https://polyhaven.com/a/san_giuseppe_bridge). The prepared notices credit
the HDRI separately. Three.js is an export tool under MIT; its original licence
is copied as `THREE-LICENSE.txt`. Khronos KTX-Software is an external tool;
its code is not copied into the repository or runtime package.

Fixture runs copy their listed notices beside the captures; native runs with
no fixture use `BISTRO-LICENSE.txt` and `BISTRO-ATTRIBUTION.txt`. Share captures
with the Bistro credit and licence/source link.

## Rough Bevy reference view

`--view bevy --max-quality --size 1280x766 --frames 90` is an approximate
camera/sunlight match to Bevy 0.19's [official Solari image](https://bevy.org/news/bevy-0-19/solari.jpg),
not a calibrated benchmark. It keeps the ordinary renderer and exposure,
uses camera (-20,10,-10) looking at (-3,4,0), a 55-degree vertical field of view,
sun direction (-0.15,-1.15,1) at intensity 4.5, and HDR environment intensity 0.7.
It is opt-in and excluded from no-fixture `--view all`. Leave `--rect-light` off
for this comparison. This rough default reference is separate from the
coherent lighting and GI in `fixtures/bistro-quality.json`; it does not use
that profile's derived environment. The metadata records actual values.
