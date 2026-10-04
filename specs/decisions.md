# Decisions

Historical rationale, including superseded designs and retired crate names.
Use the [current specs](README.md) for implementation and the
[consumer guide](../docs/README.md) for integration.

- **D-1** SGL is a private library extracted from working games, not a
  shipped product or an application framework.
  Rationale: the four source games already own composition; they need shared
  internals, not a host. The private-distribution framing is superseded by D-24.

- **D-2** The extraction sources are elemental_chaos, shadow-sp, tiny, and
  torchmates. See [architecture](architecture.md).
  Rationale: they were written from scratch with the same under-the-hood
  patterns; consolidating those patterns is the point of SGL.

- **D-3** The game owns composition and gameplay. SGL owns reusable pieces
  extracted from those games.
  Rationale: keep rules and content in the game so SGL stays callable from
  different worlds, tick rates, and process layouts.

- **D-4** 2D rendering is caller-driven wgpu. Games own winit.
  Rationale: every source game already does this; a library loop would fight
  their existing composition.

- **D-5** Networking stops at bounded opaque payloads with reliable-ordered
  and latest-state delivery. The game supplies wire magic, path, and
  subprotocol.
  Rationale: tiny, torchmates, and elemental_chaos already share this seam;
  game messages and identities above it are not reusable.

- **D-6** Prefer one shared implementation over four copies. Extract from
  working game code rather than inventing a layer for a hypothetical consumer.
  Rationale: the source games are the proof a pattern is useful.

- **D-7** Rust 2024, wgpu, and winit are the 2D rendering foundation. Native net
  I/O is synchronous from the game's point of view. Games may keep using RON
  and postcard for their own data and messages.
  Rationale: that is what the source games already run.

- **D-8** The canvas renderer lights and composites in gamma space by default
  for Godot parity; `LightingSpace::Linear` is the opt-in linear contract —
  world sprites are decoded sRGB→linear on sample into an `Rgba16Float`
  albedo, the canvas modulate, light colors, `energy`, and world instance
  colors are taken as linear, the composite encodes to sRGB once, and the
  screen channel, letterbox blit, and scene capture are unchanged.
  Rationale: elemental_chaos was tuned against Godot's gamma-space 2D
  pipeline, while shadow-sp's light/dark tuning is authored in linear, where
  an ambient brightness of 0.003 means something entirely different.

- **D-9** The specs are the contract SGL is tested against. Requirements in
  `core.md`, `client.md`, `rendering.md`, `sgl3d.md`, `netcode.md`, and `content.md` list
  the behaviours games rely on; a behaviour is removed or changed by editing
  the spec first, not by letting a game's breakage report it.
  Rationale: the source games kept SGL honest by accident; a listed contract
  keeps it honest on purpose and lets tests stand in for the games.

- **D-10** Superseded by D-13. Previously selected Three.js/TypeScript
  presentation with Rust/WASM gameplay for browser and Electron clients.

- **D-11** Superseded by D-13. Previously extracted GLB instancing, animation,
  and resource cleanup into the private `@sgl/three` package.

- **D-12** Native 3D clients use Rust/wgpu through `sgl-3d` (SGL3D), extracted
  from Hyperdrive into SGL by owner direction on 2026-09-13. The game owns its
  executable, window/input, simulation, camera policy, art, UI, and persistence.
  SGL retains the proven renderer and imported FidelityFX component, with their
  numerical/reference validation and explicit fidelity settings. Existing 2D
  APIs keep their paths.
  Rationale: share the implemented native rendering boundary while keeping SGL
  a game-composed library. No editor, ECS, separate engine, or new gameplay
  language is part of this decision. See [SGL3D](sgl3d.md). Its platform
  wording is superseded by D-20 and its 2D path clause by D-21.

- **D-13** Remove the Three.js renderer from SGL by owner direction on
  2026-09-13. Delete `@sgl/three`, the Hearthfield browser/Electron demo and
  its Rust simulation/server, their specifications, dependencies, and tooling.
  SGL3D remains the 3D renderer; `sgl-client` and browser transport remain.
  Rationale: retire the Three.js consumer path completely. Native shader ports,
  lookup data, independent rendering references, and their upstream notices
  remain part of SGL3D's numerical validation; they do not provide an SGL
  Three.js renderer or require Three.js in the workspace dependency graph.

- **D-14** Owner course correction, 2026-09-26: SGL3D optimizes for a
  modern-looking, fast game. Its rendering development rules (RD-1–RD-7) and
  roadmap replace the reference-conformance (RC-1–RC-5) and rendering-validation
  (RV-1–RV-4) contracts, the AMD SSSR fidelity contract and per-feature evidence
  records. Work builds foundations first, ports proven permissively licensed
  implementations, deletes superseded paths, and keeps checks proportionate.
  This supersedes the validation clauses of D-12 and D-13.
  Rationale: weeks of conformance work on narrow mechanisms produced little
  visible quality or speed; the missing foundations (light culling, temporal
  anti-aliasing, motion blur, GI) matter more, and proven open-source
  implementations already exist.

