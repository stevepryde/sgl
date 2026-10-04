# Architecture

SGL is a game library a game imports. The game owns the executable,
the loop, the world, and the rules. SGL consolidates reusable implementations
from working games. 3D clients, native and in the browser, use Rust/wgpu
through SGL3D, extracted from Hyperdrive. Existing 2D games retain their
caller-driven wgpu path.

## Design audience and origins

AI coding agents are the primary consumers and authors. Public boundaries
must make ownership, inputs, outputs, and lifecycle explicit. Package guides
and runnable examples must be usable without access to the source games.
[The consumer guide](../docs/README.md) routes agents by task; the root README
introduces the project to people.

SGL began by consolidating recurring code from AI-authored games:
elemental_chaos (multiplayer action), shadow-sp (2D platforming), tiny / Kindred
Acre (persistent co-op farming), and torchmates (co-op action adventure).
Hyperdrive supplied the 3D renderer. Their shared patterns informed the
library; their rules, content, and process layouts stay with the games.

## Requirements

1. The game owns composition: window, timing, simulation, content, messages,
   authority, UI layout, and process layout.
2. SGL owns reusable, game-independent pieces extracted from the source games.
   When those games share a pattern, SGL should become the copy they call.
3. Each package must own an implemented reusable boundary. New modules appear
   when extracted code needs a home.

   - `sgl-3d` (SGL3D) — retained 3D assets, PBR, lighting, shadows,
     reflections, post-processing, and rendering measurements, on native and
     browser (WebGPU) devices
   - `sgl-post-fx` — SGL's wgpu post-processing effects, including screen-space
     reflections and temporal anti-aliasing derived from DiligentFX; SGL3D
     owns scene and frame integration
   - `sgl-2d` — assets, immediate UI, and a caller-driven logical-canvas wgpu
     renderer
   - `sgl-input` — controller discovery and normalized events/state, independent
     of rendering; games retain focus, assignment and binding policy
   - `sgl-net` — opaque UDP, WebSocket, in-memory, and simulated transports
   - `sgl-core` — deterministic grid, hash, time, animation, collision, math,
     and RNG helpers
4. Native client, browser client, and headless server are all valid consumers.
   A server must be able to depend on `sgl-net` and `sgl-core` without wgpu or
   winit.
   `sgl-3d` must not require `winit`, input handling, `sgl-net`, or a game crate.
   Its wgpu backend may depend on platform graphics libraries. SGL3D and the
   ports it runs build for `wasm32-unknown-unknown` with wgpu's WebGPU backend
   ([D-20](decisions.md)).
5. Extract from working game code. Prefer one good implementation over four
   variants. Helpers take the game's tick rate, canvas size, wire identity,
   and types as parameters rather than baking one game's constants.

For 3D contracts, see [SGL3D](sgl3d.md); for its internal structure,
[SGL3D architecture](sgl3d-architecture.md). For consumer practice, see
[3D development](../docs/3d-development.md). SGL3D is the one 3D renderer, on
native and in the browser.

## Shared patterns

These already exist in the source games. They are the extraction list, not a
frozen SGL API.

- wgpu sprite batch, nearest sampling, persistent GPU resources
- Game-built draw list consumed by the renderer (world and screen channels)
- Logical canvas with letterbox blit to the swapchain
- CPU PNG decode and texture handles; renderer uploads RGBA8
- Texture atlas packing
- Fixed-step accumulator with interpolation alpha
- Frame animation from an explicit sheet-frame order and fps
- Camera
- Collision primitives (AABB sweep, tiles, circles and rects)
- 2D lighting and darkness composite
- Immediate-mode UI on the screen draw list
- Glyph and text rendering
- Two delivery classes: reliable-ordered and latest-state
- Native UDP, browser WebSocket, in-memory duplex, server mux
- Caller-supplied time on transport poll and flush
- Deterministic RNG and canonical hashing
- RON for authored data, postcard for wire payloads

Gameplay, entity layout, maps, items, protocol enums, prediction policy, save
schema, rooms, and screen flow stay in the game.

## Acceptance

- `examples/direct-game` owns winit and renders without linking `sgl-net`.
- `sgl-net` builds without wgpu or winit.
- Native workspace checks and the WASM lane, SGL3D included, pass.
- SGL3D is consumed through game-supplied geometry, lighting, camera data,
  and settings; it has no dependency on Hyperdrive rules or content.
