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
- Updating a game: read [CHANGELOG.md](CHANGELOG.md) from the game's current
  version through the target version, then follow the affected package guides.
- Validating a change: `bun scripts/tasks.ts check` is the required check;
  setup and target requirements are in [Contributing](CONTRIBUTING.md#setup).

## Compatibility and migrations

SGL is intended for games maintained by AI coding agents. New releases may
break APIs, behavior, data formats, or integration workflows. Agents are
expected to update game code as SGL evolves. Prefer a coherent, maintainable
library over minimizing migration work; do not retain obsolete APIs or add
compatibility shims solely to avoid updating consumers.

- Every consumer-visible change must update [CHANGELOG.md](CHANGELOG.md) in
  the same change, under `Unreleased`, as one short bullet: the affected crate
  and symbol, the old and new behavior in a clause, and the migration in a
  line or a short code sample. Explicitly say when no game-code changes are
  needed. Rationale, measurements, sources and implementation details belong
  in package docs, specs and the PR, not the changelog.
- Record changes that compile successfully but alter behavior too: defaults,
  settings, units, frame ordering, platform support, and asset/bake formats,
  with any required data conversion or explicit re-export, and what to
  exercise afterwards only when it is not obvious. Never require regeneration
  for unrelated edits or dependency bumps.
- All six library crates share `workspace.package.version` in the root
  `Cargo.toml`; keep `version.workspace = true` in each crate. Update the
  root's internal dependency version requirements alongside release bumps.
- At release, move those entries into a dated version section, merging
  entries that changed the same symbol; retain older migration notes. Use
  Cargo-compatible semantic versioning: during `0.x`, incompatible changes
  advance the minor version, not just the patch version.
- Games depend on every SGL crate with the same version requirement (for
  example, `"0.4.0"`) and commit `Cargo.lock`; Git consumers reference one full
  commit `rev`. Upgrade to a new minor version deliberately, applying all
  intervening migration notes and validating the game's affected workflows and
  targets.

## Licence notices and distribution

- Preserve original licences, copyright notices and provenance for copied code
  and assets. Update `licenses/upstreams.json` with the actual licence-source
  revision when updating a port; run `bun scripts/refresh-licenses.ts` and keep
  source and package notices in the same change. Never replace a retained
  licence with upstream `main` merely because it changed.
- When dependencies or ports change, regenerate `DISTRIBUTION_NOTICES.txt` with
  `bun scripts/distribution-notices.ts DISTRIBUTION_NOTICES.txt --workspace --all-features`.
  Resolve missing upstream attribution; generic MIT/BSD templates are not a
  substitute for original copyright notices. Record changed obligations in
  the changelog. See [licensing workflow](docs/licensing.md).
- When creating or maintaining a consuming game, carry the
  [game-agent rules](docs/licensing.md#instructions-for-game-agents) into its
  own `AGENTS.md` and existing packaging workflow. Generate the game's notices
  from its lockfile, target and features, preserve non-Cargo asset notices, and
  include them in the shipped native/browser files. Inspect the final artifact;
  source-tree presence alone is insufficient. Do not impose extra splash-screen,
  branding or source-publication requirements.

## Repository rules

- Tickets close when the repository's required checks pass. Do not write result
  reports, inventories, or identity tables in `docs/`.
- Never gate a ticket on owner sign-off, acceptance, or review. The owner steers
  by filing new tickets.
- SGL is a game library a game imports. The game owns composition.
  Do not call it a game engine, add an engine on top of it, or turn it into one.
  Game logic is Rust. Authored gameplay data is RON. New 3D clients use
  Rust/wgpu through `sgl-3d` (SGL3D), native and in the browser (WASM +
  WebGPU): both are first-class targets, meaning both are maintained. The
  browser is not a parity target: native supports everything the device can,
  and the browser takes a fallback or goes without a feature where it is not
  feasible (D-27). The game owns its window or
  canvas and event loop. `sgl-2d` renders 2D games and the HUD and UI over a
  3D scene. See `specs/sgl3d.md` and
  `docs/3d-development.md`. Do not add a second gameplay scripting language, an
  ECS, a physics engine (D-33), a layer process, or compatibility-lock
  machinery.
- Baked and generated assets (lightmaps, probe captures, other bakes) are
  content, refreshed only by explicitly running their export commands when their
  inputs or format change. Never add fingerprints, source or lockfile hashing,
  revision locks or other automatic staleness rejection; runtime checks stop at
  format version and structural compatibility. Dependency bumps and unrelated
  code edits must never force regeneration.
- When changing a reusable API or recommended consumer workflow, update its
  owning spec, package documentation, and affected examples in the same change.
- Keep all dependency version requirements in the root `[workspace.dependencies]`;
  member crates inherit them with `workspace = true` and select their own features
  and target conditions. Keep the diagnostics-only self dev-dependency path-only
  so Cargo omits it when packaging. Require wgpu, naga and the wasm-bindgen
  family (`wasm-bindgen*`, `js-sys`, `web-sys`) as caret ranges so a game can
  resolve their compatible fixes; `Cargo.lock` fixes what SGL itself builds
  and tests with. Do not duplicate current dependency versions
  in prose; link to the workspace manifest and lockfile. Retain version numbers where
  they identify a release migration, a dependency example, or upstream provenance.
- `crates/sgl-3d/docs/` is SGL3D's guide for agents building games: what it
  contains, its features, and every setting a game can choose. Update
  it in the same change whenever a feature, setting, value, default, preset
  behaviour or the frame workflow is added, changed or removed. Keep it short:
  it steers and points to the code and package README, which hold the detail.
- Games control what each SGL3D feature does and how much (on or off, and its
  level), never how: algorithms and their internal parameters stay in SGL3D,
  chosen per level (S3D-6 in `specs/sgl3d.md`). A change that adds or ports
  an image-changing feature adds its on/off or level control in the same
  change, with a default so the game need not set it. Correctness fixes and
  the shading model are not settings.
- SGL3D's code follows `specs/sgl3d-architecture.md` and its AR rules: layers
  that depend one way, one stage order, a renderer that only orders stages,
  one owner per layout, encoding and formula, typed boundaries, and nothing
  game-specific. Before adding or changing a stage, shared contract, layer
  boundary or public type, state in the issue or change description where it
  plugs in; if it does not fit, amend that spec first in the same change, and
  obtain and resolve an independent agent review against it. Do not add a
  parallel path, a compatibility shim or a second copy to get a feature in.
- Every GPU loop (`loop`, `while`, `for`) in SGL's WGSL has a named
  compile-time cap that no buffer's contents or length can raise, counting
  every iteration an invocation makes (nested walks share one budget),
  generous above the legitimate worst case with its reason beside it. Data may end a
  loop earlier, never later, and a loop that reaches its cap fails safe (a
  ray reports a miss, a list stops). This covers traversal, ray marching,
  list walks, particles, linked lists, work queues and culling (AR-12).
- SGL3D rendering follows `specs/sgl3d.md` (RD-1–RD-7 and its roadmap): build
  foundations in roadmap order, follow what other game engines do (port their
  compatible-licensed implementation instead of inventing our own when the
  technique already exists), improve on it where it is inefficient or falls
  short, with a recorded decision and before/after measurements, and delete
  what a better implementation supersedes. Never port from Unity's Graphics
  repository,
  Unreal, or GPL sources. Keep game policy and content in the consumer.
- Never guess at rendering. Before any rendering change, check the upstream
  reference being ported (vendored beside its port, e.g. DiligentFX; AMD's
  FidelityFX SDK and samples) and the open-source engines' code (Wicked, Bevy,
  Godot, Filament, Diligent and other major renderers), plus papers,
  Frostbite talks, Unreal and HDRP documentation and AMD/NVIDIA docs, with
  sources. Follow the standard pipeline and the pattern those engines share;
  do not invent a technique an engine already provides, and do not tune
  constants to make tests pass. Going beyond the engines (a cheaper pass, a
  better schedule, a better result) is welcome when measured against the
  ported baseline and recorded as a decision (RD-2).
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