- **D-15** Owner decision, 2026-09-28: SGL3D's screen-space reflections are a
  1:1 port of DiligentFX's SSR (`diligentfx`, `sgl-diligentfx`), composited by
  confidence over each receiver's probe and sky specular. The AMD FidelityFX
  SDK port, its wgpu backend, oracle and adapter are removed, superseding the
  FidelityFX clause of D-12. Its tracing remains, inside DiligentFX.
  Rationale: DiligentFX is AMD SSSR with a confidence output and an
  energy-preserving denoiser, and it follows the pipeline the other open-source
  engines share. AMD's denoiser removes bright narrow reflections by design, and
  its output has no confidence to composite over per-pixel probes.

- **D-16** Owner decisions, 2026-09-28, amending D-15: SGL3D itself runs
  DiligentFX's SSR and TAA from the `diligentfx` port. One post-effect context
  serves both, as in Diligent's Hydrogent renderer.
  - SSR is a `PostProcess` setting. The `sgl-diligentfx` adapter and the
    consumer-installed reflection method are removed.
  - TAA is on by default at High, with the frame rendered jittered as
    Hydrogent does. The Stevecraft TAA is removed. TAA rejects history where
    motion changes between frames, as its constant documents and Godot does,
    not by speed as upstream's code does, so fast racing motion keeps its
    history (DFX-14). Amended 2026-10-05 (#93): the 4-pixel limit left the
    near field without history at racing speed, so TAA follows Godot's TAA
    itself. A steady motion difference keeps Godot's history weight, 0.9375
    less 0.01 per pixel beyond 2.5 (0.69 at 27.5 pixels), and history is
    clipped towards the neighbourhood mean within a box of at most 1 standard
    deviation that narrows to none at 2 % of the screen per frame. Fast
    motion looks antialiased and softer, with slightly more trailing at speed
    (about 0.05 % to 0.3 % of the previous frame at 30 and 60 Hz on
    Hyperdrive's route). Still pixels take the same box, at most 1 deviation
    instead of 2.5, which changes D-17's still-pixel history (DFX-19).
  - Probe captures include area-light emitters, as in Frostbite and Wicked.
    Reflections then count a fixture's emission alongside its light's analytic
    highlight.

  Rationale: TAA and SSR are designed to share one context, and on-by-default
  TAA must live in SGL3D. Including emitters in probes keeps SSR hits and
  misses consistent, the choice the documented engines share.

- **D-17** Owner decision, 2026-09-29, amending D-15: screen-space reflections
  are pluggable. `settings::ReflectionMethod` selects DiligentFX's SSR or
  Godot's (4.7.2-stable, ported in `crates/sgl-3d`), and hardware ray tracing
  will be another method. Each returns premultiplied radiance and confidence
  for the same composition.
  - DiligentFX traces each lobe's peak (`GGXImportanceSampleBias` 1) and its
    temporal pass keeps Wicked Engine's 0.95 of history.
  - TAA keeps a longer history at still pixels and does not reject them by
    depth, as Bevy's TAA does (DFX-19). Since D-16's 2026-10-05 amendment,
    that history is clipped within clamp(1188 / height, 0.75, 1) standard
    deviations of the neighbourhood mean: Bevy's 1σ clip up to 1188 rows,
    tighter above (#94).
  - Static probe captures draw only the visibility groups the frame selects,
    as a reflection probe's culling mask does, so a game can leave near
    fixtures out of probes their proxy cannot place. D-16's emitters stay in
    probes whose proxy holds them.
  - Owner request, 2026-09-30: the methods are named Crystal (DiligentFX's)
    and Velvet (Godot's), not after their sources, since each combines
    several; the package README credits them.

  Rationale: one stochastic ray per pixel left blotches on the glossy,
  normal-mapped road that DiligentFX's denoiser held, and TAA's depth test on
  jittered depth made sub-pixel lights blink while stopped. DiligentFX's
  lobe-peak reflections are sharp; Godot's blur with roughness. Both have
  trade-offs, so the game offers both.

- **D-18** Owner decisions, 2026-09-30: with screen-space reflections proven,
  SGL3D fills in its fundamentals, lighting and shadows first. It gets an
  explicit internal architecture ([SGL3D architecture](sgl3d-architecture.md))
  with its AR rules, and the existing code is restructured to it before
  further features. The restructure may reshape the public API: `Scene` keeps
  content and a `Renderer` owns the frame, with the games updated in paired
  changes.
  Rationale: features were added without a design for how they fit together,
  so two large objects each came to own a part of everything; lighting and
  shadows built on that would entrench it.

- **D-19** Owner decision, 2026-09-30, amending D-15: AMD FSR2 is an
  antialiasing choice beside TAA, with the SDK's quality modes. The FidelityFX
  core, SPD, wgpu backend and dev-only oracle are restored as a 1:1 port for
  it; SSSR and its denoiser stay removed.
  Rationale: FSR2 antialiases like TAA while upscaling, which buys frame time
  on every backend.

- **D-20** Owner decision, 2026-10-04: the browser (WASM + WebGPU) is a
  first-class SGL3D target, equal to native. 3D clients use Rust/wgpu through
  `sgl-3d` on either. SGL3D and the ports it runs (`diligentfx`, `fidelityfx`,
  `fidelityfx-backend-wgpu`) build for `wasm32-unknown-unknown` with wgpu's
  WebGPU backend in the required check, and the browser lane, also part of
  the required check, renders SGL3D on
  WebGPU. Platform differences stop at what the browser forces (no clock, no
  file system, no blocking readback), handled by typed inputs and by the
  effective configuration's reported fallbacks. WebGL2 stays unsupported:
  SGL3D requires compute ([S3D-1](sgl3d.md)). This supersedes D-12's
  native-only wording.
  Rationale: SGL3D supported the browser from the start; the native-only
  framing came in with its extraction from Hyperdrive on 2026-09-13 and spread
  through the specs.

- **D-21** Owner decision, 2026-10-04: rename `sgl-client` to `sgl-2d`. It
  renders 2D games and the HUD and UI drawn over an SGL3D scene. This
  supersedes D-12's clause that existing 2D APIs keep their paths. Games
  move to the new name when they next bump their SGL revision; keeping
  `package = "sgl-2d"` under their `sgl-client` dependency key preserves
  their `sgl_client::` paths.
  Rationale: the other packages are named for what they provide, and
  `sgl-3d` is client code too. "Client" read as the legacy 2D path, while
  3D games such as Hyperdrive draw their HUD with it.

- **D-22** Owner decision, 2026-10-04: the FidelityFX FSR2 port leaves this
  repository. Its core, wgpu backend and oracle live in their own repository
  and are published on crates.io as `sp-fidelity` and `sp-fidelity-wgpu`,
  which SGL3D depends on. This supersedes D-19's in-repository port and the
  `fidelityfx` crate names in D-20; FSR2 as an antialiasing choice is
  unchanged.

- **D-23** Owner decision, 2026-10-04: rename `diligentfx` to `sgl-post-fx`
  and maintain it as SGL's effects library in this repository. DiligentFX is
  the origin of its SSR, TAA and shared context, not a feature-set limit or
  1:1 conformance contract. This supersedes D-15's fidelity requirement for
  that code; D-22's external FidelityFX dependency remains unchanged.
  The library follows RD-2, retaining attribution, licences and useful
  algorithm notes in `PROVENANCE.md`. GPU effects remain independent of
  SGL3D; `sgl-3d/src/view/post_fx.rs` owns scene and frame integration for
  Crystal SSR and TAA. Direct consumers use package `sgl-post-fx` and Rust
  path `sgl_post_fx`; SGL3D's rendering behavior and settings are unchanged.
  Rationale: these effects already combine fixes and enhancements from
  multiple implementations. Their development serves SGL3D, and preserving
  upstream equivalence would constrain that work. Separate publication can
  be considered when another consumer needs it.

- **D-24** Owner direction, 2026-10-04: prepare Steve's Game Library (SGL)
  for open-source use, with AI coding agents as its primary consumers and
  authors. Keep the name and game-owned composition. The root README is for
  people; a concise consumer guide routes agents to package docs, examples,
  and contracts. Contribution guidance must work without private games or
  machine-local paths. This supersedes D-1's private-distribution framing;
  Git/local consumption and disabled crates.io publication remain unchanged.
  Rationale: make the reusable library approachable to others while retaining
  the explicit boundaries that let agents build and maintain games with it.

- **D-25** Owner direction, 2026-10-04: publish SGL's current tracked snapshot
  as `stevepryde/sgl` with fresh Git history, retaining all licences and
  provenance. Reset the six library crates to `0.1.0` and prepare them for
  crates.io; the owner performs registry publication. Examples remain
  unpublished. This supersedes D-24's disabled-publication clause.
  Rationale: give the public library a clean starting point and let games
  consume versioned registry packages as well as Git and local dependencies.

- **D-26** Owner direction, 2026-10-04: SGL evolves for games maintained by
  AI coding agents. Breaking releases are acceptable when they improve the
  library; minimizing consumer edits is not a reason to retain obsolete APIs
  or add compatibility shims. Every consumer-visible update carries actionable
  migration guidance in [CHANGELOG.md](../CHANGELOG.md). Games needing stability
  pin exact crate versions or Git revisions and upgrade deliberately.
  Rationale: agents can migrate game code as the library improves; explicit
  upgrade instructions and opt-in dependency updates make that practical.
