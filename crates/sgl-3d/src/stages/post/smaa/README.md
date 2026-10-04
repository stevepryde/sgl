# Native SMAA 1x

The WGSL shader ports Three.js 0.185.1's
`examples/jsm/tsl/display/SMAANode.js`; the port was first made in Hyperdrive,
before SGL3D was extracted from it. It retains Medium color-edge detection,
eight search steps, the upstream area and search lookup tables, and
neighborhood blending. No temporal samples, history, jitter or
preset-dependent behavior are involved.

The port retains Three's fullscreen triangle, CPU-computed inverse dimensions,
vertex-interpolated UV/offset/pixel coordinates and sequential search corrections.
The final atlas correction uses explicit `fma` to prevent Metal reassociation of
the pixel size, 255 scale and normalized atlas value. Recomputing offsets in the
fragment or regrouping these expressions can change the chosen blend direction
at nearly equal weights.

`area.png` (160×560) and `search.png` (66×33) are decoded, otherwise unchanged
base64 PNGs from that installed Three.js source. They are embedded in Rust;
portable bundles do not need separate atlas files. The implementation and
lookup tables originate in [SMAA v2.8](https://github.com/iryoku/smaa/releases/tag/v2.8).
The adjacent `LICENSE-three.txt` and `LICENSE-smaa.txt` retain their notices.

Call `Smaa::new(device, queue, width, height, output_format)`, then `resize`
when target size changes. `encode(device, encoder, input, output, timing)` performs
edge detection, weight calculation, and blending. Input and output must be
distinct equally sized views; input must be filterable and texture-bindable.
Input is linear HDR color before AgX tone mapping and sRGB output conversion. Overlay the
HUD after presentation. Resize retains pipelines, samplers and lookup textures.

## Settings that isolate it

`Settings::antialiasing` `Off` skips all three SMAA passes. Tone mapping and any
chosen scene upscale precede a texel copy to the output attachment, which
dithers it and retains its normal sRGB conversion. `Settings::bloom` `Off` skips the
Gaussian pyramid and bloom sampling; its targets shrink independently of the
scene and reflection histories. `Settings::atmosphere` `false` skips the
volumetric fog and mist.

SMAA runs in the post stage on HDR, after reflections and before tone mapping,
so it can change the displayed reflection pixels but cannot change ray hits or
reflection history. Bloom can spread existing bright pixels; atmosphere can add
depth-dependent fog and procedural mist.

## Evidence

The following observations retain the original reports; they are not
acceptance of the current whole-scene output.

Numerical GPU checks exercise Off → On → Off at 80×64 and 37×29. Off matches
the independent sRGB transfer within one 8-bit code value, the output's
dither, for RGBA16F, RGBA and BGRA outputs. A separate HDR impulse check verifies that
Bloom Off removes previous halos and explicit On spreads positive energy into
otherwise black neighboring pixels, across High → Low → High. These establish
control behavior, not resolution of the reported scene artifacts.

```sh
cargo test -p sgl-3d --lib antialiasing_off_preserves_captured_pixels -- --ignored --nocapture
cargo test -p sgl-3d --lib bloom_switch_removes_halos_and_preserves_low_override -- --ignored --nocapture
```

The ignored real-device test renders diagonal geometry at native resolution
and at 16× resolution in each dimension. It compares the SMAA result with the
averaged high-resolution raster, at two sizes to exercise resize. This detects
incorrect edge directions and atlas/search sampling that shader compilation
cannot reveal. Captures are written under `.cache/smaa-qa`. It is an explicit
development test, never a build, startup or deployment gate.

```sh
cargo test -p sgl-3d --lib diagonal_edges_approach_supersampled_rasterization -- --ignored --nocapture
```

On 2026-09-09 the standalone module test passed on Apple M5 / Metal. Sum of
absolute diagonal coverage error fell from 7614.14 to 5358.38 at 128×96 (29.6%)
and from 10919.18 to 7355.44 after resizing to 192×128 (32.6%). These figures
establish the isolated filter's behavior; they do not establish Windows/Linux
GPU acceptance.

The diagonal test compares against supersampled mathematical coverage; it does
not depend on AI image recognition.
