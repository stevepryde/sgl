# sgl-2d

Rust/wgpu rendering for 2D games and for HUDs and UI over a 3D scene. Includes
sprite rendering, a logical canvas, texture assets, Aseprite sheets, text,
lighting, overlays, and immediate-mode widgets. The game owns its window or
canvas, event loop, simulation, input translation, and UI layout.

## Rendering

[`canvas`](src/canvas.rs) renders a logical-resolution scene with world and
screen draw channels, text, lighting, overlays, and letterbox presentation.
Start with the [direct game](../../examples/direct-game/src/main.rs), which
owns winit and draws sprites: `cargo run -p sgl-direct-game`. It also builds
for `wasm32-unknown-unknown`, but the repository has no page that runs it. The [tool UI example](../../examples/sgl-2d/examples/tool_ui.rs)
adds text and widgets: `cargo run -p sgl-2d-examples --example tool_ui`.

The game keeps composition; do not wrap the canvas in another engine layer.
For resource lifetimes, color spaces, and presentation behavior, read the
[2D rendering contract](../../specs/rendering.md).
`Context` caps initial and resized swapchain dimensions per axis to the device's
maximum 2D texture dimension; `Context::size` reports the configured dimensions.
`Renderer::read_scene` and `capture_scene` block for their readback and are native-only; a browser build
does not have them. Shadow geometry shares one buffer across shadowed
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

For HUDs over SGL3D, share the game-owned wgpu device and queue and draw UI
after the 3D scene. A HUD that drives `canvas::sprite::SpritePass` itself
keeps one pass: `upload` adds a texture or replaces a known handle's pixels
(an invalid texture returns a `TextureError`), `upload_normal` registers a
diffuse's normal map after its `upload` or replaces it under a different
handle, and `draw_stats` reports the draws `draw_screen` encodes. Follow the
[SGL3D frame lifecycle](../sgl-3d/docs/README.md#a-frame)
and [3D integration conventions](../../docs/3d-development.md).
`sgl_core::math` and SGL3D's `sgl_3d::glam` re-export the same workspace glam
types, so matching vectors and matrices pass between them directly.

Run `cargo test -p sgl-2d -p sgl-2d-tests` for focused checks, including GPU tests when an
adapter is available. The [required check](../../CONTRIBUTING.md#validate)
adds the repository's platform coverage.
For dependency setup, see [Building games with SGL](../../docs/README.md).
