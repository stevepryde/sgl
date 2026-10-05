# SGL3D for game agents

Start here when building or changing a game that renders with SGL3D
(`sgl-3d`). These pages say what SGL3D offers and how to use it, then point to
the code. They are kept current with every feature change; the code and the
[package README](../README.md) hold the detail.

- [Features](features.md): what SGL3D renders and where each feature's API is.
- [Settings](settings.md): every rendering setting a game can choose, with
  values, defaults and preset behaviour.

## What SGL3D is

A Rust/wgpu 3D renderer that a game links as a library, native or in the
browser (WASM + WebGPU): both are first-class targets, running the same code.
Native takes every feature the device supports; the browser falls back or goes
without where it cannot, and the effective configuration reports which.
The game owns the window or canvas, event loop, input, simulation, camera, UI,
content and settings file.
SGL3D keeps GPU resources and encodes each frame into the game's command
encoder. It is not an engine: there is no ECS, editor, scene graph, scripting
or asset pipeline, and none should be added around it.

## A frame

A game holds three values: a `Scene` (content), a `Renderer` (the frame) and
its rendering `settings::Settings`.

1. Device: request `graphics_device::limits(&adapter)`,
   `graphics_device::features(&adapter)` and
   `graphics_device::fsr2_features(&adapter)`, plus `TEXTURE_COMPRESSION_BC`
   for compressed bakes and material textures and `TIMESTAMP_QUERY` for GPU timing.
   In the browser the device is the page's WebGPU one, requested the same way;
   it needs 21 sampled textures per stage (Chromium 149 and later).
2. Load: `Scene::new` starts empty. Add content between frames and keep the
   identities it returns: `asset::load` (a file) or `asset::load_slice` (bytes,
   as a browser fetches them) into
   `Scene::add_asset`, instances with `add_instance` (static or moving),
   point, spot and rectangle lights with `add_light`, decals with
   `add_decal_image` and `add_decal`, environments with
   `add_environment`, an irradiance volume with `set_irradiance_volume`
   (its cells written by region with `write_irradiance_cells`), and a
   dynamic GI volume with `set_dynamic_gi_volume`. `Renderer::new` takes the output
   format, the output size in physical pixels, the window's scale factor and
   the settings.
3. Each frame: `Scene::set_instance` per moving instance (pose, `visible`,
   `capture_visible`), `set_instance_deformation` per deforming one (its
   joint matrices and morph weights), `set_light` per changed light and
   `set_decal` per moved decal; `Renderer::resize` (nothing happens unless the size, the scale or a
   setting that [applies at resize](settings.md#when-changes-apply) changed);
   a `FrameInput` with the camera, the authored look and the presentation
   time in seconds (`elapsed_seconds`, an `f64`, which moves materials'
   normal layers and the mist); `Renderer::render`
   into the output view. Draw the game's UI (`sgl-2d` can, on the same
   device), submit, and call `Renderer::finish_frame`.
4. Set `FrameInput::camera_cut` on a camera cut. History restarts itself then,
   after a resize that changes the targets and for a different scene.
5. A large world: keep the game's own coordinates and call
   `Scene::move_origin` to keep what it renders near the render origin
   (chunk-aligned in a streamed world); then give the camera and edits in
   the new frame. Nothing restarts or redraws.

[`examples/offscreen.rs`](../examples/offscreen.rs) is the loop to copy
(`--decals` adds decals, `--motion-blur` motion blur, `--fog` volumetric
fog);
[`examples/skinned.rs`](../examples/skinned.rs) adds a skinned, morphed glTF
and the game's side of animating it,
[`examples/instances.rs`](../examples/instances.rs) many instances of a few
models, [`examples/water.rs`](../examples/water.rs) a lake whose
animated surface receives screen-space reflections,
[`examples/streaming.rs`](../examples/streaming.rs) a block world streamed
in 16 m chunks about a moving camera, edited and remeshed, with the render
origin following it, and what each scene operation costs, and
[`examples/dynamic_gi.rs`](../examples/dynamic_gi.rs) a room lit by a dynamic
GI volume, and [`examples/irradiance_volume.rs`](../examples/irradiance_volume.rs)
a block world's cave lit by the game's own light field, relit by region. [`examples/browser_smoke.rs`](../examples/browser_smoke.rs) is the
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
| A feature's API, data format and limits | Its section in the [package README](../README.md) |
| Rendering rules and roadmap | [SGL3D spec](../../../specs/sgl3d.md) |
| How SGL3D's code is structured, for changing SGL3D itself | [SGL3D architecture](../../../specs/sgl3d-architecture.md), [code structure](../README.md#code-structure) |
| Why something is the way it is | [Decisions](../../../specs/decisions.md) |
| Public types | `src/lib.rs` (the public surface), `src/scene/mod.rs` (`Scene`), `src/renderer/mod.rs` (`Renderer`), `src/frame_input.rs` (`Camera`, `FrameInput`), `src/settings.rs` (`Settings`), `src/content/` (content types) |

## Steering

- Keep game policy in the game: presets, settings UI, save format, content,
  lighting design, cameras and the frame budget.
- Give SGL3D final presentation poses after the game's own interpolation;
  it keeps motion history itself.
- Treat lightmaps, irradiance atlases and specular probes as content. The
  game bakes them with explicit export commands and installs them at load;
  SGL3D never writes files. Rebake only when lighting content or a bake format
  changes, never for dependency or code updates.
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
  `Renderer::antialiasing_in_effect(&settings)` and `fsr2_error()` say when
  the device fell back (FSR2 always does in the browser).
- In the browser, pass what a native game would read: asset bytes, and the
  frame time in `FrameInput::frame_time_ms`. Capture probes natively; the
  capture's blocking readback fails on WebGPU.
- Measure cost per pass with `timing::GpuTiming` before and after a change.
- Need a rendering feature SGL3D lacks? Build it in SGL3D, not in the game,
  following the spec's
  [rendering development rules](../../../specs/sgl3d.md#rendering-development)
  and the [architecture](../../../specs/sgl3d-architecture.md): port a proven,
  permissively licensed implementation into its place in the stage order.

## Distributing a game

Follow the [licence-notice workflow](../../../docs/licensing.md). Include this
crate's [bundled-code notices](../THIRD_PARTY_NOTICES.txt), the game's resolved
Cargo dependency notices and any game-asset licences in the shipped files.

## Math types

Use `sgl_3d::glam`; SGL3D, `sgl-2d` and `sgl-core` share the workspace glam
dependency. Matching vectors and matrices pass directly between packages.
[Migration notes](../../../CHANGELOG.md) cover upgrades from the earlier split.
