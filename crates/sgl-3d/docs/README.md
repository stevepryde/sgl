# SGL3D for game agents

Start here when building or changing a game that renders with SGL3D
(`sgl-3d`). These pages say what SGL3D offers and how to use it, then point to
the code. They are kept current with every feature change; the code and the
[package README](../README.md) hold the detail.

- [Features](features.md): what SGL3D renders and where each feature's API is.
- [Settings](settings.md): every rendering setting a game can choose, with
  values, defaults and preset behaviour.

For UI composed into a view's output, `View::output_encodes_srgb()`
reports whether its shader should emit encoded sRGB or linear values.

## What SGL3D is

A Rust/wgpu 3D renderer that a game links as a library, native or in the
browser (WASM + WebGPU): both are first-class targets, running the same code.
Native takes every feature the device supports and the browser every feature
WebGPU supports; it falls back or goes without only where the web platform
cannot, and the effective configuration reports which.
The game owns the window or canvas, event loop, input, simulation, camera, UI,
content and settings file.
SGL3D keeps GPU resources and encodes each frame into the game's command
encoder. It is not an engine: there is no ECS, editor, scene graph, scripting
or asset pipeline, and none should be added around it.

## A frame

A game holds a `Scene` (content), a `Renderer` (what its views share: the
pipelines, dynamic GI and the frame boundary), a `View` for each image of the
scene it draws (its targets, stages and histories) and its rendering
`settings::Settings`. The renderer creates geometry pipelines on the first
draw that needs each variant. Loading a level, entering a new geometry pass
or capturing a probe can therefore incur first-use compilation. Views and
captures on the same renderer reuse the cached pipelines.

