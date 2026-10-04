# Agent instructions

SGL is Steve's Game Library, designed primarily for AI coding agents building
games. Keep APIs explicit, docs navigable, and examples useful without access
to the original games or a maintainer's machine.

## Read by task

- Building a game: [consumer guide](docs/README.md), then the relevant package
  guide. For 3D, start with [SGL3D for game agents](crates/sgl-3d/docs/README.md).
- Changing SGL: [contribution workflow](CONTRIBUTING.md),
  [architecture](specs/architecture.md), and the owning contract in the
  [spec index](specs/README.md). Load only the relevant detail.
- Validating a change: `bun scripts/tasks.ts check` is the required check;
  setup and target requirements are in [Contributing](CONTRIBUTING.md#setup).

## Repository rules

- Tickets close when the repository's required checks pass. Do not write result
  reports, inventories, or identity tables in `docs/`.
- Never gate a ticket on owner sign-off, acceptance, or review. The owner steers
  by filing new tickets.
- SGL is a game library a game imports. The game owns composition.
  Do not call it a game engine, add an engine on top of it, or turn it into one.
  Game logic is Rust. Authored gameplay data is RON. New 3D clients use
  Rust/wgpu through `sgl-3d` (SGL3D), native and in the browser (WASM +
  WebGPU) alike: both are first-class targets. The game owns its window or
  canvas and event loop. `sgl-2d` renders 2D games and the HUD and UI over a
  3D scene. See `specs/sgl3d.md` and
  `docs/3d-development.md`. Do not add a second gameplay scripting language, an
  ECS, a layer process, or compatibility-lock machinery.
- Baked and generated assets (lightmaps, probe captures, other bakes) are
  content, refreshed only by explicitly running their export commands when their
  inputs or format change. Never add fingerprints, source or lockfile hashing,
  revision locks or other automatic staleness rejection; runtime checks stop at
  format version and structural compatibility. Dependency bumps and unrelated
  code edits must never force regeneration.
- When changing a reusable API or recommended consumer workflow, update its
  owning spec, package documentation, and affected examples in the same change.
- `crates/sgl-3d/docs/` is SGL3D's guide for agents building games: what it
  contains, its features, and every setting a game can offer players. Update
  it in the same change whenever a feature, setting, value, default, preset
  behaviour or the frame workflow is added, changed or removed. Keep it short:
  it steers and points to the code and package README, which hold the detail.
- SGL3D's code follows `specs/sgl3d-architecture.md` and its AR rules: layers
  that depend one way, one stage order, a renderer that only orders stages,
  one owner per layout, encoding and formula, typed boundaries, and nothing
  game-specific. Before adding or changing a stage, shared contract, layer
  boundary or public type, state in the issue or change description where it
  plugs in; if it does not fit, amend that spec first in the same change, and
  obtain and resolve an independent agent review against it. Do not add a
  parallel path, a compatibility shim or a second copy to get a feature in.
- SGL3D rendering follows `specs/sgl3d.md` (RD-1–RD-7 and its roadmap): build
  foundations in roadmap order, follow what other game engines do (port their
  compatible-licensed implementation instead of inventing our own when the
  technique already exists), and delete what a better implementation
  supersedes. Never port from Unity's Graphics repository,
  Unreal, or GPL sources. Keep game policy and content in the consumer.
- Never guess at rendering. Before any rendering change, check the upstream
  reference being ported (vendored beside its port, e.g. DiligentFX; AMD's
  FidelityFX SDK and samples) and the open-source engines' code (Wicked, Bevy,
  Godot, Filament, Diligent and other major renderers), plus papers,
  Frostbite talks, Unreal and HDRP documentation and AMD/NVIDIA docs, with
  sources. Follow the standard pipeline and the pattern those engines share;
  do not invent techniques or tune constants to make tests pass.
- Code stays MIT-compatible: port only from the RD-2 allowed sources, keep their
  licence and provenance, and list them in the notices. Unreal and Unity's
  Graphics repository may inform practice but their code is never copied.
- `crates/sgl-post-fx` is SGL's wgpu effects library, derived from DiligentFX.
  It evolves for SGL3D under RD-2, without an upstream feature-set or 1:1
  conformance obligation. Preserve source attribution, licences and useful
  algorithm notes in `PROVENANCE.md`. It knows nothing of SGL3D; scene and
  frame integration stays in SGL3D.
- FSR2 comes from the published `sp-fidelity` crates, a 1:1 port of AMD's
  SDK kept in its own repository (github.com/stevepryde/sp-fidelity).
  Integration problems are fixed in SGL3D, in the stage that uses the port.
- Rendering evidence is proportionate: the required check, tests that can fail
  at real boundaries, and per-pass GPU timings. Look at captures from the real
  game while iterating, but the owner judges the look; do not declare materials,
  reflections or motion correct, and never base an automated test on image
  interpretation.
