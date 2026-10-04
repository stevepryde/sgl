# sgl-2d

Rust/wgpu rendering for 2D games and for HUDs and UI over a 3D scene. Includes
sprite rendering, a logical canvas, texture assets, Aseprite sheets, text,
lighting, overlays, and immediate-mode widgets. The game owns its window or
canvas, event loop, simulation, input translation, and UI layout.

## Choose a rendering path

- [`render`](src/render/mod.rs): compact sprite batches with caller-supplied
  transforms. Start with the [direct game](../../examples/direct-game/src/main.rs),
  which owns winit and draws a sprite: `cargo run -p sgl-direct-game`.
- [`canvas`](src/canvas.rs): a logical-resolution scene, world and screen
  draw channels, text, lighting, overlays, and letterbox presentation.
  Start with the [tool UI example](examples/tool_ui.rs):
  `cargo run -p sgl-2d --example tool_ui`.

Both paths keep composition in the game. Choose the one that matches the
required drawing model; do not wrap them in another engine layer.
For resource lifetimes, color spaces, and presentation behavior, read the
[2D rendering contract](../../specs/rendering.md).

## Assets and text

[`assets`](src/assets.rs) owns texture handles and CPU texture loading;
[`aseprite`](src/aseprite.rs) reads exported sheet metadata. Games own paths,
content schemas, and animation policy. The canvas text renderer rasterizes
TTF glyphs into atlas pages; upload pages it marks changed after `end_frame`.
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
after the 3D scene. Follow the [SGL3D frame lifecycle](../sgl-3d/docs/README.md#a-frame)
and [3D integration conventions](../../docs/3d-development.md).
SGL3D's glam re-export and the 2D math types are different versions; convert
through arrays where needed.

Run `cargo test -p sgl-2d` for focused checks, including GPU tests when an
adapter is available. The [required check](../../CONTRIBUTING.md#validate)
adds the repository's platform coverage.
For dependency setup, see [Building games with SGL](../../docs/README.md).
