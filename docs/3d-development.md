# Developing 3D games with SGL

Use Rust/wgpu through `sgl-3d` for new 3D clients, native or in the browser
(WASM + WebGPU); both are first-class targets. The
[SGL3D contract](../specs/sgl3d.md) owns the requirements; its
[package README](../crates/sgl-3d/README.md) describes resource and frame use.
SGL3D is part of SGL, with the game owning composition. It adds no editor,
ECS, scene authoring application, or gameplay language.

## Composition

The game owns its executable, window or canvas and events, input, simulation
and networking clocks, camera behavior, UI, content, and save schema. It loads or builds CPU
assets and supplies the renderer with final presentation transforms, lighting,
environment maps, and settings. SGL3D retains GPU resources and encodes rendering
into the game's command encoder. It neither creates a window nor handles input;
use game-owned winit or another compatible window integration, or in the
browser a canvas surface on the page's WebGPU device. Request the device with
`graphics_device::limits` and `graphics_device::features` on either. A
native game that opts in to hardware ray tracing also requests
`graphics_device::ray_tracing_features` with wgpu's experimental token and
turns `Settings::hardware_ray_tracing` on, and `Settings::ray_traced_shadows`
for ray-traced shadows.
In the browser, fetch asset bytes and load them with `asset::load_slice` or
`asset::load_slice_with_options`, pass the measured frame time in
`FrameInput::frame_time_ms`, and capture specular probes natively:
`Renderer::capture_specular_probe` blocks for readback, which WebGPU cannot,
so the browser loads the baked result.

Keep metres, +Y up, and a right-handed camera looking along local -Z. Adapt
source conventions once at the game boundary. Use `sgl_3d::glam` for renderer
matrices. The 2D and 3D crates share the workspace glam types; matching
vectors and matrices can cross the UI/3D boundary directly. Use the wgpu
dependency specified by the [workspace manifest](../Cargo.toml) when sharing
the game-owned device and queue. All dependency versions live in that manifest;
package manifests select features. Linear light values and HDR buffers must remain
linear until final presentation conversion.

Load geometry and textures once per scene through `asset::load` or
`asset::load_slice`. Select named rigid parts with `LoadOptions::nodes`;
keep naming conventions in the game predicate. Skinned and morphed assets
bring their rig and clips as plain data (`Asset::rig`); the game samples and
blends clips and poses each deforming instance every frame with
`Scene::set_instance_deformation`. Do not maintain a second glTF
decoder for embedded models or portraits. Portraits can compose the same
imported assets with a `Scene` and a `Renderer` under a game-owned studio camera.
Matching Blender images also requires matching lighting and color management.

Each rendered frame, update retained instances, call `Renderer::resize` and
`Renderer::render` with that frame's `FrameInput`, submit, then call
`Renderer::finish_frame`. The game owns interpolation; SGL3D records previous
rendered poses for motion and reflections, and `finish_frame` advances that
history once the frame is submitted. History restarts itself on
`FrameInput::camera_cut`, a resize that changes the targets and a different
scene. Do not advance simulation from the renderer. For rigid bodies, see
[Physics](README.md#physics).

Persist `settings::Settings` inside the game's own settings record. Keep an
explicit value separate from `Preset` and resolve it without rewriting the saved
choice. Switching presets or rendering on an adapter with fewer capabilities
must preserve explicit quality, reflection, and precision choices. Present the
effective capability honestly and leave the highest implemented fidelity
selectable. Check the package's current limits before promising platform or
asset support.

For rendering changes, follow the
[SGL3D rendering development rules and roadmap](../specs/sgl3d.md#rendering-development):
build foundations first, port proven permissively licensed code, delete what it
supersedes, and measure per-pass GPU time. Changes to SGL3D itself keep to the
[SGL3D architecture](../specs/sgl3d-architecture.md). Use captures from the real game to
iterate; the owner judges the look.
