# Rendering

`sgl-2d` renders through one caller-driven wgpu renderer, `canvas`. It
accepts a game-built `DrawList` and `LightFrame`, renders at a logical
resolution, and adds atlasing, letterboxing, camera transforms, text, shadows,
and lighting.
The game owns winit and decides when to update and draw. Lighting composites
in gamma space by default for Godot parity; `LightingSpace::Linear` is the
opt-in linear contract ([D-8](decisions.md)).

CPU-side texture loading and handles live in `assets`, including
the shared 1×1 white texture that flat quads scale up; overlay primitives
(lines, rect outlines, fills, circles) built from it live in `canvas::overlay`,
and immediate-mode widgets that emit screen-channel draw commands live in `ui`.
Their CPU-side contracts are in [client](client.md).

## Requirements

1. The game creates its window, brings the GPU up with `canvas::Context`
   (surface, adapter, device, configured surface), forwards resize events, and
   calls the renderer from its own redraw path. A failed bring-up returns a
   `RendererInitError` the game can show (`Context::try_new` /
   `try_new_async`); `Context::new` / `new_async` panic with it.
2. The game uploads decoded straight-alpha RGBA8 textures explicitly under
   their asset handles. Rendering does not load files or decode images.
   Uploading a handle that is already uploaded replaces its pixels.
   A texture can be replaced under its existing handle: equal dimensions
   preserve its placement and registered normal map, while changed dimensions
   relocate it and detach the now-incompatible normal map. Replacing a handle
   used as a normal map updates every size-compatible diffuse association and
   detaches each incompatible one.
3. A frame supplies game-owned draw and light data at the configured logical
   size. The camera may carry a pixels-per-unit / y-up world convention so the
   game pushes world-unit positions and sizes and the renderer applies the
   pixel seam; the default remains y-down logical pixels.
4. Pipelines, textures, the camera uniform, quad data, and instance allocation
   persist across frames. The instance buffer grows only when required.
5. Adjacent sprites using the same texture share a draw call. Nearest sampling
   and premultiplied-alpha blending are the current defaults.
6. Minimized or transiently unavailable surfaces skip a frame. The renderer
   does not request another redraw or advance game state.
7. The renderer renders and reads back its offscreen scene without a
   window: a `Gpu` (device + queue, which a windowed `Context` derefs to)
   is enough for uploads, `render_scene`, `read_scene`, and `capture_scene`.
   Only the swapchain blit needs a surface.

## Acceptance

- `examples/direct-game` implements `ApplicationHandler` itself on native and wasm.
- Repeated frames do not recreate a pipeline, sampler, texture, or window.
- Fixed headless scenes match the committed golden frames in
  `crates/sgl-2d/tests/golden/` in both lighting spaces.
- A renderer-only consumer does not link networking.