1. Device: request `graphics_device::limits(&adapter)`,
   `graphics_device::features(&adapter)` and
   `graphics_device::fsr_features(&adapter)`, plus `TEXTURE_COMPRESSION_BC`
   for compressed bakes and material textures and `TIMESTAMP_QUERY` for GPU timing.
   Hardware ray tracing is optional and opt-in: a game that takes it, so
   that the scene's rays trace its acceleration structures, also requests
   `graphics_device::ray_tracing_features(&adapter)` with
   `experimental_features: unsafe { wgpu::ExperimentalFeatures::enabled() }`
   and turns `Settings::hardware_ray_tracing` on, and
   `Settings::ray_traced_shadows` too for ray-traced shadows
   ([hardware ray tracing](../README.md#hardware-ray-tracing)).
   In the browser the device is the page's WebGPU one, requested the same way
   from an instance `sgl_webgpu::instance` creates
   ([sgl-webgpu](../../sgl-webgpu/README.md)); any device at WebGPU's default
   limits runs SGL3D.
   `Renderer::binding_tier` reports the device's binding tier: `Extended`
   at 48 or more sampled textures per stage, else `Basic`, where baked
   light is non-directional, dynamic GI is off
   (`Renderer::dynamic_gi_in_effect`), so are volume paths
   (`Renderer::volume_paths_in_effect`), the anisotropy, clearcoat,
   iridescence, transmission, thickness, sheen and diffuse transmission maps
   give way to their factors and transmission is blended through unrefracted
   ([features](features.md#platforms)).
2. Load: `Scene::new` starts empty. Add content between frames and keep the
   identities it returns: `asset::load` (a file), `asset::load_slice`
   (bytes), or in the browser `asset::load_slice_in_browser` (bytes, its
   images decoded by the browser off the page's thread; awaited)
   (`Asset::ignored` lists what the file uses that SGL3D leaves out) into
   `Scene::add_asset`, instances with `add_instance` (static or moving;
   a glTF's instanced nodes, `Asset::instanced`, are a model each,
   `AssetIds::instanced`, with a transform per placement to install),
   point, spot and rectangle lights with `add_light` (a glTF's authored
   lights, `Asset::lights`, placed with `PunctualLight::place`; its suns go
   in the frame's directional slots, which the game picks), decals with
   `add_decal_image` and `add_decal`, environments with
   `add_environment`, an irradiance volume with `set_irradiance_volume`
   (its cells written by region with `write_irradiance_cells`), and a
   dynamic GI volume with `set_dynamic_gi_volume`. `Renderer::new` takes the
   device; `View::new` takes the renderer, the output format, the output size
   in physical pixels, the window's scale factor and the settings. Float
   composition textures stay linear on both platforms. Use `View::new_surface`
   with the actual surface configuration for direct window/canvas
   presentation; it owns the display transfer. See the package README's frame
   workflow.
   Import caps remain conservative by default. For a measured larger workload,
   use `asset::LoadOptions::budget` with checked `asset::LoadBudget::new` and
   `load_slice_with_options` or `load_with_options`; source and decoded caps
   are separate and never guarantee available heap or GPU memory. See
   [asset limits](../README.md#asset-and-environment-limits).
   If the scene needs no world-space ray effects, choose immutable
   `SceneOptions { ray_data: RayData::Disabled }` through `Scene::with_options`.
   It skips ray-only images/BVHs while keeping raster and shadow-map data;
   manually prepared models need matching `PreparedModelOptions`.
   Existing constructors remain enabled. GI, world-reflection and traced
   shadow effective-state queries take both settings and scene, so requested
   settings are not confused with this capability. See the
   [scene lifecycle](../README.md#retained-scene-and-frame-lifecycle).
3. Each frame: `Scene::set_instance` per moving instance (pose, `visible`,
   `capture_visible`), `set_instance_deformation` per deforming one (its
   joint matrices and morph weights, from an `animation::Pose` sampled at
   the game's time), `set_light` per changed light and
   `set_decal` per moved decal; `View::resize` (nothing happens unless the size, the scale or a
   setting that [applies at resize](settings.md#when-changes-apply) changed);
   a `FrameInput` with the camera, the authored look and the presentation
   time in seconds (`elapsed_seconds`, an `f64`, which moves materials'
   normal layers, their shaders and the mist); `Renderer::render` with
   `&mut [ViewFrame { view, input, settings, output, timing }]`
   into the output texture (`ViewOutput::Texture(&view)`). Draw the game's UI (`sgl-2d` can, on the same
   device), submit, and call `Renderer::finish_frame(&mut scene)`. One
   `render` call is one frame of the scene: never render it twice for one
   finished frame. A frame rendered and
   not finished, then rendered again as the same frame, restarts no
   history but FSR's; a cut in it holds until a frame is finished, and a
   stage switched off in it (antialiasing, a reflection method, another
   projection) restarts as after a finished frame without it; ambient
   occlusion and ray-traced shadows continue, as FSR does when it did not
   run in it. An abandoned encoder
   retains a pending fixed-to-automatic exposure reset. Retrying submitted
   filtered fog without finishing resets the overwritten volume history.
4. Set `FrameInput::camera_cut` on a camera cut. A view's history restarts
   itself then, after a resize that changes its targets, for a different
   scene, and after a finished frame of its scene that did not render it.
   `Fog::length` may change every frame; the fog keeps its history.
5. A large world: keep the game's own coordinates and call
   `Scene::move_origin` to keep what it renders near the render origin
   (chunk-aligned in a streamed world); then give the camera and edits in
   the new frame. Nothing restarts or redraws.

### Compiling ahead

A frame creates the shaders and pipelines it first needs, which stalls the
thread that renders: the first frame of a level, or of a settings change,
and a live probe's first refresh. `Renderer::compile` takes the arguments
of `render` (no encoder) and readies them before that frame, behind the
game's loading screen. Each loading
frame, after `View::resize` as before `render`:

```rust
let compilation = renderer.compile(&device, &queue, &mut scene, &mut views);
for job in compilation.jobs {
    workers.send(job); // the game's threads run `job.run()`; in the browser, run each here
}
if compilation.ready() { /* `render` with these arguments creates nothing */ }
```

- Every shader module and pipeline the frame needs is a compile job
  (`job.label()` names its stage). The call creates none itself, and
  neither does the frame once it is ready, so no thread of the game stalls
  for one; on native, give the jobs to several threads and they compile in
  parallel.
- FSR is the exception: its upscaler creates its own pipelines on the
  calling thread, its passes' in the `View::new` or `View::resize` that
  selects FSR (or changes its version or the view's scene size), and a few
  small helpers in the first frame after. Select FSR while the loading
  screen is up.
- A `View` stays on the thread that created it; `compile` runs there and
  only plans.
- It readies each live probe's first refresh too
  (`Renderer::refresh_specular_probe` through `views[0]`, from where the
  probe is), so that refresh creates no program, and it changes nothing a
  refresh in flight keeps.
- Draw once `ready()`. `pending` can rise between calls: a stage whose
  programs are missing leaves out what it feeds, which is asked for once
  its job has finished.
- It submits none of the frame's commands and commits no history; the
  scene's own uploads happen as in a frame. For each view it is a render
  that was not finished (step 3), so compile a view while the loading
  screen, not the view, is on screen, between frames: after `finish_frame`
  and before the next `render`.
- `render` never waits for it: what is missing is created in place, as
  without it. A job dropped without running is offered again;
  `Compilation::errors` lists failed jobs, each once.
- Call it again after a settings change, a resize or for another level's
  scene, with a renderer that draws no other scene meanwhile. Most resizes
  need nothing. A feature that holds nothing while off (screen-space
  indirect light) lets its programs go when it is turned off, so call it
  after turning the feature back on too.
- In the browser, run the jobs here and keep presenting until `ready()`,
  calling `compile` once a frame: a job starts WebGPU's asynchronous
  pipeline creation and returns, the browser compiles while the page runs,
  and `pending` counts what it is still compiling. Starting a creation
  still costs the browser some work, so the game chooses: all at once is
  ready soonest, a handful a frame keeps the loading screen smoothest. In
  Chromium the block world's 38 jobs at once paused the page for about
  70 ms; 8 a frame for about 15 ms, ready about 30 % later
  ([`browser_loading`](../../../examples/sgl-3d/examples/browser_loading.rs)).
  The device must come from `sgl_webgpu::instance`. A frame drawn before a
  job resolves creates that program itself, as without `compile`.

[`examples/loading.rs`](../../../examples/sgl-3d/examples/loading.rs) is the loop to copy, and
[`examples/browser_loading.rs`](../../../examples/sgl-3d/examples/browser_loading.rs) the browser's.

### Several views

A split screen, an inset, a rear view or an editor's viewports are several
`View`s of one `Scene`, each from `View::new(&renderer, …)` with its own
output, sizes and settings, listed in one `Renderer::render`; dropping a view
releases what it held. The first listed is the frame's primary view: its
`FrameInput` steers the scene's frame work (dynamic GI's lights, environment,
time and frustum, the rays' visibility mask) and its settings choose what a
scene has one of (`hardware_ray_tracing`, `dynamic_gi`,
`anisotropic_filtering`, the diagnostics layers); everything else is each
view's own. List every view that keeps temporal history in every frame: a
view left out of a finished frame restarts, so a view drawn every other frame
should use SMAA or no antialiasing. List every view of a frame in one
`render`: two renderers drawing one scene in a frame make the first's views
restart every frame. Each view holds its own targets, FSR
context and, once a frame or probe capture of it places a local shadow,
local-light shadow atlas, so memory grows with views. One renderer
shows one scene at a time; a game showing two scenes at once holds a renderer
for each. [`examples/views.rs`](../../../examples/sgl-3d/examples/views.rs) draws a main view and
a rear-view inset.

### A view on an in-world screen

A monitor, a security feed or a picture-in-picture inside the world is a
view that draws into a render texture of the scene instead of a texture of
the game's, and a material that shows it:
`Scene::add_render_texture(&device, RenderTexture::new(size))`; a `View`
created with `RenderTexture::format` and that size, listed with
`ViewOutput::RenderTexture(id)`; and `SurfaceMaterial::emission_render_texture`
on the screen's material (`Scene::set_material`), whose `emission` is the
screen's brightness and whose `wrap` is `ClampToEdge`, authored when the
material is added. List the main view first as ever: the renderer draws
the feed before the views that show it, so the screen shows this frame, and
leaves the feed out of the frames in which no view has the screen in its
frustum. Leave the feed out of the frame to freeze its screen. The picture
is the feed's displayed image, decoded as an authored sRGB texture is: no
gamma in between. Rays and dynamic GI see the screen's authored emission
map, and the picture has no mips.
[Render textures](../README.md#render-textures),
[`examples/monitor.rs`](../../../examples/sgl-3d/examples/monitor.rs).

### A mirror or a water plane

A flat surface that should mirror the scene, off-screen and moving things
included, is a planar reflector: `Scene::add_planar_reflector(PlanarReflector::new(point, normal))`
and `SurfaceMaterial::planar_reflector` on the material of the geometry
that lies in that plane (`Scene::set_material`). There is no view to
create, list or order. Make the material what the surface is: a metal of
roughness 0 for a mirror, a dark low-roughness dielectric for water, with
its normal map if it ripples. Each view that shows the surface renders the
scene once more for it, so use it for the few surfaces that need it, pick
the level with `Settings::planar_reflections`, and read
`View::planar_reflections` for what each costs. Screen-space reflections,
probes and world-space rays keep serving every other surface.
[Planar reflections](../README.md#planar-reflections),
[`examples/planar_reflections.rs`](../../../examples/sgl-3d/examples/planar_reflections.rs).

The examples live in the repository, not the published crate:
<https://github.com/stevepryde/sgl/tree/main/examples/sgl-3d/examples>.
[`examples/offscreen.rs`](../../../examples/sgl-3d/examples/offscreen.rs) is the loop to copy
(`--decals` adds decals, `--motion-blur` motion blur, `--fog` volumetric
fog, `--cookie` a spot light's cookie);
[`examples/skinned.rs`](../../../examples/sgl-3d/examples/skinned.rs) adds a skinned, morphed glTF
sampled and posed with `animation::Pose`,
[`examples/gltf_lights.rs`](../../../examples/sgl-3d/examples/gltf_lights.rs) a glTF's authored
lights installed, with the suns for the frame's two slots picked,
[`examples/gltf_instancing.rs`](../../../examples/sgl-3d/examples/gltf_instancing.rs) a glTF's
instanced nodes installed as instances of one model each,
[`examples/instances.rs`](../../../examples/sgl-3d/examples/instances.rs) many instances of a few
models, [`examples/water.rs`](../../../examples/sgl-3d/examples/water.rs) a lake of 16 m
chunks whose waves its own shader moves and whose surface receives
screen-space reflections (its `dof-*` runs focus it with depth of field),
[`examples/shaders.rs`](../../../examples/sgl-3d/examples/shaders.rs) vegetation bent by a wind
shader and glass whose thickness varies across it,
[`examples/volumes.rs`](../../../examples/sgl-3d/examples/volumes.rs) closed glass that absorbs
over each view ray's path inside it, seen from outside and inside,
[`examples/streaming.rs`](../../../examples/sgl-3d/examples/streaming.rs) a block world streamed
in 16 m chunks about a moving camera, edited and remeshed, with the render
origin following it, and what each scene operation costs, and
[`examples/dynamic_gi.rs`](../../../examples/sgl-3d/examples/dynamic_gi.rs) a room lit by a dynamic
GI volume, and
[`examples/irradiance_volume.rs`](../../../examples/sgl-3d/examples/irradiance_volume.rs) a block
world's cave lit by the game's own light field, relit by region.
[`examples/browser_smoke.rs`](../../../examples/sgl-3d/examples/browser_smoke.rs) is the
browser's version: a WebGPU device, procedural content with block-compressed
bakes (a specular probe and an irradiance atlas), frames under several settings
and an asynchronous readback.
[Browser](../README.md#browser-wasm--webgpu) lists what differs there.

## Where to look

| Need | Go to |
| --- | --- |
| Units, axes, glam version, poses, history | [Conventions](../README.md#dependencies-and-data-conventions), [3D development](../../../docs/3d-development.md) |
| Frame lifecycle in detail | [Retained scene and frame lifecycle](../README.md#retained-scene-and-frame-lifecycle) |
| Lightmaps, irradiance atlases and ambient cubes | [Baked diffuse lighting](../README.md#baked-diffuse-lighting) |
| Light the game computes, relit by region (a voxel world's sky and block light) | [Irradiance volume](../README.md#irradiance-volume) |
| Bounce light without a bake | [Dynamic diffuse GI](../README.md#dynamic-diffuse-gi) |
| A sky and sun colour for any time of day, without a panorama | [Procedural sky](../README.md#procedural-sky) |
| A look over the whole lit scene before the tone map (underwater, a tint by distance), or one over the displayed picture | [HDR effects](../README.md#hdr-effects) |
| Highlight a targeted block, entity or interactable, through walls too | [Outlines](../README.md#outlines) |
| A feature's API, data format and limits | Its section in the [package README](../README.md) |
| Rendering rules and roadmap | [SGL3D spec](../../../specs/sgl3d.md) |
| How SGL3D's code is structured, for changing SGL3D itself | [SGL3D architecture](../../../specs/sgl3d-architecture.md), [code structure](../README.md#code-structure) |
| Why something is the way it is | [Decisions](../../../specs/decisions.md) |
| Public types | `src/lib.rs` (the public surface), `src/scene/mod.rs` (`Scene`), `src/renderer/mod.rs` (`Renderer`), `src/renderer/view.rs` (`View`, `ViewFrame`), `src/frame_input.rs` (`Camera`, `FrameInput`), `src/settings.rs` (`Settings`), `src/content/` (content types) |

## Steering

- Keep game policy in the game: presets, settings UI, save format, content,
  lighting design, cameras and the frame budget.
- Export PNG/JPEG images at most 16384 texels on a side. Their decoded
  RGBA8 bytes share the asset's 4 GiB native or 1 GiB browser decoded-data
  budget. KTX2 BC7, ETC2 RGBA8 and ASTC 4×4 mip chains must have valid dimensions and exact level
  sizes; malformed or oversized content fails at load
  ([loading](../README.md#asset-and-environment-limits),
  [compressed images](../README.md#compressed-material-images)). ETC2/ASTC
  material chains also require an explicitly prepared decoded level-zero
  companion for rays and decals; SGL3D does not transcode them in the browser.
  Keep fetches, exports and bounded scene chunks game-owned.
- Author normal maps green up, as glTF does: linear XYZ in RGB with Z in
  blue, +Y toward the image's top (decreasing V). Convert a DirectX
  (green-down) map at export by inverting green, and write Z for a
  two-channel one. A raised feature's upper edge is green in a green-up
  map and its lower edge in a green-down one. A mesh built in code takes
  glTF's UVs (V down the image) and either glTF's tangent frame in
  `Vertex::tangent` (T along increasing U, B = (N × T)·w toward decreasing
  V) or a zero tangent, which shades on each triangle's frame; only the
  glTF loader generates tangents
  ([material textures](../README.md#material-textures)).
  Check a map, a texture directory or a glTF/GLB's normal maps with
  `cargo run --release -p sgl-3d-examples --example normal_convention -- PATH...`
  ([source](../../../examples/sgl-3d/examples/normal_convention.rs)).
- Supply finite material normal/bump scales and lightmap chart scales/offsets,
  and ambient-cube channels in finite `0..=65504`; scene setters reject
  invalid values and retain the previous content (see the package README).
- Give SGL3D final presentation poses after the game's own interpolation;
  it keeps motion history itself.
- Treat lightmaps, irradiance atlases and specular probes as content. The
  game bakes them with explicit export commands and installs them at load;
  SGL3D never writes files. Rebake only when lighting content or a bake format
  changes, never for dependency or code updates.
- Refresh a live specular probe only where something it shows moves, and
  only as often as the game can afford: nothing refreshes one unless the
  game calls `Renderer::refresh_specular_probe`, and `ProbeRefreshPace`
  bounds what a call costs ([live probes](../README.md#live-specular-probes)).
- Persist `settings::Settings` in the game's own settings record and follow
  the [preset rules](settings.md#presets).
- Build scene and frame values from their defaults and set only what
  differs, so the game keeps compiling when SGL3D adds a value:
  `FrameInput::new(camera)`, `InstanceState::new(model)`,
  `Decal::new(image)`, and `..Default::default()` for the other values
  (`Settings`, lights and their shadows, fog and fog volumes, mist,
  environment lighting, materials, exposure and the post-processing
  parameters). In a `const`, use `..DirectionalShadow::DEFAULT`.
- Report the effective state honestly:
  `View::antialiasing_in_effect(&settings)` and `fsr_error()` say when
  the device fell back (FSR does where a browser's adapter lacks
  `float32-filterable` or six storage textures a stage). Where FSR does not
  run, choose `SceneResolution` explicitly and opt into
  `Settings::upscaling` for portable spatial enlargement to the output.
- In the browser, pass what a native game would read: asset bytes, and the
  frame time in `FrameInput::frame_time_ms`, and await readbacks
  (`Renderer::capture_specular_probe_async`, `diagnostics::read_async`): a
  browser cannot block on WebGPU.
- Measure cost per pass with `timing::GpuTiming` before and after a change.
  The [browser baseline](../../../examples/sgl-3d/BROWSER_BASELINE.md) measures
  a repeatable textured scene and separate upload phases with the existing
  `measure-browser` runner; its completion timings are offscreen, not display
  pacing. The [Bistro workflow](../../../examples/sgl-3d/BISTRO.md) extends it
  to prepared glTF scenes, matched native/browser cameras, explicit fixture
  settings, exact output captures and deterministic motion sequences. Its
  opt-in shadowed practical-light recipe explicitly authors dielectric bulb
  shells while retaining opaque wall/fixture casters; original profiles remain
  controls. See its material-authoring limitations before adapting it.
  The optional [Intel Sponza and curtains workflow](../../../examples/sgl-3d/SPONZA.md)
  uses that runner with a reproducible local-only asset cache, neutral baselines
  and a full-building source-HDR sunlit/GI recipe with optional native-authored
  gallery reflections; its downloaded assets are never bundled with SGL.
- Need a rendering feature SGL3D lacks? An effect of your own surfaces
  (waves, wind, a glass's varying thickness) is a
  [shader](../README.md#programmable-surfaces): the game's WGSL, which every
  pass runs. A feature of the renderer is built in SGL3D, not in the game,
  following the spec's
  [rendering development rules](../../../specs/sgl3d.md#rendering-development)
  and the [architecture](../../../specs/sgl3d-architecture.md): port a proven,
  permissively licensed implementation into its place in the stage order.

## Distributing a game

Follow the [licence-notice workflow](../../../docs/licensing.md). Include this
crate's [bundled-code notices](../THIRD_PARTY_NOTICES.txt), the game's resolved
Cargo dependency notices and any game-asset licences in the shipped files.

Build the browser game with a profile of its own (Cargo ignores a
dependency's profile), run `wasm-bindgen --target web` on the module without
`wasm-opt`, and serve the `.wasm` brotli-encoded, or gzip where the server
cannot:

```toml
[profile.web]          # cargo build --profile web --target wasm32-unknown-unknown
inherits = "release"
lto = "fat"
codegen-units = 1
opt-level = "s"
strip = "symbols"
```

[Browser](../README.md#browser-wasm--webgpu) gives its measured effect on the
download and on CPU time.

## Math types

Use `sgl_3d::glam`; SGL3D, `sgl-2d` and `sgl-core` share the workspace glam
dependency. Matching vectors and matrices pass directly between packages.
[Migration notes](../../../CHANGELOG.md) cover upgrades from the earlier split.
