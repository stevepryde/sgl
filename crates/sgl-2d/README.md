# sgl-2d

Rust/wgpu rendering for 2D games and for HUDs and UI over a 3D scene. Includes
sprite rendering, a logical canvas, texture assets, Aseprite sheets, text,
lighting, overlays, and immediate-mode widgets. The game owns its window or
canvas, event loop, simulation, input translation, and UI layout.

## Rendering

[`canvas`](src/canvas.rs) renders a logical-resolution scene with world and
screen draw channels, text, lighting, overlays, and letterbox presentation.
Start with the [direct game](../../examples/direct-game/src/main.rs), which
owns winit and draws a sprite on native and browser:
`cargo run -p sgl-direct-game`. The [tool UI example](examples/tool_ui.rs)
adds text and widgets: `cargo run -p sgl-2d --example tool_ui`.

The game keeps composition; do not wrap the canvas in another engine layer.
For resource lifetimes, color spaces, and presentation behavior, read the
[2D rendering contract](../../specs/rendering.md).

## Assets and text

[`assets`](src/assets.rs) owns texture handles and CPU texture loading;
[`aseprite`](src/aseprite.rs) reads exported sheet metadata. Games own paths,
content schemas, and animation policy. The canvas text renderer rasterizes
TTF glyphs into atlas pages; upload pages it marks changed after `end_frame`
to the same renderer or sprite pass, which updates them in place.
See the [client contract](../../specs/client.md) for handle identity, loader
limits, text, and coordinate conventions.

## UI and HUDs

[`ui`](src/ui.rs) is immediate mode. Build the controls each frame with stable
names and game-owned values. Translate platform events into `UiInput`, then
respect keyboard and pointer capture before dispatching world actions.
Keep focus-loss handling, clipboard access, layout, and persistence in the
game. [Tool composition](../../specs/client.md#tool-composition) documents
pane layout, themes, input ordering, clipping, and keyboard behavior.

For HUDs over SGL3D, share the game-owned wgpu device and queue and draw UI
after the 3D scene. A HUD that drives `canvas::sprite::SpritePass` itself
keeps one pass: `upload` adds a texture or replaces a known handle's pixels,
and `draw_stats` reports the draws `draw_screen` encodes. Follow the
[SGL3D frame lifecycle](../sgl-3d/docs/README.md#a-frame)
and [3D integration conventions](../../docs/3d-development.md).
`sgl_core::math` and SGL3D's `sgl_3d::glam` re-export the same workspace glam
types, so matching vectors and matrices pass between them directly.

Run `cargo test -p sgl-2d` for focused checks, including GPU tests when an
adapter is available. The [required check](../../CONTRIBUTING.md#validate)
adds the repository's platform coverage.
For dependency setup, see [Building games with SGL](../../docs/README.md).
