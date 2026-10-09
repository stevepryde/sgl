# Building games with SGL

Start here when an AI coding agent is building a game with Steve's Game
Library. Read the guide from the same release or revision as the dependencies. Load the
package guide and relevant source as needed; the full specs and rendering
reference do not need to be in every prompt.

## Choose a starting point

| Task | Read | Working example or API |
| --- | --- | --- |
| Build a 3D game, native or browser | [SGL3D agent guide](../crates/sgl-3d/docs/README.md), [integration conventions](3d-development.md) | [Offscreen frame](../crates/sgl-3d/examples/offscreen.rs), [WebGPU frame](../crates/sgl-3d/examples/browser_smoke.rs) |
| Give a 3D surface its own vertex and surface functions (water, wind, glass) | [Programmable surfaces](../crates/sgl-3d/README.md#programmable-surfaces) | [Water](../crates/sgl-3d/examples/water.rs), [wind and glass](../crates/sgl-3d/examples/shaders.rs), [glass volumes](../crates/sgl-3d/examples/volumes.rs) |
| Build a 2D game | [sgl-2d](../crates/sgl-2d/README.md) | [Minimal window and sprite](../examples/direct-game/src/main.rs) |
| Add a HUD, menu, or editing tool | [sgl-2d UI](../crates/sgl-2d/README.md#ui-and-huds), [tool composition](../specs/client.md#tool-composition) | [Tool UI](../crates/sgl-2d/examples/tool_ui.rs) |
| Add fixed-step simulation, collision, or deterministic helpers | [sgl-core](../crates/sgl-core/README.md) | [Public modules](../crates/sgl-core/src/lib.rs) |
| Choose a game's physics | [Physics](#physics) | [Arcade collision](../crates/sgl-core/src/collision.rs) |
| Add multiplayer transport | [sgl-net](../crates/sgl-net/README.md) | [Transport API](../crates/sgl-net/src/lib.rs) |
| Add controller support | [sgl-input](../crates/sgl-input/README.md) | [Controller example](../crates/sgl-input/examples/controllers.rs) |
| Work directly with GPU post-effects | [sgl-post-fx](../crates/sgl-post-fx/README.md) | [SGL3D integration](../crates/sgl-3d/src/view/post_fx.rs) |

## Add SGL to a game

Use only the crates your game needs, all at the same version:

```toml
[dependencies]
sgl-core = "0.3.0"
sgl-2d = "0.3.0"
```

When changing SGL alongside a game, use a sibling checkout:

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
Both re-export the same workspace glam types, so matching vectors and matrices
can be passed directly between packages. See [3D conventions](3d-development.md)
before sharing a GPU device or composing 2D UI over a 3D frame.

## Physics

SGL has no physics engine. Physics belongs to the game; choose it by what
the game is.

- Most games, including driving, platformers and shooters, and especially
  multiplayer games, are better with custom arcade physics: movement rules
  written for the game's feel, which stay tunable, cheap, deterministic, and
  easy to predict and roll back over the network. A rigid-body solver's
  emergent behaviour fights designed handling. Collide against the game's own
  world model, such as a track spline, road, voxel grid or boxes, not its
  render meshes. In 2D, [`sgl_core::collision`](../crates/sgl-core/README.md)
  sweeps and slides boxes. SGL3D's ray queries serve rendering only; keep
  gameplay queries in the simulation, which a headless server runs without a
  GPU. A query library such as [parry3d](https://parry.rs) suits a game that
  must collide with arbitrary meshes.
- Use a rigid-body engine only when simulated physics is the game: stacking,
  destruction, physics puzzles or sandboxes. Add [Rapier](https://rapier.rs)
  (`rapier3d` or `rapier2d`). Its vectors and rotations, like parry's, are
  glam types, so they pass to and from `sgl_3d::glam` and `sgl_core::math`
  directly. In 3D, use metres and gravity along -Y to match SGL3D. Enable its
  `enhanced-determinism` feature when targets or a server and its clients
  must simulate identically.

Step physics inside the game's fixed-step loop (set Rapier's
`IntegrationParameters::dt` to `FixedClock::fixed_dt()`) and copy the resulting
poses to instances or sprites when presenting.

## Distributing a game

Follow [shipping licence notices](licensing.md). Generate the combined notices
for the game's locked dependencies and build selection, include licences for
its assets, and ship the files with the native build or browser site. Carry
that guide's game-agent rules into the game's own `AGENTS.md` and packaging
workflow so future updates preserve them.

## Updating a game

SGL expects AI agents to keep games current. A new release may change APIs,
behavior, or data formats; compatibility with old game code is not guaranteed
across breaking releases.

Give every SGL crate the game depends on the same version requirement and commit
the game's `Cargo.lock`:

```toml
[dependencies]
sgl-core = "0.3.0"
sgl-2d = "0.3.0"
```

During `0.x`, `"0.3.0"` accepts compatible `0.3.x` releases but never `0.4`,
which may break APIs; `Cargo.lock` keeps the resolved graph until you update.
For Git dependencies, use the same full commit `rev` for all SGL crates.

1. Identify the game's current version or Git revision and the target release.
2. Read every intervening entry in [CHANGELOG.md](../CHANGELOG.md), including
   `Unreleased` only when targeting an unreleased Git revision.
3. Update the SGL requirements (or Git `rev`) together, apply the listed code and data
   migrations, and consult the target version's package guides and examples.
   Regenerate baked content only when a documented input or format change
   requires it.
4. Build and test the game on its supported targets, then exercise the affected
   gameplay, rendering, input, networking, or persistence workflow. Compilation
   alone does not catch changed defaults or behavior.
5. Refresh [distribution notices](licensing.md) for changed dependencies, ports
   or assets and include them in the game's packaged output.

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
