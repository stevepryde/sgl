# Steve's Game Library

SGL is a Rust game library built for AI coding agents and the people making
games with them. It gives agents reusable rendering, input, networking, and
simulation building blocks, with explicit contracts and working examples to
build from.

The game owns its loop, world, rules, and content. Choose the crates you need
and compose them in Rust. There is no required editor, ECS, or gameplay
scripting language. Native and browser (WASM + WebGPU) are first-class 3D
targets.

SGL grew out of AI-authored games that kept rebuilding the same internals.
This is the shared library those games now use. The aim is to make it easier
to build your own game—and, if it inspires you, your own library.

## Start here

**Building a game with an AI agent?** Point it at
[Building games with SGL](docs/README.md). That guide routes it to the right
crate, contracts, and examples without loading the whole repository into
context. For 3D work, continue with [SGL3D for game agents](crates/sgl-3d/docs/README.md).

**Working on SGL itself?** Start with [AGENTS.md](AGENTS.md) and
[Contributing](CONTRIBUTING.md). The [spec index](specs/README.md) maps each
library boundary to its governing contract.

## Features

- **3D rendering:** glTF PBR materials, skinning and morph targets, instancing,
  mesh LOD, decals, clustered lights, cascaded and local shadows, baked diffuse
  lighting, reflection probes, and screen-space and software-ray reflections.
- **Image quality:** TAA, SMAA, native FSR2 upscaling, XeGTAO ambient occlusion,
  volumetric fog, motion blur, bloom, automatic exposure, AgX tone mapping,
  colour grading, and per-pass GPU timings.
- **2D and UI:** sprite batches, logical canvases, texture atlases, Aseprite
  sheets, glyph rendering, lighting, overlays, and immediate-mode widgets for
  game HUDs, menus, and tools.
- **Simulation and input:** fixed-step timing, deterministic hashing and RNG,
  grids, frame animation, swept-AABB collision, and controller events/state.
- **Networking:** reliable-ordered and latest-state delivery over UDP,
  WebSocket, or in-memory transports; simulated networks for repeatable tests.
- **Agent-oriented development:** explicit ownership and lifecycle contracts,
  task-based documentation, runnable examples, and native/WASM/browser checks.

See the [package guides](#crates) for APIs and limits, and the
[SGL3D feature guide](crates/sgl-3d/docs/features.md) for platform differences
and rendering settings.

## Crates

| Crate | Provides |
| --- | --- |
| [sgl-core](crates/sgl-core/README.md) | Fixed-step time, grids, hashing, seeded RNG, animation, collision, and math |
| [sgl-net](crates/sgl-net/README.md) | Opaque-payload UDP, WebSocket, memory, and simulated transports |
| [sgl-input](crates/sgl-input/README.md) | Controller discovery, events, and held state |
| [sgl-2d](crates/sgl-2d/README.md) | Sprite and logical-canvas rendering, assets, text, lighting, and immediate UI; also HUDs over 3D |
| [sgl-3d](crates/sgl-3d/docs/README.md) | PBR scenes, glTF assets, lighting, shadows, reflections, post-processing, and GPU diagnostics |
| [sgl-post-fx](crates/sgl-post-fx/README.md) | GPU post-processing effects used by SGL3D, derived from DiligentFX |

SGL is under active development. The library crates are published on crates.io
and share one version. Keep SGL dependencies on the same release or revision and
consult the matching docs when updating. See [dependency setup](docs/README.md#add-sgl-to-a-game).

## Updates and stability

SGL evolves for games maintained by AI coding agents. New releases may require
changes to game code; we prioritize improving the library over preserving old
APIs. Every consumer-facing update carries [changelog and migration notes](CHANGELOG.md)
that agents can follow.

For a game that needs stability, pin an exact version such as
`sgl-3d = "=0.2.0"` and commit `Cargo.lock` (or pin a full Git commit `rev`).
Upgrade deliberately using the [agent update workflow](docs/README.md#updating-a-game).

## Try it

With [Rust](https://www.rust-lang.org/tools/install) installed, clone this
repository and run the minimal 2D window example:

```sh
git clone https://github.com/stevepryde/sgl.git
cd sgl
cargo run -p sgl-direct-game
```

Rustup uses the repository's [pinned toolchain](rust-toolchain.toml).
The example draws a sprite with a game-owned window and event loop.
For a richer UI example, run `cargo run -p sgl-2d --example tool_ui`.
For 3D, start with the [offscreen example](crates/sgl-3d/examples/offscreen.rs)
and its [frame guide](crates/sgl-3d/docs/README.md#a-frame).

Repository development also needs Bun, Node.js, the matching wasm-bindgen
CLI, and a GPU for browser checks. [Contributing](CONTRIBUTING.md) covers setup
and the required `bun scripts/tasks.ts check` command.

## Inspirations and credits

SGL builds on the work generously shared by
[AMD FidelityFX](https://github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK),
[Wicked Engine](https://github.com/turanszkij/WickedEngine),
[Diligent Graphics](https://github.com/DiligentGraphics/DiligentFX),
[Godot](https://github.com/godotengine/godot),
[Filament](https://github.com/google/filament),
[Bevy](https://github.com/bevyengine/bevy),
[Intel XeGTAO](https://github.com/GameTechDev/XeGTAO),
[SMAA](https://github.com/iryoku/smaa), and
[three.js](https://github.com/mrdoob/three.js).
Their implementations, research, and documentation have shaped SGL's rendering.
Thank you to their authors and contributors.

Source-specific attribution and licence terms are preserved in
[THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt) and alongside the derived code.

## Licence

First-party source is available under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your choice. `sgl-post-fx` retains its
[Apache-2.0 licence](crates/sgl-post-fx/LICENSE.txt); bundled third-party code
and assets retain their own terms. See [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt)
for attribution and licence details. Game agents should follow
[shipping licence notices](docs/licensing.md), which provides a combined
[distribution bundle](DISTRIBUTION_NOTICES.txt) and a generator for the game's
actual dependencies.
