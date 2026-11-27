# sgl-2d

Rust/wgpu rendering for 2D games and for HUDs and UI over a 3D scene. Includes
sprite rendering, a logical canvas, texture assets, Aseprite sheets, text,
lighting, overlays, and immediate-mode widgets. The game owns its window or
canvas, event loop, simulation, input translation, and UI layout.

## Rendering

[`canvas`](src/canvas.rs) renders a logical-resolution scene with world and
screen draw channels, camera translation/zoom/`Camera::rotation`, text
(including `TextRenderer::draw_rotated`), lighting, overlays, and letterbox
presentation.
Start with the [direct game](../../examples/direct-game/src/main.rs), which
owns winit and draws sprites: `cargo run -p sgl-direct-game`. It also builds
for `wasm32-unknown-unknown`, but the repository has no page that runs it. The [tool UI example](../../examples/sgl-2d/examples/tool_ui.rs)
adds text and widgets: `cargo run -p sgl-2d-examples --example tool_ui`.

The game keeps composition; do not wrap the canvas in another engine layer.
For resource lifetimes, color spaces, and presentation behavior, read the
[2D rendering contract](../../specs/rendering.md).
In the browser `Context` and `Gpu::headless_async` create their wgpu instance
on [sgl-webgpu](../sgl-webgpu/README.md); a game that creates its own device
there uses `sgl_webgpu::instance` too.
The device has default limits and no optional features unless the game
requests it: `Context::try_new_with` / `Gpu::headless_with` (and their
`_async` forms) pass the selected adapter to the game's closure and request
the `wgpu::DeviceDescriptor` it returns, so one device can serve the canvas
and SGL3D (`sgl_3d::graphics_device::{limits, features}`). A device the game
negotiated itself wraps as `Gpu { device, queue }` for offscreen rendering.
`Context` caps initial and resized swapchain dimensions per axis to the device's
maximum 2D texture dimension; `Context::size` reports the configured dimensions.
`Renderer::read_scene_async` reads the scene back on either platform; its
blocking form `read_scene` and `capture_scene` (a PNG at a path) are
native-only. Shadow geometry shares one buffer across shadowed
lights, bounded by the device's `max_buffer_size` and u32 draw range. When
an entire occluder edge does not fit, it and subsequent edges are dropped;
lights with no retained shadow geometry render unshadowed.

## Assets and text

[`assets`](src/assets.rs) owns texture handles and CPU texture loading;
[`aseprite`](src/aseprite.rs) reads exported sheet metadata. Games own paths,
content schemas, and animation policy. Hash-export frame keys accept trailing
ASCII indices with or without an extension, such as `hero 0.aseprite`,
`hero_0.png`, `hero.v2_0`, and `v1.2 0`; dotted titles are preserved. The
loader reads trailing digits first, stripping a final extension only when
the key does not end in digits. Indices must cover `0..len` exactly; a
single-frame export may omit the index. The canvas text renderer rasterizes
TTF glyphs into atlas pages; upload pages it marks changed after `end_frame`
to the same renderer or sprite pass, which updates them in place.
See the [client contract](../../specs/client.md) for handle identity, loader
limits, text, and coordinate conventions.

## UI and HUDs

[`ui`](src/ui.rs) is immediate mode. Build the controls each frame with stable
names and game-owned values. Translate platform events into `UiInput`, then
respect keyboard and pointer capture before dispatching world actions.
Keep focus-loss handling, clipboard access, layout, and persistence in the
game. Clearable line and password edits keep text and the caret left of their
visible clear button. Numeric fields scrub from horizontal travel; a drag on
either axis prevents the click from opening typing. Oversized numeric typing
drafts use scientific notation to leave room for edits. [Tool composition](../../specs/client.md#tool-composition) documents
pane layout, themes, input ordering, clipping, and keyboard behavior.
Custom `overlay_panel_begin` controls remain below and pointer-blocked by
open dropdown popovers and confirmation modals, even when submitted later.
Ordinary widgets stay below these layers regardless of list length. Draw
cursors saturate within their bands after millions of steps; equal-z draws
retain screen-list push order.

For a HUD over SGL3D, request one device for both renderers through
`Context::try_new_with` (or `Gpu::headless_with` offscreen) with
`sgl_3d::graphics_device::{limits, features}`. Create the SGL3D `View` with
`canvas::blit::SCENE_FORMAT` at `Renderer::target_size` and render it into
`Renderer::scene_view`, fetched again after `set_target_size`.
`Renderer::render_screen` draws the list's
screen channel over it, and `Renderer::blit_scene` then `Frame::present`
shows it letterboxed. `read_scene_async` (or native `read_scene` /
`capture_scene`) reads back that same image, with or without a surface
frame.

```rust
let ctx = Context::try_new_with(window, true, |adapter| wgpu::DeviceDescriptor {
    required_features: sgl_3d::graphics_device::features(adapter),
    required_limits: sgl_3d::graphics_device::limits(adapter),
    ..Default::default()
})?;
let (width, height) = canvas.target_size(); // canvas: sgl_2d::canvas::Renderer
let mut view = View::new(&renderer, &ctx.device, &ctx.queue, SCENE_FORMAT, [width, height], 1., &settings)?;

// Each frame:
let mut encoder = ctx.device.create_command_encoder(&Default::default());
let output = ViewOutput::Texture(canvas.scene_view());
renderer.render(&ctx.device, &ctx.queue, &mut encoder, &mut scene,
    &mut [ViewFrame { view: &mut view, input: &input, settings: &settings, output, timing: None }]);
ctx.queue.submit([encoder.finish()]);
renderer.finish_frame(&mut scene);
canvas.render_screen(&ctx, &mut hud);
if let Some(frame) = ctx.acquire() {
    canvas.blit_scene(&ctx, &frame);
    frame.present();
}
```

A game that drives its own pass can still use `canvas::sprite::SpritePass`
directly ([rendering](../../specs/rendering.md) requirements 2 and 5).
Follow the [SGL3D frame lifecycle](../sgl-3d/docs/README.md#a-frame)
and [3D integration conventions](../../docs/3d-development.md).
`sgl_core::math` and SGL3D's `sgl_3d::glam` re-export the same workspace glam
types, so matching vectors and matrices pass between them directly.

Run `cargo test -p sgl-2d -p sgl-2d-tests` for focused checks, including GPU tests when an
adapter is available. The [required check](../../CONTRIBUTING.md#validate)
adds the repository's platform coverage.
For dependency setup, see [Building games with SGL](../../docs/README.md).
