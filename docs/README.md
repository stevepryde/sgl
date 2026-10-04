# Building games with SGL

Start here when an AI coding agent is building a game with Steve's Game
Library. Read the guide from the same release or revision as the dependencies. Load the
package guide and relevant source as needed; the full specs and rendering
reference do not need to be in every prompt.

## Choose a starting point

| Task | Read | Working example or API |
| --- | --- | --- |
| Build a 3D game, native or browser | [SGL3D agent guide](../crates/sgl-3d/docs/README.md), [integration conventions](3d-development.md) | [Offscreen frame](../crates/sgl-3d/examples/offscreen.rs), [WebGPU frame](../crates/sgl-3d/examples/browser_smoke.rs) |
| Build a 2D game | [sgl-2d](../crates/sgl-2d/README.md) | [Minimal window and sprite](../examples/direct-game/src/main.rs) |
| Add a HUD, menu, or editing tool | [sgl-2d UI](../crates/sgl-2d/README.md#ui-and-huds), [tool composition](../specs/client.md#tool-composition) | [Tool UI](../crates/sgl-2d/examples/tool_ui.rs) |
| Add fixed-step simulation, collision, or deterministic helpers | [sgl-core](../crates/sgl-core/README.md) | [Public modules](../crates/sgl-core/src/lib.rs) |
| Add multiplayer transport | [sgl-net](../crates/sgl-net/README.md) | [Transport API](../crates/sgl-net/src/lib.rs) |
| Add controller support | [sgl-input](../crates/sgl-input/README.md) | [Controller example](../crates/sgl-input/examples/controllers.rs) |
| Work directly with GPU post-effects | [sgl-post-fx](../crates/sgl-post-fx/README.md) | [SGL3D integration](../crates/sgl-3d/src/view/post_fx.rs) |

## Add SGL to a game

Use only the crates your game needs. Once the initial `0.1.0` release has been
published to crates.io:

```toml
[dependencies]
sgl-core = "0.1.0"
sgl-2d = "0.1.0"
```

Until then, or when changing SGL alongside a game, use a sibling checkout:

```toml
[dependencies]
sgl-core = { path = "../sgl/crates/sgl-core" }
sgl-2d = { path = "../sgl/crates/sgl-2d" }
```

Or use Git dependencies:

```toml
[dependencies]
sgl-core = { git = "https://github.com/stevepryde/sgl.git" }
sgl-2d = { git = "https://github.com/stevepryde/sgl.git" }
```

Cargo records the resolved revision in the game's `Cargo.lock`; commit that
lockfile. To select a particular revision, add the same `rev` to every SGL
Git dependency. There is no umbrella crate or installation step for SGL.
Bun and Playwright are repository development tools, not game dependencies.

Use `sgl_3d::glam` for 3D math and `sgl_core::math` for shared 2D math.
The crates currently use different glam versions; convert via arrays where
they meet. See [3D conventions](3d-development.md) before sharing a GPU device
or composing 2D UI over a 3D frame.

## Keep ownership explicit

- The game owns its window or canvas, event loop, input bindings, clocks,
  simulation, camera behavior, UI layout, content, protocol, and persistence.
- Game logic is Rust. Use RON for authored gameplay data; keep schemas in the
  game. Asset formats follow the relevant loader's documented support.
- SGL consumes caller-supplied data and retains the resources its API owns.
  Use the existing loaders, renderer, widgets, and transports before adding
  equivalents in the game.
- Headless servers can use `sgl-core` and `sgl-net` without graphics or windowing.
  Browser 3D requires WebGPU; WebGL2 is unsupported. Controller support and
  native bake/readback operations have their own platform limits.

## Work from a concrete example

1. Choose the smallest relevant example and get its existing workflow running.
2. Read the package's current API and limits before designing around a feature.
   [SGL3D features](../crates/sgl-3d/docs/features.md) and
   [settings](../crates/sgl-3d/docs/settings.md) distinguish available choices.
3. Implement game policy in the game. If a reusable SGL boundary is missing,
   describe the requirement against the [owning spec](../specs/README.md).
4. Validate the game on its intended targets. A native build does not exercise
   browser I/O, WebGPU, or physical controller behavior.

When changing SGL itself, follow [AGENTS.md](../AGENTS.md) and
[Contributing](../CONTRIBUTING.md). Specs define required behavior; package
guides explain usage; source defines exact signatures. Resolve disagreements
explicitly rather than silently changing the contract to match an accident.
