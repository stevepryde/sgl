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
  unpublished. This supersedes D-24's disabled-publication clause. D-68
  supersedes the owner-publishes clause.
  Rationale: give the public library a clean starting point and let games
  consume versioned registry packages as well as Git and local dependencies.

- **D-26** Owner direction, 2026-10-04: SGL evolves for games maintained by
  AI coding agents. Breaking releases are acceptable when they improve the
  library; minimizing consumer edits is not a reason to retain obsolete APIs
  or add compatibility shims. Every consumer-visible update carries actionable
  migration guidance in [CHANGELOG.md](../CHANGELOG.md). Games depend on SGL with
  ordinary version requirements, commit `Cargo.lock`, and move to a new minor
  version deliberately; Git consumers reference a commit `rev`.
  Rationale: agents can migrate game code as the library improves; explicit
  upgrade instructions and opt-in dependency updates make that practical.

- **D-27** Owner direction, 2026-10-05: the browser stays a first-class SGL3D
  target in that it is maintained: SGL3D builds for it and renders on it in
  the required check, and a game runs on it. It is not a feature-parity
  target. Native supports everything the device can do; where a feature or
  speed-up is not feasible in the browser, the browser takes a fallback or
  goes without it, reported through the effective configuration. A native
  feature or optimisation is never held back for browser parity. This
  supersedes D-20's "equal to native" and its limit of platform differences
  to what the browser forces.
  Rationale: the browser is a target to keep working, not a ceiling on
  native.

- **D-28** Owner direction, 2026-10-05: hardware ray tracing is opt-in.
  `Settings::hardware_ray_tracing` is off by default and no preset turns it
  on; a game that wants it requests the device's feature
  (`graphics_device::ray_tracing_features`, with wgpu's experimental token)
  and turns the setting on. This supersedes the default of on in the design
  of roadmap 13 (#174), and later changes keep it. Ray-traced shadows, which
  need it, follow it: `Settings::ray_traced_shadows` is a `bool`, off by
  default and in every preset, superseding the design's `Preset` that would
  have resolved them on at High once their denoiser landed.
  Rationale: wgpu 29 marks its ray queries experimental, so a game takes
  them on deliberately.

- **D-29** Owner direction, 2026-10-06 (#204): the ray-traced shadow
  denoiser's cheaper form is a setting, not the default:
  `Settings::ray_traced_shadow_quality` Low filters the directional light's
  shadow alone, in two passes, and leaves the local lights to the temporal
  blend; High, the default on the High preset, keeps the four denoised
  slots and three passes; the Low preset takes Low unless the game sets
  High. A saving that leaves the image as it was, within one 8-bit step,
  needs no setting: High filters the directional light's alone whenever no
  local light holds a denoised slot.
  Rationale: Low looked nearly as good in the owner's comparison for about
  1 ms a frame less, so a game chooses it, or takes it with the Low
  preset; the High preset keeps the look.

- **D-30** Owner direction, 2026-10-06 (#226): hardware ray tracing is an
  optional luxury, never a requirement. No SGL feature requires it: every
  feature that uses it has a path without it, which its design states.
  Dynamic GI and world-space reflections (`Moving` and `All`) trace the
  portable BVHs without it. Ray-traced shadows' path without it is the
  shadow maps, which then shadow everything: a portable trace of their rays
  was measured and not taken, the owner keeping it only if it was usable.
  On an Apple M5 natively on Metal (release, 1920×1080, the rays at half
  resolution; medians), over 1000 props the portable shadow rays took
  1.16 ms with the sun alone and 2.27, 4.32 and 6.93 ms with 3, 8 and 15
  local lights, against 0.49, 0.70, 1.02 and 1.47 in hardware: about
  0.38 ms a local light against 0.065. The feature then cost 2.6–9.1 ms a
  frame over the maps, against 1.9–3.7 in hardware. On the consumer's
  route (Hyperdrive's Meridian at High, 1721×1080) the portable rays took
  8.5 ms median and 92 ms at the 95th percentile, against 2.1 and 13.6 in
  hardware, and the frame 23.0 and 112 ms against the maps' 13.9 and 15.6.
  The sun alone is the one case that might be usable without hardware
  ray tracing (2.6–2.9 ms over the maps on the props), unmeasured on the
  consumer's route; it is built only if the owner asks.
  Rationale: almost no games use hardware ray tracing, so no feature may
  depend on it; a software path too slow to use adds nothing over the
  fallback a feature already has.

- **D-31** Owner direction, 2026-10-07 (#241, #246): a low minimum and a
  raised ceiling for what SGL3D binds. Asked whether to raise S3D-1's floor
  of sampled textures per stage from 21 to 31 for new material maps: "Can
  we make it optional? Low minimum, raise ceiling?"; then "core should use
  16 texture slots so that it works on more devices" and "or a basic
  option with 16 is fine too". S3D-1's floor becomes WebGPU's default
  limits, 16 sampled textures per stage among them, and a device takes
  one of two binding tiers by its `max_sampled_textures_per_shader_stage`:
  `Basic` below 48, with a basic look, and `Extended` at 48 or more,
  Dawn's upper tier, with every binding
  ([Binding tiers](sgl3d-architecture.md#designs-that-span-stages)),
  reported by `Renderer::binding_tier`. Mobile is neither targeted nor
  excluded: "I don't currently support mobile, though I don't want to
  explicitly not support it either". Mobile GPUs take a tier by the same
  limit, and no mobile-specific work is done.
  Plan: #241's first change (#251) tiered group 2: on `Basic` the
  anisotropy map, and every map #242–#245 add, gives way to its factor.
  Its second change (#246) tiered lit group 0, `Basic` dropping the
  lightmap's and irradiance atlas's directionality and dynamic GI, each
  reported, and moved S3D-1's floor to WebGPU's defaults.
  Rationale: two tiers, not three. Chrome's Dawn offers 16 or 48, so it
  never sits between them; wgpu 30's Metal offers 96 on macOS and Apple6
  and later and 72 on Apple4 and Apple5, and DX12 at resource binding tier
  2 or above, which wgpu requires, is far above 48. The devices between 21
  and 47 are iOS GPUs older than Apple4 (23 in wgpu 30) and Vulkan drivers
  whose `maxPerStageResources`, which wgpu shares among several limits,
  lands there. A middle tier at 21 would keep today's look on those few at
  the cost of a third layout, provider and boundary test, where AR-3 and
  AR-11 favour fewer variants and the owner accepts a basic look at the
  floor.

- **D-32** Owner decision, 2026-10-07 (#248): SGL3D's lighting model is a
  defined hybrid of references, chosen after comparing its shading with
  three.js r185, Filament ef1a133, Bevy 9d12036, Godot b130438, Wicked
  4323a33 and the Khronos glTF Sample Renderer:
  - material meaning (F0, F90, layering order and extension semantics)
    follows glTF 2.0 and its KHR extensions, with Khronos's sample renderer
    as the reference;
  - core shading maths and energy treatment follow Filament, ported from
    Bevy's WGSL where Bevy follows Filament;
  - an extension lobe that Filament lacks, or defines differently from KHR,
    follows three.js r185;
  - rectangle lights follow Bevy and ltc_code.

  Every indirect irradiance source (the environment's diffuse light, the
  hemisphere fill, lightmaps, irradiance atlas charts, ambient cubes and
  both volumes) lights a surface by one rule. Two additions follow the same
  references: glTF's diffuse coupling, which dims a dielectric's diffuse
  under each light by the Fresnel its specular takes, and sized highlights,
  Karis's representative point for a point or spot light's radius and the
  directional light's disc.

  Three departures from those references, each measured against f64
  integrals:
  - Direct light's multiple scattering takes the environment's own gain,
    Fdez-Agüera's 1 / (1 − F_avg (1 − E)) as three.js's
    computeMultiscattering and Khronos's getIBLGGXFresnel apply it, in place
    of Filament's 1 + F0 (1 / E − 1), so direct and environment light agree
    on every channel. The two are equal for a white metal; on coloured rough
    metals Filament's factor reflected up to 19% more than Kulla and Conty's
    (iron at roughness 1, gold's blue up to 16%), where Fdez-Agüera's stays
    within −6% to +4%.
  - The DFG table is Bevy's 64 × 64 one. three.js's 16 × 16 holds no
    roughness above 0.969 or N·V below 0.031, so a white metal under direct
    light reflected only 0.89 of a white furnace at roughness 1; Bevy's
    keeps it within 0.5% up to roughness 0.95 (N·V from 0.05) and within 3%
    at 1.
  - Sized highlights take Karis's representative point and normalisation
    (α/α′)², with the light's cone widening α taken into half-vector space
    by its Jacobian 1 / (4 l·h) (Walter et al. 2007): α′ = α + r / (2d √(l·h)),
    where Karis's α + r/2d holds at normal incidence only. Against f64
    integrals of GGX over the sphere, from roughness 0.045 to 0.5, sizes
    from the sun's to a fifth of the distance and light elevations from 0.2
    to 1.45 rad, the energy a smooth metal reflects is 0.80–1.02 of the
    sphere's with the Jacobian (and 0.50–0.98 of its radiance along the
    mirror of a smooth surface), 0.84–7.3 with Karis's widening (about
    1 / cos of the elevation: 3.7 at 1.3 rad) and 0.51–6.0 with Bevy's full
    function, whose `specular_fix_remap` and solid-angle factor are not
    taken. The directional light's disc is a sphere at unit distance whose
    radius is the disc's: Bevy shades no sun disc, and Filament's and
    Frostbite's, without the normalisation, reflected 4–16 times the sun's
    energy on the smoothest surface.

  Rationale: the three.js-derived direct-light multiple scattering lost
  energy (a rough white metal reflected 0.90, 0.84 and 0.72 of a white
  furnace at roughness 0.5, 0.75 and 1.0) and disagreed with the
  environment's, and indirect diffuse was weighted by its source; one named
  reference per concern fixes them at their cause (S3D-5) and tells later
  changes which engine to port from.

  Sheen and diffuse transmission (#245, Mission Control's decision on its
  design review):
  - KHR_materials_sheen follows Filament ef1a133 whole, which implements
    it: the Charlie distribution with Ashikhmin's (Neubelt's) visibility,
    its directional albedo E in the DFG table's blue channel from
    Filament's own generator (`DFV_Charlie_Uniform`, ported as
    `scripts/sheen-dfg.ts`), and the base scaled by 1 − max3(sheen) E at
    the view under lights and the environment alike, beneath the coat (KHR
    layers the coat over the sheen). three.js r185's lobe is Filament's;
    its analytic E, `IBLSheenBRDF`, is a fit within 0.146 of the
    Charlie–Kulla albedo and 0.24 of the Charlie–Ashikhmin one for N·V and
    roughness from 0.25, and missed them by up to 2.4 below; paired with
    the Ashikhmin lobe, a white sheen over a white Lambertian reflected up
    to 1.24 of a uniform sky of directional lights (3.4 at N·V 0.02 and
    roughness 0.1), so it is not taken. KHR's min of the view's and each
    light's scaling is not taken either: the view's alone keeps lights and
    the environment one model.
  - Three departures: the scaling is clamped to 0–1, since E exceeds 1
    below N·V 0.1 at roughness below 0.3 (to 10.7 at the table's smoothest
    grazing texel), where Filament leaves it unclamped; the indirect lobe
    is the albedo at the view times the irradiance, as three.js r185 lights
    it, where Filament and Khronos's sample renderer take prefiltered
    radiance, so every indirect source lights it by the one rule, it needs
    no G-buffer channel, and ambient occlusion occludes it linearly with
    the diffuse share; and a rectangle light takes no sheen, as three.js
    r185's takes none, Filament having no rectangle lights.
  - Beneath a sheen the G-buffer's F0 and F90 hold the base lobe dimmed at
    the camera's view, which scales the split sum's single scattering
    exactly, so source completion needs no channel for it; ambient
    occlusion's multi-bounce tint reads the dimmed F0.
  - KHR_materials_diffuse_transmission takes KHR's meaning, its colour and
    the dielectric's Fresnel at the light's mirror image (the Khronos
    sample renderer's), where Bevy 9d12036 colours the lobe with the base
    and leaves it uncoupled, and Bevy's back lobe: a Lambertian about the
    reversed normal, each light's shadow looked up on that side, and that
    side's ambient light. Departures: Bevy's opt-in back-side shadow is
    always taken; the back side's ambient light is occluded by neither the
    material's occlusion nor the frame's, as Bevy leaves it, where the
    Khronos sample renderer applies the occlusion map to it; a lightmap
    holds one side's light, so one without directionality, and every
    lightmap on the Basic binding tier, gives the back side the front's;
    and the ray-traced shadows' mask holds the camera surface's own side,
    so the back side takes the maps. With KHR_materials_volume the back
    lobe lies the volume's thickness behind the surface, as Bevy places it,
    the thickness in world metres the mean of the pose's axis scales, as
    the Khronos sample renderer takes it for diffuse transmission, and is
    attenuated over it by Beer-Lambert's law, as that renderer attenuates
    it; each light's direction and fall-off stay the surface's, where Bevy
    takes them at the back lobe's point.

- **D-33** Superseded by D-94. Owner direction, 2026-10-08: SGL has no
  physics engine; games choose their physics. Most games, and especially
  multiplayer ones, use custom arcade physics, in 2D on
  `sgl_core::collision`; a rigid-body engine (Rapier, as the game's own
  dependency) is for games whose play is simulated physics. SGL builds
  physics only where it brings a significant benefit.
  Rationale: Rapier made the owner's driving game handle worse than arcade
  rules; designed handling, prediction and rollback come easier from rules
  written for the game.

- **D-34** Decision, 2026-10-09 (#268): reliable delivery has four
  independent lanes (`RELIABLE_LANES`), each exact and in order, unordered
  across lanes, sharing a connection by weighted deficit round robin only;
  there is no strict priority. Every fragment is charged one fragment,
  since each takes one datagram or frame, so a backlogged lane sends its
  weight in fragments per round and the gap between two of its fragments is
  at most the other lanes' weights. On UDP each lane has its own sequence
  space, window and retransmission, as ENet channels, GameNetworkingSockets
  lanes and QUIC streams do, so loss on one lane never delays another;
  WebSocket lanes share the TCP stream, keep admission per lane and bound
  the application's interleave to one 16 KiB fragment. A long message's
  first fragment declares its total, checked against the cap before
  anything is buffered; there is no reassembly stall timer, since
  keepalives decide liveness, the ARQ progress and the caps memory. Both
  wire formats move to version 2 without negotiation: UDP carries a lane
  acknowledgement mask in the kind byte's high nibble and only the
  acknowledgements that fit, so `MAX_LATEST_STATE_BYTES` stays 1168 while
  reliable fragments shrink to 1150 to carry all four; WebSocket gains flags
  and lane bytes (an 18-byte header) and 16 KiB frames.
  Owner direction, 2026-10-09: the same change adds a third delivery
  class, `Delivery::Unreliable(Lane)`, superseding D-5's two-class scope.
  Unreliable means never retransmitted or fragmented (at most
  `MAX_UNRELIABLE_BYTES`, 1168) and unordered, delivered at most once (UDP
  receivers drop network duplicates by a per-lane sequence and a
  1,024-message window, as ENet's unsequenced packets do). The sending side
  never discards an accepted message of any class while the connection
  lives: overproduction is refused with `WouldBlock` from a bounded
  per-lane queue, an unsent UDP message waits for a later flush, and
  WebSocket sends every unreliable frame in its lane's schedule under the
  existing pacing. A receiver that is not polled drops its oldest unpolled
  unreliable messages, as a full UDP socket buffer does; reliable overflow
  still closes the peer (Mission Control's decision on review, so a slow
  poller is never disconnected for unreliable traffic on any transport). A
  lane's unreliable messages share its quantum, taking turns with its new
  reliable fragments.
  Rationale: the issue requires progress for every lane, which strict
  priority does not give (GameNetworkingSockets documents that lower
  priorities are starved); 8:1 reproduces Stevecraft's scheduler as a game
  setting, not a default. Keeping the latest-state cap avoids breaking games
  that size snapshots to it. Games send position-style data unreliably
  expecting most of it to arrive, as on UDP; dropping it at the sender
  under backpressure would starve them on WebSocket for as long as a
  backlog lasts.
- **D-35** Owner direction, 2026-10-09 (#272): SGL3D extends to what a game's
  surfaces need through the game's own shaders, not a water category or a
  second renderer. A game adds a WGSL module (`Scene::add_shader`) that
  defines a vertex function, a surface function and a parameter block over
  SGL3D's contract; a material names it, and SGL3D composes it into its own
  geometry and caster programs (`shading::programs`), validated when it is
  added so no pipeline created from it later fails. The vertex function runs
  in every raster pass's vertex shader after skinning and morphing, not in
  the deform stage, which would make every shaded instance a moving one
  with its own vertices and no cache (Godot b130438's `vertex()` in every
  pass variant, Filament ef1a133's `materialVertex()`, Bevy 9d12036's
  material vertex shaders); motion comes from a second evaluation with the
  last submitted frame's time, parameters, instance data and pose, as
  Godot's motion-vector variant does; culling grows by a per-material
  displacement bound, as Godot's `extra_cull_margin`; scene colour reaches
  a surface only through SGL3D's one transmission path, which the surface
  function drives per fragment; the opaque depth reaches the blended draws'
  surface functions on the Extended binding tier alone; rays, bakes and
  static shadow layers see the rest geometry and the plain material, as in
  every reference engine; and a game's loops are counted loops within a
  per-call budget, AR-12's form for code SGL3D does not write. SGL3D ships
  no water, wind or glass: those equations are the game's, and the examples
  carry their own. Rationale: one renderer serves every game's surfaces
  without game categories in the library (S3D-2, AR-6), each feature of the
  standard pipeline (shadows, reflections, transmission, temporal
  antialiasing) sees the same surface, and the composition follows the
  reference engines' established practice (RD-2).
- **D-36** Decision, 2026-10-09 (#269): the reliable message cap is a
  setting, `ReliableConfig::max_message_bytes` (default 64 KiB, ceiling
  16 MiB), not a constant; the receiver's cap governs, so both ends set the
  same one. Transports fragment and reassemble: the sender keeps each
  message whole and sends ranges of it, and the receiver checks the declared
  total before buffering and grows its buffer with what arrives (at most
  twice what has arrived), never past the total. Outbound, a lane holding no bytes admits one message of
  any size up to the cap, as a send larger than `SO_SNDBUF` still proceeds
  and GameNetworkingSockets refuses on bytes already queued rather than on
  the message's own size. Inbound, a lane holds one message larger than its
  `inbound_bytes` beside smaller ones, rather than one only when empty: a
  receiver that returns everything a poll completed (the UDP endpoint) or
  cannot stop reading (threaded ingress, the browser) would otherwise close
  a healthy peer whose large message is followed by a small one before the
  next poll. Native WebSocket stops reading a connection whose lane is full
  until `poll` makes room (Mission Control's decision): WebSocket may be
  slower than UDP, never weaker. The browser API has no read backpressure,
  and a receiver-advertised window would change the wire format, so a
  browser game sizes its inbound bounds instead.
- **D-37** Decision, 2026-10-09 (#270 follow-up): `FixedClock` accumulates
  exact `Duration` time with the step `Duration::from_secs_f64(1.0 / hz)`,
  not `f32` seconds. Rationale: a game timing frames with `Duration` must get
  the same tick counts from SGL's clock as from its own integer clock; `f32`
  rounding ran 0 ticks on a first 1/30 s frame at 30 Hz and dropped 141
  steps of a 5 s stall instead of 142. The cross-target clock fixture in
  `tests/sgl-core/tests/parity.rs` was regenerated with whole-nanosecond
  inputs (its final `alpha` changed; the step count did not).
- **D-38** Decision, 2026-10-09: UDP receivers pace a sender instead of
  closing it. A lane that cannot take its next completed message (its
  inbound bounds, or the endpoint's per-poll global message ceiling) keeps
  the fragment that would complete it at the front of its receive window,
  unconsumed and unacknowledged, and reports it held, so the sender's
  window closes and its `send` returns `WouldBlock`. The threaded server
  polls its endpoint within the room its ingress has left, so its worker
  no longer acknowledges past what the caller can hold. The six-byte
  acknowledgement is kept: the window shrinks from 33 fragments in flight
  to `WINDOW` (32), and the freed bit 31 says HELD (UDP version 3). A held
  fragment is neither resent, counted toward retry exhaustion nor timed
  for RTT, and keepalives decide liveness meanwhile, as TCP keeps a
  connection open while a receiver answers its zero-window probes
  (RFC 9293 §3.8.6.1) and QUIC blocks a sender by flow control without
  loss recovery (RFC 9000 §4). The sender's window starts at the
  cumulative acknowledgement, as TCP's SND.UNA does, since a fragment the
  receiver buffered out of order may become the one it holds. Rationale: a
  transport may be slower, never weaker, and a healthy peer is not closed
  for local saturation (D-36's WebSocket read backpressure); on UDP
  `InboundOverflow` now means only the endpoint's global inbound byte
  ceiling.
- **D-39** Decision, 2026-10-09: a UDP flush packs reliable fragments,
  unreliable messages, latest state and acknowledgements into datagrams of
  up to `MAX_DATAGRAM_BYTES`, as ENet packs commands and QUIC frames into
  one packet; the per-item framing already allowed it, so the wire is
  unchanged. Items keep the lane schedule's order and a datagram closes
  only when the next does not fit, so the scheduler still charges each
  item once whatever its size (amending D-34, whose reason was one datagram
  per item): charging items rather than bytes keeps the gap bound in items
  and so in datagrams, and keeps one unit for UDP items, WebSocket frames
  and memory messages; a lane of many small messages beside bulk takes a
  weight for its rate. Latest state leads each flush's first datagram
  instead of reserving one. With a lane's whole window able to ride one
  datagram, a single lost acknowledgement left the sender waiting for the
  receiver's keepalive and inflated its round-trip estimate until the lane
  stalled under 20 % loss, so a lane acknowledges an arrival in the next
  two flushes. The simulator's `lane_loss_per_10k` removes a lane's items
  rather than the datagram, since a bulk fragment can now share one with
  another lane's message.
- **D-40** Decision, 2026-10-09: WebSocket latest state waits only for the
  lane frames flushed with or before it, keeping that order (the state
  never overtakes events the game flushed first) but no longer waiting
  behind frames flushed later. Behind every released lane frame, it never
  left while a producer kept a bulk lane full, and a newer `send` after a
  flush withheld the flushed state. Each released state keeps its flush's
  generation; one with nothing flushed after it is dropped when the next is
  released, so the queue holds at most one state more than the lanes have
  queued generations. It is not a barrier: while it waits, lane frames
  flushed later may leave first, since holding them would make one lane's
  long message hold every lane back by more than one fragment. Within one
  poll a receiver could not see that order anyway: `pop_inbound` returns
  lane messages before the latest state (#311).
- **D-41** Decision, 2026-10-09 (#283): a game's blended volume measures its
  optical path from three depth layers SGL3D draws of the volumes whose
  shader reads it, each over a copy of the opaque depth: the nearest entry
  face, the nearest exit face and the nearest exit behind it at each pixel;
  `scene_volume_path` returns the length along the view ray and what bounds
  it, on the Extended tier behind `Settings::volume_paths` (on by default;
  it costs only frames that draw such a volume: three depth passes over its
  meshes with three copies of the opaque depth, and three render-size depth
  targets held from then until the setting is off). The reference engines
  take a volume's thickness from its material (KHR_materials_volume's factor
  and map, which glTF has rasterisers bake and ray tracers replace by the
  traced distance; Bevy 9d12036, Filament ef1a133 and three.js r185 read it;
  Godot b130438 has none) or measure water below a plane (Wicked 4323a33
  `oceanSurfacePS.hlsl`), which a finite volume is not; the opaque depth
  behind a surface (#272) bounds the path only from above. The exit layer is
  the back-face depth of Wyman's refraction (*An Approximate Image-Space
  Approach for Interactive Refraction*, SIGGRAPH 2005), whose view-ray term
  the path is, as Oliveira and Brauwers apply it to deforming meshes
  (*Real-Time Refraction Through Deformable Objects*, I3D 2007); SGL3D adds
  the entry layer, so a face seen from inside measures from its entry or the
  eye, and one depth peel of the exits (Everitt, *Interactive
  Order-Independent Transparency*, NVIDIA 2001), so a volume behind
  another's far side, or a concave volume's second part, measures its own
  exit; a third crossing, or an exit behind another exit, is reported
  `VOLUME_HIDDEN`, with an upper bound as its length, rather than given that
  bound unmarked. Nearest-face layers need only the depth test, where
  summing signed depths along the ray would count volumes the nearest hides
  and need 32-bit float blending, optional on WebGPU. The passes keep a side
  by discarding the other in a fragment entry, not by culling: a
  double-sided material's batch holds mirrored and unmirrored instances,
  whose sides only the object record's pose tells (`object_front`).
  Rationale: rasterised volumes get the distance a ray tracer measures at
  their first two crossings, following their deformed geometry, and
  materials that do not read it pay nothing. Limits: the path follows the
  view ray, not the refracted one; a nested volume (ice in water) starts or
  ends the outer volume's path at its own faces; an opaque object inside a
  volume, seen from inside it, has no blended face in front of it and is not
  absorbed; a masked cut-out still bounds; rays and probe captures have no
  layers.
- **D-42** Decision, 2026-10-10 (#312): `derive_stream_seed` absorbs its
  base and each component in turn with one `SplitMix64` step apiece,
  replacing rotate-and-XOR into one cursor, whose overlapping bits cancelled
  (chunk `(65536, 0)` and `(0, 1)` shared a stream). Each step is a bijection
  of its input, so one changed component always changes the seed. Every
  derived seed changed; the frozen `derived_seed_is_frozen` value in
  `crates/sgl-core/src/rng.rs` was regenerated and checked against an
  independent model of the construction.
- **D-43** Decision, 2026-10-10 (#303): a UDP fragment's retransmission
  timeout doubles for each resend since the peer last acknowledged
  anything new, up to the 1 s ceiling the measured timeout already has, and
  anything newly acknowledged on any lane returns every lane's fragments to
  the measured timeout. `max_reliable_transmissions` is removed: a peer is
  closed `TimedOut` after `timeout_ms` of silence, or once a lane with an
  unheld fragment in flight has had nothing newly acknowledged for
  2 × (`timeout_ms` + 1 s) since that fragment's first send while the peer
  is still heard. A flat timeout and a
  resend count made liveness twelve round-trip timeouts (600 ms on a LAN),
  so a caller-polled receiver that stalled briefly with data in flight was
  closed long before `timeout_ms`. The doubling is RFC 6298 §5.5's backoff,
  kept per fragment as ENet keeps it per command. The reset is QUIC's,
  which zeroes its probe-timeout backoff on every acknowledgement (RFC 9002
  §6.2.1, Appendix A.7 `OnAckReceived`), per peer as QUIC does it per
  connection: one datagram carries every lane's acknowledgements, so
  random loss on a live path is resent at the measured timeout and a quiet
  lane is not backed off while another is acknowledged (per-fragment
  doubling alone stalled a lane under 50 % loss). The 1 s ceiling bounds
  how long a stalled peer waits after it answers again. The lane bound is
  ENet's, which closes a peer whose oldest unacknowledged reliable command
  outlives a time limit reset by progress, and is as generous as ENet's
  limits: a stall `timeout_ms` tolerates ends within `timeout_ms` of the
  fragment's first send, so the peer then has more than `timeout_ms` + 2 s
  to take it. That covers the next resend (within the 1 s ceiling), lost
  resends and the round trip, which the RTO ceiling does not bound; a
  tighter `timeout_ms` + 2 s left a recovering peer about 1 s. Only a
  peer that keeps answering but never takes a fragment reaches the bound.
  Held fragments (D-38) are exempt: they wait for the receiver's caller.
- **D-44** Decision, 2026-10-11 (#463): additive glow takes the fog's
  transmittance alone, `a × c × T`, not `a × (c × T + S)`. The additive
  blend adds to a background that already holds the in-scattered light `S`
  at that depth, so adding it again let a black glow brighten the fog and N
  overlapping layers add `N × S`, drawing fog-coloured halos around
  effects. Unity's built-in additive shaders fog toward black
  (`UNITY_APPLY_FOG_COLOR` with zero colour), transmittance only; Godot
  (b130438) applies `rgb × fog.a + fog.rgb` to every blend mode, and SGL3D
  departs from it here. Blended surfaces keep `c × T + S` over their
  coverage, which their over blend replaces.
- **D-45** Decision, 2026-10-11 (#542): a bump map's strength stays per
  pixel in raster and per metre at ray hits. Raster follows three.js r185's
  `BumpMapNode`, whose height gradient is a screen-space difference over one
  pixel's UV step; a ray hit has no pixel, so it takes the height gradient
  per metre on its triangle's UV frame. World-space reflections and
  dynamic GI therefore tilt a bump by the pixels a metre covers relative
  to the camera: stronger wherever a camera pixel covers less than a metre
  (probe captures and screen-space reflections are raster, as the camera).
  Matching them needs a ray footprint at the hit,
  such as Wicked Engine's `RayCone` (Akenine-Möller et al., "Improved
  Shader and Texture Level of Detail Using Ray Cones", JCGT 2021), which
  SGL3D lacks: its hits sample textures at LOD 0. Filament and Bevy have
  no bump maps to follow, and a per-metre raster would depart from
  three.js, the port. A ray footprint, which hit mip selection would take
  too, is the foundation that would let the two agree.
- **D-46** Decision, 2026-10-12 (#654): auto exposure's near-black bin
  positions the percentile window but does not enter the average. The
  pinned [Bevy 9d12036 port](https://github.com/bevyengine/bevy/blob/9d120361303727a66b62f31f0d053793af62417a/crates/bevy_post_process/src/auto_exposure/auto_exposure.wesl#L147-L177)
  includes it at minimum luminance; SGL3D departs from it because a frame
  half black and half luminance 1 should meter the lit half, rather than
  boost it by about four stops. [Wicked Engine cee348b](https://github.com/turanszkij/WickedEngine/blob/cee348b7135d4b6369f85a99ed5e9ed222b71bc7/WickedEngine/shaders/luminancePass2CS.hlsl#L26-L29)
  likewise subtracts the near-black bin from its average's denominator.
  SGL3D keeps Bevy's weighted percentile positions and its minimum-luminance
  fallback when no samples remain, so black still rejects bright outliers
  and an all-black or sub-sample window stays finite. This changes only the
  averaging walk, from 64 bins to 63; metering and its GPU pass stay in the
  exposure stage, with no additional resources or passes.

- **D-47** Owner direction, 2026-11-17: SGL is whatever can be reused between
  games, and no kind of capability is excluded in advance. It began as the
  reusable layer lifted out of custom games and stays that: the game provides
  its shell, its logic and its composition, in Rust and, where it chooses, a
  scripting runtime, and builds from SGL's pieces. An entity system, physics
  (a custom layer, or Rapier or Jolt behind one) or a scripting runtime (Nybl
  is the candidate) may join SGL when a game needs it, as a library a game
  can take or leave. What stays fixed is who holds control: the game owns its
  executable and main loop, SGL is never a host that a game scripts, and a
  headless server still builds without what it does not use
  ([architecture](architecture.md) 4). This replaces the categorical
  exclusions `AGENTS.md` and S3D-1 carried, and the reading of D-12's "no
  editor, ECS, separate engine, or new gameplay language" as a rule: it
  described that decision's scope. D-33 remains the guidance for choosing
  physics today, and its condition, physics in SGL where it brings a
  significant benefit, is the rule. None of these is planned yet; each is
  planned through the architect skill when a game needs it.
  Rationale: a fixed list of excluded features ruled out things SGL may
  come to need, to prevent an outcome, SGL becoming an engine, that the
  ownership rule already prevents.

- **D-48** Owner direction, 2026-11-17: the web is a first-class target in
  its own right, through wgpu's WebGPU backend. Each target takes everything
  its own platform can do: native every feature the device supports, the web
  every feature WebGPU supports. Where the web platform cannot run a native
  feature, the web does not use it and takes a fallback or goes without,
  reported through the effective configuration, and no native feature or
  speed-up is held back for it: D-27 stands in that. The tightening is the
  converse: a feature the web can run is supported there, never left
  native-only because native came first, and the web is not judged against
  native but against what its own platform offers. WebGPU only for now;
  WebGL stays unsupported, and if it is ever added it is a minimal fallback,
  not a second full target. This replaces D-27's summary of the browser as
  "maintained" and "not a feature-parity target".
  Rationale: "maintained, not parity" described the web by what it lacks and
  licensed leaving it behind; first class means complete on its own terms.

- **D-49** Owner direction, 2026-11-17: a piece belongs in SGL when it is
  reusable in principle: nothing in it is one game's, so another game could
  use it as it stands. It does not have to be used by several games, or by
  any yet, and it need not be extracted from more than one. This is the test
  behind AR-6 ("a mechanism only one game could want lives in that game")
  and replaces the reading of architecture 2 and 5 ("when those games share
  a pattern", "one good implementation over four variants"), which describe
  how SGL began, and of D-47's "when a game needs it", as a count of
  consumers.
  Rationale: SGL is the reusable layer of the owner's games; waiting for a
  second game before moving a reusable piece into it defeats the point.

- **D-50** Owner direction, 2026-11-17: SGL is written and maintained
  entirely by AI agents. The owner steers and writes none of it, and the same
  holds for the games that use it and for their assets. Architecture,
  workflow and tooling therefore assume an agent on both sides of every
  boundary and never a human author. Everything needed to change or use the
  library is in the repository: contracts, guides and issues, with nothing
  left to a maintainer's memory. Work is planned, implemented, reviewed and
  verified by agents. Verification is something an agent can run and read,
  not something a person looks at; the owner judges the look and steers,
  which is direction, not a step in the work. Authored content is text, code
  or the output of a command an agent runs, never a step in an interactive
  tool. This replaces "primary consumers and authors" in the architecture
  and the contribution guide.
  Rationale: designing for a human contributor who does not exist adds
  ceremony for nobody and leaves gaps where a person was expected to fill
  them in.

- **D-51** Owner direction, 2026-11-17: SGL is public because it depends on
  and borrows from open-source engines and libraries, and making its own
  source available, with the correct attributions, is the right return.
  Nothing taken from another project is claimed as SGL's own. What is SGL's,
  made through AI coding agents, is the assembly: putting those pieces
  together and adapting them into one coherent whole. SGL is run as an
  open-source project that makes no guarantee of correctness or of
  maintenance. Anyone using it does so for their own purposes, with no
  expectation of support, and is encouraged to fork it or copy features into
  a library of their own. This states the posture behind D-24 and D-25, and
  it means that attribution is part of every port, that nothing is designed,
  kept or promised for the sake of outside users, and that a breaking change
  owes them a changelog entry and the version number semver requires, nothing
  more.
  Rationale: the project stands on others' work and should say so exactly;
  it is one person's library, shared, not a product with obligations.

- **D-52** Owner direction, 2026-11-17: SGL follows semver as Cargo reads
  it, so that updating within a version requirement never breaks a game. A
  release is breaking when a game that built and behaved correctly on the
  previous one must change its code, data or settings to do so again; during
  `0.x` that advances the minor version, and a patch release is every other
  kind, additions and fixes alike. Semver is the whole of the release
  discipline: there is no stable line, stabilisation period or release train,
  and a new version need not be large; it is only a version. This replaces
  the rule of 2026-10-13 that fixes are patch releases even when callers
  must change, under which `0.4.1` changed APIs in a patch; the changelog's
  history stands as written. The next release after `0.6.0` holds to it.
  Rationale: the version number exists to keep the owner's games from
  breaking on `cargo update`; a patch that requires caller changes defeats
  that.

- **D-53** Owner direction, 2026-11-17: changes land on `main` directly when
  the landing agent is confident in them, without a pull request, unless one
  adds value. Review is proportionate: an independent agent's review where
  the change alters structure or a contract or carries real consequence or
  uncertainty (AR-10), the landing agent's own reading of the diff
  otherwise. The issue is the plan, the branch is the unit of review, and
  the landing commit's message is the record. `main` keeps its history rule
  (no force-push, no deletion); the rule requiring pull requests is off.
  Rationale: with agents as the only authors and reviewers (D-50), a pull
  request repeats what the issue and the commit already say and costs tokens
  and steps without adding a check.
- **D-54** Owner direction, 2026-11-17: checks are tiered by cost. A change
  is validated by the checks that can fail because of it (formatting, Clippy
  on the crates it touches, the tests of the modules it touches and the
  scenarios it bears on, each by name), and a ticket closes on those. The
  full check (`bun scripts/tasks.ts check`) runs once at a stable boundary:
  before a release, or over a batch of landed changes; never per change, and
  never two long runs at once on one machine. Tests are written to run
  alone, by name, in seconds; one that cannot be made cheap without losing
  its oracle is named `long_…`, marked `#[ignore]` with the reason, and runs
  in the full check only. A failure the full check finds is filed and fixed
  like any defect. This replaces "the required check" on every change.
  Rationale: with several agents working on one machine, each ending its
  change in the whole suite, often twice, the suite ran many times over for
  one batch and a slot waiting on tests was a slot not working; integration
  failures are found as well by one run as by one per change.
- **D-55** Owner direction, 2026-11-17: long tests are capped. A test runs
  alone in seconds, ten at most; a `long_…` test (D-54) takes at most two
  minutes alone, and all of them together at most ten, which the full check
  enforces by stopping its long lane there and failing. A test that needs
  more is split or shortened; the caps are not raised to fit one.
  Rationale: a lane for tests allowed to be long grows until the full check
  takes an hour unless something bounds it, and a bound that only a person
  notices is no bound (D-50).
- **D-56** Owner direction, 2026-11-17: a surface's baked indirect diffuse
  light adds to the frame's ambient (the environment's diffuse light and the
  hemisphere fill); only the volumes replace it. A lightmap or irradiance
  atlas chart adds to the frame's ambient whole, and a moving instance's
  ambient cube adds to it in the share the volumes leave; the irradiance
  volume and the dynamic GI volume alone take the frame's ambient's place,
  each by its share, the irradiance volume letting its sky visibility of it
  through. A game's bake therefore excludes the sky and the hemisphere fill,
  or the game lowers them where its bake holds them: a closed lightmapped
  room under a bright environment takes that environment on its walls unless
  the game manages it through its hemisphere and environment intensities, or
  lights the room from an irradiance volume, whose sky visibility occludes
  the frame's ambient, in place of charts. This differs from Bevy 9d12036,
  whose lightmapped meshes drop the environment's diffuse light
  (`pbr_functions.wesl` 683–711). SGL keeps it because it is what the
  consumers' bakes assume (baked fixtures, the sky as fill) and what an
  ambient cube needs: an unset cube is black, and a moving instance without
  one would otherwise take no indirect light at all.
  Rationale: the code, the tests and the existing bakes were already
  additive, and the records that read as exclusive were wrong (#779).
- **D-57** Owner direction, 2026-11-18 (more than one view a frame is
  required): a game draws several views of one scene in one frame through
  three objects. `Scene` is content. `Renderer` is what every view of a
  scene shares: the geometry pipeline cache and bind group layouts, the
  deform stage, the dynamic GI stage with its probes, the device's ray-query
  form and the frame boundary. `View` is one camera's frame: its targets and
  sizes, its stages with their state, group 0's bindings, and every history
  kept in screen space or fit to a camera (camera history, DiligentFX's
  context, FSR's context, Velvet, world rays, ambient occlusion, ray-traced
  shadows, the local-light shadow atlas, its static layers, cache and
  ranking, the directional cascades, fog, exposure, the cull pyramid and
  draw lists, statistics). One `Renderer::render(scene, &mut [ViewFrame])`
  is one frame of the scene and exactly one frame attempt: there is no
  begin-without-end state, the scene step sees every view (so ray structures
  are built when any view traces), and borrowing makes one view with two
  cameras in a frame unrepresentable; the cost is that a game cannot
  interleave its own passes between views in the encoder. `views[0]` is the
  primary view: its `FrameInput` gives the rays' visibility mask, the eye
  the acceleration-structure builds are ordered from, and dynamic GI's
  lights, environment, time and frustum; its `Settings` give
  `hardware_ray_tracing`, `dynamic_gi`, `anisotropic_filtering` and the
  diagnostics layers, which a secondary view takes from it in place of its
  own. Dynamic GI stays in the stage order and runs in the primary's render
  with its group 0; other views bind its probes. A view's history continues
  only from the scene's last finished frame: a view resets on its first
  frame, on another scene, and after a finished frame of its scene that did
  not render it, since the scene keeps one frame of motion history (a
  skipped view would mix a two-frame camera delta with one-frame object
  motion; per-view object history is built by none of the cited engines); a
  view drawn at half rate therefore has no temporal accumulation and should
  use SMAA or Off. The scene records what a frame draws when `render` begins
  it and commits that record at `finish_frame`, so an edit after the
  submission belongs to the next frame (F-13). `finish_frame(scene)` names
  no views: each render's attempt carries a flag that `finish_frame` sets,
  held by the renderer and by every view the frame rendered with its
  pending frame. A view settles its pending frame as finished at the start
  of its first `&mut self` method after that frame was finished, whatever
  frames followed without it, and as unfinished only when it renders again
  first; a view method called between `render` and `finish_frame` leaves
  the frame pending. Settling at any `&mut self` method, as designed, would
  have restarted TAA every frame for a game that queries or resizes a view
  before finishing. (A single cell holding the last finished attempt, as
  first built, made a view left out of the next finished frame settle its
  own finished frame as unfinished, losing its statistics, probe readbacks,
  culling pyramid, exposure and local-shadow cache commit.) The frame's
  views are listed in one `render`: two renderers drawing one scene in a
  frame is not supported, since the second `render` supersedes the first's
  attempt and the first renderer's views then reset every frame.
  (`finish_frame(scene, views)` was rejected: a view omitted from the list
  would silently restart every frame.) The local shadow atlas, its static
  layers, cache and ranking are per view, placed by each camera's screen
  coverage, as Filament's shadow maps are per view; the cost is atlas memory
  and face draws per view (a shared fixed-size map per light, Bevy's shape,
  would redesign the atlas). Stage pipelines stay with their stage, so per
  view; only the geometry pipeline cache, deform and dynamic GI are shared,
  since splitting about fourteen stages into programs and state is not
  justified by a one-time creation cost (about 22 ms for a second view).
  Rejected: one `Renderer` per view (dynamic GI, deform and the pipeline
  cache need an owner that changes when a renderer is dropped, and
  call-order misuse states appear, a secondary rendered before its primary);
  renderer-held views behind `ViewId`s (ownership and drop become implicit).
  One renderer shows one scene at a time: another scene specialises its
  pipelines again and restarts dynamic GI, so a game showing two scenes at
  once holds a renderer for each. Known limits, no issue filed yet: L1, rays
  see the scene under the primary view's visibility mask (the mask lives in
  the scene's ray source header), and a view's output cannot yet be a scene
  texture; L2, each view compiles its own stage pipelines and holds its own
  local shadow atlases; L3, FSR restarts after an unfinished frame (D-58).
  Rationale: it follows Filament ef1a133 (`FView` owns `FrameHistory`,
  committed per view in `FView::commitFrameHistory`; the scene is shared),
  Wicked 4323a33 (one `RenderPath3D` per view with its own
  `camera_previous`; scene update, acceleration structures and DDGI gated on
  one path), Bevy 9d12036 (global `PreviousGlobalTransform`, per-camera
  `PreviousViewData`) and Godot b130438 (per-instance `prev_transform` keyed
  on the global frame). Before it, one renderer drawing two cameras
  reprojected each through the other's matrices with no reset, and a second
  renderer on one scene saw zero object motion or committed the frame the
  first drew (O-1, 2026-11-18 history audit).
- **D-58** Owner direction, 2026-11-18 (the frame contract): a frame that is
  rendered and not finished, whether or not its commands were submitted,
  commits nothing, and when it is rendered again as the same frame (the
  same views, with the same stages in effect) restarts nothing, with two
  named exceptions. Each
  stage keeps what describes its GPU history (frame counts, the half of each
  pair it writes, slot assignments, seeds) as the last finished frame left
  it (`view::history::Kept`), so a frame rendered again reads and writes the
  same halves. The ray-traced shadow denoiser's filter history, which Wicked
  keeps in one texture, is a pair like its moments, so it needs no exception
  (B-27). `sgl-post-fx`'s TAA and SSR treat an index executed again as a
  repeat of that frame, its reset included: a repeat of a frame that reset
  resets again, and one of a frame that continued reads the same history
  (DFX-12, DFX-50), where DiligentFX restarts on any index that is not the
  last plus one. FSR restarts: AMD's context advances its frame state
  (`first_execution`, its resource frame index, the previous jitter offset
  and its constants) inside dispatch at record time
  (`sp-fidelity-0.3.2/src/fsr2/component.rs:1510, 1669, 1700, 1933`), that
  state is private to sp-fidelity, and AMD's SDK has no snapshot or restore.
  sp-fidelity gains no snapshot API: AMD's SDK has none to follow, it would
  need a cross-repository release and an inventory of FSR3's in-place
  resources, and it buys continuity only on a frame that is already a hitch.
  Filtered fog does not reproject for one frame (V-5): a submitted frame's
  slice filter overwrote the volume a retry would reproject, and SGL cannot
  tell an abandoned frame from a submitted one; a third froxel volume would
  buy one frame of fog history on a hitch.
  Where the unfinished attempt differs in kind from the frame after it,
  the contract is narrower, by owner direction after the independent
  review of the frame contract (2026-11-18). A reset holds until a frame
  is finished: a view whose render was a reset (a cut, a pending resize
  reset, a frame that does not follow its last finished frame) and was not
  finished renders its next frame as a reset too, whatever its
  `camera_cut` says, since the cut was never shown; this extends the
  view's pending reset, which only a finished frame clears, and spares
  `sgl-post-fx` an index gap (the unfinished reset ran at index 0) and the
  retry a continuation from a half the attempt overwrote. A stage that did
  not run in the unfinished render (antialiasing off, a reflection method
  off, another projection) restarts on the next frame where that render
  released its targets, as a finished frame without it would (F-4), and
  TAA and Crystal restart where that render rebuilt their context for a
  different TAA flag (F-5): the release and the rebuild stay at render,
  since moving them to settle time is machinery for a frame that is
  already a hitch.
  Rationale: the History contract already said an abandoned frame commits
  nothing, and engines that record at render and commit at finish
  (Filament's `FrameHistory`, committed per view) do not restart on a
  repeated frame; restarting every temporal stage on a hitch made the hitch
  visible for a second frame (B-4, 2026-11-18 history audit).
- **D-59** Decision, 2026-11-19 (#826): the sky's share of environment
  specular is occluded by a sky visibility the GI volumes record, never by
  normalising it against the indirect diffuse light a surface receives. The
  dynamic GI volume's probes record the share of their rays that reach the
  sky and give a receiver the a(n) the irradiance volume gives. Not taken:
  environment-map normalisation (Lazarov, SIGGRAPH 2013) and Godot's
  occlusion from ambient luminance (b130438 `scene_forward_clustered.glsl`
  2260–2267). Under D-56 a chart adds to the frame's ambient, so its ratio
  to the sky's irradiance never falls below one; in the dynamic GI volume
  the ratio counts bounce as sky, so shade beside a sunlit street keeps the
  sky's reflection; and a luminance threshold has no meaning in light
  exposed after shading. XeGTAO's bent normals are not taken either: they
  act within ambient occlusion's radius, and cost a wider working term, a
  second history and a second specular occlusion formula.
  Rationale: a visibility depends on geometry alone, is the quantity the
  irradiance volume's path already takes, and costs no tap (the alpha of
  the texel the irradiance tap reads) and no memory (the estimator's spare
  half).
- **D-60** Decision, 2026-11-19 (#832): the procedural sky. The frame's
  environment is one enum, `FrameEnvironment` (`None`, `Map(EnvironmentId)`,
  `Sky(Sky)`), not a map field beside an overriding sky field: a frame names
  one thing that surrounds it, and no combination needs a precedence rule.
  The sun's colour through the air is resolved once per light on the CPU, at
  the camera's height, by `view::directional_light` with the Rust twin of
  the transmittance table's integral, not per lit fragment from the table as
  Bevy (`render/pbr_lighting.wesl` 927-945) and Wicked Engine
  (`GetAtmosphericLightTransmittance`) do: sampling the table per fragment
  would bind it to every lit fragment stage, and Basic's blended stage has
  no sampled texture left; resolved once, every consumer of the frame's
  lights (surfaces, shadows, the fog, probe captures, ray hits, the dynamic
  GI volume) takes one sun colour, and a light wholly below the horizon is
  off and casts no shadow. The medium is analytic and its numbers are
  SGL3D's constants (Hillaire's Earth as Wicked Engine 4323a33 holds it),
  with the game's scales of its three densities, not Bevy's medium textures
  or fields a game sets: Bevy 9d12036's `ScatteringMedium::earth` swaps the
  Mie pair and its falloffs are 13.3 km and 2 km. The transmittance integral
  samples uniform steps at their midpoints: Bevy's right-endpoint sum at 0.3
  of each step overstates Earth's transmittance at 40 samples by 0.9, 1.8
  and 4.0 % (red, green, blue) at the zenith and 4.7, 9.1 and 21.3 % at 10°
  elevation; the midpoint form is within 0.10, 0.13 and 0.19 % and 0.42,
  0.51 and 0.78 % (f64, against a 400,000-step integral). No `Settings`
  level: the tables cost nothing per frame once computed (a light or the
  camera's height changing recomputes the sky view, about 0.11 ms on an
  Apple M5), so a quality level would have no trade-off to offer (S3D-6);
  the environment's variant is the on/off control.
- **D-61** Decision, 2026-11-19 (#829): post owns sharpening for every
  antialiasing mode, as one RCAS in its final pass over the display-referred
  image, and FSR2 and FSR3 are dispatched without their own RCAS. Godot
  (b130438, `render_forward_clustered.cpp` 2566) and Wicked Engine (4323a33,
  `wiRenderPath3D.cpp` 2352) pass the sharpness to FSR2's dispatch, which
  sharpens the linear HDR frame before their post chain, and SGL3D departs
  from them: one pass then sharpens TAA, SMAA, unantialiased, FSR and
  spatially upscaled frames alike, a frame cannot be sharpened twice when
  FSR and spatial upscaling both run (which it was before), and RCAS always
  works on the 0..1 perceptual image AMD's FSR1 documentation asks for.
  Bevy (9d12036, `bevy_anti_alias/src/contrast_adaptive_sharpening`) and
  Wicked's own sharpen filter (`wiRenderPath3D.cpp` 2518) sharpen after
  tone mapping likewise. The strength keeps AMD's scale and sample default
  (0.8); Bevy's default 0.6 multiplies the lobe directly, about 0.63 on
  AMD's scale. FSR1's RCAS, fixed at 0.2 stops (Godot's default) until now,
  follows the setting.
- **D-62** Decision, 2026-11-19 (#833): a procedural sky lights the frame
  through an environment atlas in the format a map supplies (Three r185's
  PMREM atlas, CubeUV, cube size 128: 384x512 RGBA16F, ten levels), which
  the environment resolver binds wherever a map's atlas is bound, so every
  consumer reads a sky through `pmrem_sample` with no branch and no binding
  is added on either tier. Not Bevy 9d12036's second format (a 128-texel
  cube refiltered whole every frame and a 32-texel irradiance cube,
  `atmosphere/environment.rs`, `light_probe/generate.rs`), which would put a
  branch in every consumer (AR-2, AR-3), nor an SH or analytic diffuse term,
  since the atlas's roughest level is what a map's diffuse light already
  reads. Its sharp level is the sky-view table toward each texel (Bevy's
  `environment.wesl`) without the lights' discs, whose reflection is each
  light's own highlight (S3D-5); the rest is Three r185's GGX filter
  (`PMREMGenerator._applyGGXFilter`, `_getGGXShader`). It is refreshed in
  place over a 16-step cycle, one step a frame (the capture, level 1 a face
  a step, level 2 three faces a step, levels 3 to 9 a step each; at most
  4,096 texels and 1.05 million samples a step), started when the sky-view
  table has been computed since the last cycle's capture, and whole on a new
  atlas and in a frame that restarts the view's history; the step and the
  atlas's revision advance when a frame is finished (X-5), and the dynamic
  GI volume takes a changed revision as a changed environment. Three filters
  once offline and Bevy every frame; Godot b130438's
  `Sky.PROCESS_MODE_INCREMENTAL` (a roughness layer a frame) is the practice
  followed. Measured on an Apple M5 (release, 1920x1080,
  `Settings::default()`, a shared machine): a step 0.06 to 0.11 ms median
  (0.27 ms p95 at most), the whole cycle in one frame 1.29 ms median (2.06
  ms p95), nothing at rest; in Chrome 149's WebGPU on the same machine, the
  whole cycle 2.36 ms median (3.34 ms p95) and a step below Chrome's 0.066
  ms timestamp quantum at the median (0.20 ms p95).
- **D-63** Decision, 2026-11-19 (#831): local exposure is built from
  published technique, not a port. No engine RD-2 allows has one: Bevy
  9d12036, Godot b130438, Filament ef1a133, Wicked Engine 4323a33, DiligentFX
  f26cfe5 and O3DE hold no local exposure, local tone mapping or bilateral
  grid (file names at those revisions and code search). It follows Jasmin
  Patry, "Real-Time Samurai Cinema: Lighting, Atmosphere, and Tonemapping in
  Ghost of Tsushima" (SIGGRAPH 2021 Advances in Real-Time Rendering, slides
  130 to 146): a bilateral grid of log luminance filled with linear slice
  weights, `Io = c × (B − M) + d × (Ii − B) + M`, and a base of 40%
  bilateral and 60% wide Gaussian (radius 13 on 64 cells, cubic B-spline
  upsample) against the bilateral filter's ringing on smooth gradients. The
  talk does not give its grid's blur or luminance range, so the grid is
  Chen, Paris and Durand's (SIGGRAPH 2007, sections 3.1 and 5.3: sampling
  rates equal to the kernel widths, a five-tap blur an axis) at Durand and
  Dorsey's (SIGGRAPH 2002) sizes: 64 cells for a spatial sigma near 2% of
  the image, slices 4/3 stop apart for a range sigma of 0.4 log10. SGL3D's
  own choices: the ±8 stop range (auto exposure's), middle grey 0.18 after
  exposure as the pivot, detail strength fixed at 1 (the talk used more
  sparingly; Unreal's documentation advises 1), separate highlight and
  shadow contrasts selected by the base's side of the pivot (the talk and
  Unreal's documentation both author the two separately; neither states
  the selection), a Gaussian sigma of a third of the stated radius, and the
  binomial five-tap kernel. Exposure fusion (Wronski 2022, public-domain
  demo; NVIDIA dxvk-remix, MIT) was not taken: three synthetic exposures
  through Laplacian pyramids cost many more passes, and neither source is
  on RD-2's list. Unreal's source was not read. The build departs from the
  plan's one invocation a cell: a workgroup of 32 invocations shares each
  cell and adds their sums in a tree, which took the pass at 1920x1080 on
  an Apple M5 from 0.69 ms to 0.32-0.38 ms (64 invocations gave 0.44 ms,
  128 gave 7.5 ms).
- **D-64** Decision, 2026-11-19 (#834): a procedural sky's aerial
  perspective is folded into the frame's one fog volume, not applied by
  Bevy 9d12036's full-screen pass (`aerial_view_lut.wesl` read by
  `render_sky.wesl`, dual-source blended, which blended surfaces never see)
  or Wicked Engine 4323a33's per-draw lookup of its camera volume
  (`skyAtmosphere.hlsli`, bindless in every forward shader), since Basic's
  blended fragment stage has no sampled texture left and one sample a draw
  is cheaper than two. The sky stage computes the aerial volume (Bevy's,
  32×32×32 RGBA16F over the camera's frustum, ten steps a slice) with
  Wicked's squared slices to 32 km, linear radiance and the mean
  transmittance in alpha, and march distances, positions and steps alike,
  scaled by `Sky::aerial_perspective`: distance scaling, as Unreal documents
  its Aerial Perspective View Distance Scale, so the air at each sample is as
  far and as high as the scaled distance puts it. That departs from Wicked's
  `aerialPerspectiveScale`, which multiplies only each step's optical depth
  (`skyAtmosphere.hlsli` 650) at unscaled positions and so thickens the air
  at the camera's height. The fog's integration composes the
  fog over it, as Wicked's `aerialPerspectiveCS.hlsl` composes its fog over
  its aerial perspective: each fog slice holds `(I_fog + T_fog × I_air,
  T_fog × T_air)`, 32 slices follow from the fog's length to 32 km of view
  depth, spaced by the square, and a last slice holds the fog alone for the
  sky, whose sky-view table already holds the air. Within the fog's length
  this is Wicked's approximation, the fog treated as lying wholly in front
  of the air, where the two share the path: the fog's in-scatter is not
  dimmed by the air in front of it, and the air's in-scatter there is dimmed
  by the whole fog to that depth, a relative error of at most about
  1 - T_air(length), 0.2 to 0.4 % for a fog 100 to 200 m long at strength
  1. A `Fog::start` (#840) puts the fog wholly behind the near air, where
  the error is at most about 1 - T_air(start), about 1 % at a 500 m start.
  That is the same bound taken over the volume's reach instead of its
  length, since a start that leaves any medium lies within that reach (at
  most about 1.5 times `length`, at the frame's corners); it is reached when
  the fog lies far, and is not a new error.
  `shading/fog.wgsl`'s
  `FogLayout` (in `Frame`, `FroxelVolume` and `SourceCamera`) owns the layout
  and the one mapping from a depth to a slice; without aerial slices the
  volume and every frame are as before. With the fog off the integration
  runs alone on a 32×32 volume of those slices, so aerial perspective is
  independent of `Settings::atmosphere` and `FrameInput::atmosphere`.
  Measured on an Apple M5 (release, 1920×1080, `Settings::default()`, a
  procedural sky, a shared machine): `aerial perspective` 0.24 to 0.29 ms
  median (0.49 ms p95); `fog integration` 0.025 ms median alone (0.18 ms
  p95), and with the fog on 0.07 ms median without aerial perspective and
  0.13 ms with it (0.22 and 0.48 ms p95); `blended` and `reflection source
  completion` unchanged within the machine's noise. In Chrome 149's WebGPU
  on the same machine (the 1280×720 browser baseline under the sky):
  `aerial perspective` 0.066 ms median alone and 0.13 ms with the fog (0.13
  and 0.26 ms p95); `fog integration` below Chrome's 0.066 ms timestamp
  quantum alone, and with the fog 0.066 ms median without aerial
  perspective and 0.20 ms with it (0.33 and 0.52 ms p95); `blended` 0.85
  and 1.11 ms median without the fog and with it, either way.
- **D-65** Decision, 2026-11-19 (#830): screen-space indirect light is
  gathered in one frame and added in the next frame's forward shading by
  reprojection, and its sectors take GTAO's slice measure within a hard
  reach. No depth prepass exists to gather before shading (#393), and the
  G-buffer holds no diffuse colour for completion to weight a bounce by,
  with no colour target to spare. The paper's and SSRT3's weight, a uniform
  sector count times two cosines, is made up for by a gain (SSRT3's
  `GIIntensity` defaults to 10), which S3D-5 rules out, and its search has
  no falloff, so a sample beyond the radius is cut, as XeGTAO fades its own.
  The search spans that radius: it projects the radius whole, as Bevy's and
  XeGTAO's searches do, where SSRT3's `_HalfProjScale` halves it, so its
  march spans half its radius across the view; measured on the plane-and-
  wall fixture at a 1 m radius, the halved march leaves the wall's bounce
  short beyond 0.5 m, its High level's light 0.161 from the hemisphere
  integral on average, past the 0.16 budget, against 0.116 with the radius
  whole. Its horizon
  angles take an exact arc cosine, where SSRT3's `GTAOFastAcos` errs by up
  to 0.008 rad, which leaves a receiver's own tangent plane covering a
  sliver of its horizon off the view axis. It is a pass of its own beside
  XeGTAO, whose radius and resolution differ; XeGTAO is not replaced by the
  bitmask search, whose published form does not weight by cosine.
  Rationale: the cheapest arrangement that leaves the fused pass and ambient
  occlusion's visibility untouched, and an estimator whose energy an
  analytic test can hold.
- **D-66** Decision, 2026-11-19 (#827): a material's occlusion also shadows
  direct light. Every light's front lobes are scaled by Filament ef1a133's
  `computeMicroShadowing` (Chan 2018, "Material Advances in Call of Duty:
  WWII"): saturate(N·L / √(1 − occlusion))², on the mapped normal, the share
  of a cosine-weighted cone of that visibility that a light at N·L reaches.
  glTF 2.0 says its occlusion leaves direct lighting alone, and D-32 takes
  material meaning from glTF; this departs from it, as Filament does for its
  glTF materials, because the cavities a normal map draws cast no shadow of
  their own and neither the shadow maps nor a depth march resolve them.
  Filament applies it to directional lights only; SGL3D applies it to every
  light, a rectangle's at the direction to its centre, and in every view
  (raster, probe captures, ray hits, dynamic GI probe hits), so one material
  is lit alike whatever lights or shows it (S3D-5). It is the shading model,
  not a setting (S3D-6): a material without an occlusion map, or with
  `occlusion_strength` 0, takes none. Uncharted 4's form (Brinck and Maximov
  2016, saturate(|N·L| + 2·occlusion² − 1)) was not taken: it is an artist's
  fit that returns N·L itself at an occlusion of 0.71 and black below N·L 0.5
  at 0.5, where Chan's follows from the cone the occlusion value stands for.
- **D-67** Decision, 2026-11-19 (#825): Bend Studio's screen-space shadows
  sample joins RD-2's allowed sources. Both its files state Apache-2.0
  (© 2023 Sony Interactive Entertainment), the licence Filament and Diligent
  are ported under. Godot b130438's `screen_space_contact_shadows.glsl`, the
  engine port SGL3D follows, derives from it without carrying its notice, so
  SGL3D credits both. Rationale: attribution is exact (D-51), and an
  engine's file can embed another project's code (RD-2).

- **D-68** Owner direction, 2026-11-20: the agent that cuts a release
  performs all of it: the changelog section, the version, the release commit
  on `main`, the `vX.Y.Z` tag and publication of the six crates to crates.io
  with the publishing machine's Cargo login. The steps are in
  [Contributing](../CONTRIBUTING.md#cut-a-release). This supersedes D-25's
  clause that the owner performs registry publication; `0.9.0` is the first
  release recorded under it. Rationale: agents write and maintain all of SGL
  (D-50), and a release that stops at the tag leaves games on the registry
  waiting on a step with no decision in it.

- **D-69** Decision, 2026-11-20 (#844): SGL3D's contact shadows take Bend
  Studio's `IgnoreEdgePixels` (`bend_sss_gpu.h`, Apache-2.0, D-67), always
  on, where Godot b130438, whose port they are, leaves the option out: a
  depth read the 2% edge test takes as an edge casts nothing. It is the
  stage's own rule, not a setting (S3D-6). Without it a plane seen near
  edge-on (a floor beyond about 33 m from 1.6 m up at 1080 rows and a 55°
  field of view) and the frame's outermost pixels read as edges whose
  unfiltered depths shadow the plane. On an empty level floor from 1.6 m
  up at 480×270 at `Medium`, over 35 suns (3°, 10°, 20°, 35° and 60° up,
  every 30° round from behind the camera to ahead of it), 18 shadowed the
  floor, at up to 21,404 of its 64,800 pixels (10,958 with the sun 20° up
  and 60° off ahead), to a mask of 0; and up to 133 pixels lay shadowed
  along the border of a floor seen from above at `Long`. With it, none
  in either, nor on that floor at 1920×1080. The cost is Bend's "can also
  thin out otherwise valid shadows": a caster's silhouette against a
  receiver more than the threshold deeper casts nothing. Measured at
  `Medium` on that floor at 1920×1080 with a 3 cm post 45 cm tall, over
  eight suns round at 25° and at 50° up, the floor pixels the post shadows
  within reach that the mask finds went from 4,599 to 4,577 and 3,080 to 3,014
  of 5,129 and 3,349 at 3 m, 1,299 to 1,296 and 599 to 576 of 1,574 and
  693 at 6 m, and 74 to 72 and 64 to 32 of 340 and 131 at 12 m, where the
  post is under three pixels wide; the floor pixels clear of the post that
  it darkens went from 55,516–65,173 to 201–1,666 at 25° (the band) and
  from 301–728 to 74–699 at 50°. A caster still casts where it meets its
  receiver, the depths there being within the threshold, and the existing
  fixtures' oracles hold unchanged. The pass costs the same: Apple M5,
  Metal, 1920×1080, Bistro's `bistro-quality` recipe, three runs each,
  medians 2.22–2.49 ms before and 2.22–2.44 after on the street view,
  2.17–2.58 and 2.22–2.28 on the bike view. Bistro's captures do not
  separate the two: they differ by 0.63–0.77 grey levels, mean absolute,
  against 0.54–0.89 between two runs of either. Rationale: the band covers
  open ground at any distance contact shadows no longer resolve, under any
  sun to the side of the view, while the shadow lost is the outer part of
  casters a few pixels wide (RD-2).
- **D-70** Decision, 2026-11-23 (#865): a view's picture as a material
  texture. The scene owns a render texture, a view draws its frame into
  it and a material names it as its emission map; nothing is read back.
  (1) Order: the renderer orders a frame's views, producers before the
  views that show them, whichever view is primary, so a screen shows this
  frame with no rule for the game to follow; the primary view's prepare
  and the scene's GPU work run first, which keeps one primary-view policy.
  Ordering by what each view samples in the frame is SGL3D's own: Godot
  b130438 draws child viewports before their parents
  (`_sort_active_viewports`), a hierarchy SGL3D's views do not have; Bevy
  9d12036 takes a game-set `Camera::order`, and three.js r187 and Filament
  leave it to the caller. (2) Feedback reads the last submitted picture
  from the one texture: a producer writes it only in its last pass, so
  views in a loop, in listed order, need no second texture or copy, at one
  frame of latency for the view listed first. A double-buffered texture
  with one frame of latency everywhere was rejected: a reflected camera
  (#866) cannot lag its consumer. (3) A producer renders only in frames
  where another rendered view's frustum and visibility mask hold a surface
  that samples it, tested on the CPU from this frame's bounds, where
  Godot's default `VIEWPORT_UPDATE_WHEN_VISIBLE` uses the frame before's
  draws: the first frame a screen is in view already has its picture.
  Occlusion is not tested. The game's lever is not listing the view. (4)
  Colour: the texture holds the view's displayed picture, sRGB-encoded,
  because a screen is a display; the emission map decodes it as an
  authored image and the consumer's display transform applies once.
  three.js r187 keeps a render target scene-referred and untonemapped
  (`Renderer.currentToneMapping`), which a mirror or portal needs: that is
  a later field of `RenderTexture` with a default, which is why the format
  is the description's (`RenderTexture::format`) and not a constant; its
  producer then stops before exposure and tone mapping, a branch of the
  view's last stages that #866 owns.
  (5) It fills the emission map's binding, on both tiers, and no other
  map: the base colour's alpha reaches casters and the depth of masked
  surfaces, where a picture drawn this frame has no meaning. (6) Rays and
  dynamic GI read the material as authored, since the ray source holds CPU
  texels; probe captures rasterise and sample the last submitted picture.
  (7) No mip chain: the picture is sampled at one level, as three.js's
  render targets are by default, and minifies with aliasing; mips are a
  per-frame cost a later level control would buy. (8) The picture is
  sampled through the material's one sampler, so it wraps as the material
  was authored: a screen clamps (`Material::wrap`), a video wall may
  repeat. three.js clamps a render target's texture by default; SGL3D's
  wrap belongs to the material until a map has a sampler of its own. (9) A
  scene that holds a screen keeps the temporal masks in every frame, as
  one that holds a shader material does (F-15), which costs a TAA or FSR
  view its composition pass and masked resolve while the picture stands
  still. Measured numbers are in the change that landed it. Amended
  2026-11-23 (#877): a frame binds the masks only where something marks
  them in its view (F-15), so a screen whose picture stands still costs
  neither.

- **D-71** Decision, 2026-11-23 (#851): SGL3D readies a frame's shaders and
  pipelines ahead of it with `Renderer::compile`, a planning pass over the
  frame's own code path whose commands are dropped, and compiles the
  programs held in `Compiled` slots in jobs the game runs on its own
  threads; a frame that meets a missing program still creates it in place.
  The geometry pipeline cache's entries are the slots so far. The basis for
  jobs: on an Apple M5, an ordinary first frame run whole on a worker
  thread created its programs over 4.9 s while a second thread's passes on
  the same device saw a longest gap of 4.6 ms, so wgpu's ordinary creation
  on another thread does not stall the one that presents. As built, the
  block world of the `loading` example needs 39 pipelines for its first
  frame, 7 of them jobs; the other 32 are created by the first `compile`
  call on its own thread, the loading loop's longest gap (2.5 to 5.9 s over
  three starts with a cold driver cache, about 50 to 120 ms warm, against
  about 110 to 260 ms for the ordinary first frame warm), after which the
  first frame creates none. Moving the other stages' programs into slots
  (#879) and a `Send` view, which lets a game run the whole pass on a
  loading thread (#881), close that gap. Alternatives: every stage planning
  into one cache of owned descriptors (Bevy's shape), which restates each
  stage's choices apart from its drawing; a hidden rendered frame, which
  submits work and takes part in history. The browser's remedy is WebGPU's
  asynchronous pipeline creation: in Chromium 149 eight heavy pipelines
  created synchronously stopped the page presenting for 11.3 s while the
  calls themselves returned in 0.1 ms (the stall is in the GPU process, so
  a rendering worker does not move it), one a frame for 1.4 s at a time,
  and asynchronously for 10 ms. wgpu exposes neither call
  (gfx-rs/wgpu#3794) nor the handles to make a wgpu pipeline from a raw
  one, so a job in the browser is synchronous creation that the page paces
  between frames, and takes the asynchronous calls when wgpu has them
  (#880). Creating raw twins first, so that Chromium's deduplication
  answers the synchronous call at once (measured: 0.1 ms), was not taken:
  it rests on one browser's unspecified behaviour and a second copy of
  wgpu's descriptor mapping.
- **D-72** Decision, 2026-11-23 (#859): a material's maps in a sampler
  state other than the one its group 2 sampler binds are sampled in the
  shader (`material_sampling.wgsl`: `textureLoad` at the texels and level
  Vulkan's texel filtering and image level selection give), not through a
  sampler binding per map as three.js r187 (`WebGPUTextureUtils.js`),
  Bevy and Filament bind them. Lit group 0 binds five samplers to a fragment
  stage. A sampler for each map would bind three more to every material on
  Basic and fifteen on Extended, 21 samplers a stage there, past WebGPU's
  and Metal's 16, and default materials would bind them too. Extra sampler
  bindings in the shader-sampled pipelines' layout alone would spare
  default materials, but take a second group 2 layout and pipeline layout
  for every pass, and still hold at most eleven states a material where
  glTF allows sixteen, so the shader path would be needed beside them. With
  one bound sampler, the state most of a material's maps take (a trilinear
  one winning a tie), a material whose maps share a state costs what a
  default one does: group 2 keeps 9 entries on Basic and 21 on Extended,
  one bind group, the same pipelines, and one GPU sampler a state for the
  scene where each material made its own (the irradiance volume example's
  five materials: one sampler, from five). Its record grows 16 bytes (224
  to 240; 77 ray words from 75). The irradiance volume example's walk at
  1920×1080 (Apple M5, Metal, shared machine, provisional), opaque
  geometry and lighting, median / p95 ms: 1.365 / 1.999 before, 1.335 /
  1.964 and 1.336 / 1.967 after; in its cave 1.309 / 1.428 before, 1.304
  / 1.356 and 1.303 / 1.356 after. The dynamic GI example's probe rays,
  whose hits now read each map's state: 0.133 / 0.160 ms before, 0.134 /
  0.163 after. A shader-sampled map takes no anisotropic filtering and up
  to eight texel loads a sample, in pipelines keyed by the bindings it
  samples so (`Variant::shader_sampled`). Its cost, the irradiance volume
  walk with each block material's metallic-roughness map on its base map's
  image, clamped where the base repeats, against both repeating, 300
  frames, two runs each: opaque geometry and lighting 2.329 and 2.366 ms
  median against 1.879 and 1.880. A first form that compiled the shader
  path into every map's sample, switched on one pipeline constant, cost
  3.979 against 1.855: the inlined paths took the whole lighting shader's
  registers, so each pipeline now compiles it into its shader-sampled
  bindings alone.
- **D-73** Decision, 2026-11-23 (#867): live specular probes are refreshed
  by the capture operation kept on the GPU, published whole. (1) One cube
  array: live probes take cubes beside the baked ones in the scene's one
  array, and receivers sample them through the bindings they had. A second
  array was rejected: `Basic` binds exactly its 16 textures, and a scene
  without live probes would pay a binding. The cost is one format, so BC6H
  baked probes and live ones are refused together. (2) Publication: a
  refresh renders and filters into staging the renderer holds and copies
  the finished cube, every level and face, into the scene's array in one
  run of copies. Godot b130438 renders and filters a probe in place in its
  atlas, so a probe in flight shows new faces under old roughness levels;
  the staging costs about 2.3 cubes of memory a renderer and one copy a
  refresh, and no consumer samples a part-updated cube. (3) Face size:
  live faces render at the probe's face size, as Godot's and three.js's
  do, not at the bake's 2048-texel supersample (#263): at 128 texels a
  face that is 1/256 of the bake's fill, and about 2.3 MiB of staging against
  the bake's 80 MB of targets. A thin emitter may alias in one refresh,
  and a bake remains the way to store one exactly; a refresh and a bake of
  a scene of texel-aligned emitters hold the same cube
  (`a_refresh_holds_the_cube_a_bake_of_the_same_scene_returns`). (4)
  Culling: each face draws a list culled to its view, as Godot's
  `_render_reflection_probe_step` culls each face, where a bake draws one
  unculled list six times. (5) Shadows and sky: the directional shadow is
  drawn for the probe, in cascades about its centre, as in a bake.
  Local-light shadows are the borrowed view's frame atlas, with a copy of
  its last frame's records that marks each placed shadow held, so moving
  casters and moving lights (S-31) shadow the probe and nothing is placed
  or drawn. Placing the refresh's own layers in the view's atlas, as a
  bake does, was measured: the next frame drew the six static layers of a
  point light again, 19 draws against none
  (`a_frame_after_a_refresh_renders_as_without_it` with that change); an
  atlas of the probes' own (Godot's `reflection_probe_shadow_atlas`) costs
  its memory and draws for every scene with a live probe. A light the
  view's frame gave no shadow so casts none in the probe. A procedural sky
  is the view's atlas as its last frame left it: a sky stage of the
  refreshes' own, for a probe that follows a vehicle, recomputes the sky
  view and runs the atlas's 16 steps (D-62) at the start of every refresh.
  (6) Hidden groups are the probe's (`LiveSpecularProbe::hidden_groups`,
  Godot's `cull_mask`): faces leave them out and cascades keep them, so a
  reflecting object is not in its own probe and still shadows the ground
  the probe shows, and a refresh takes the frame's own input; three.js's
  way, hiding the object for the capture, loses that shadow. (7) The world
  grid stays over baked probes; live probes are walked after it, at most
  16, so a probe that follows an object writes one 224-byte record a move
  instead of rebuilding 256 KiB of cells. (8) The capture's views
  (uniforms, light list, draw instances, shadow records) are held with the
  staging and written by each call: a refresh of a warmed probe creates no
  buffer, texture, layout or pipeline, only each face's two bind groups
  (`refreshes_read_nothing_back_and_upload_no_texels`); a bake fills views
  of its own through the same routine. Measured on an Apple M5, a
  shared machine (`live_probe --measure`: 1280x720, one 128-texel probe,
  directional shadow of two cascades, medians of GPU pass time): a
  frame without a refresh 2.53 ms in 17 timed passes; a whole refresh 0.44
  ms (cascades 0.09, six faces 0.17, filter 0.18), a frame that does one
  3.06 ms; faces then filter 0.44 ms a refresh, its costlier call 0.27 ms,
  a frame at most 2.85 ms; a face a call 0.92 ms a refresh, since each of
  the six face calls draws the cascades again (0.54 ms of it), its
  costliest call 0.18 ms (the filter's), a frame at most 2.72 ms. The
  probe's cube is 1.0 MiB and the staging 2.3 MiB. In
  Chromium's WebGPU on the same machine (the browser lane's fixture: 64
  texels, no directional shadow; the browser quantises a pass's time to
  0.066 ms): a whole refresh 0.20 ms; faces then filter 0.20 ms, its
  costlier call 0.13 ms; a face a call 0.72 ms over seven calls, its
  costliest 0.26 ms. Frames of a scene
  without a live probe recorded the same device commands on the branch as
  at its base (9e91bc6c), by wgpu's API trace: 34 render and 4 compute passes and the
  same textures, buffers, pipelines and bind groups in the first frame, 28
  render and 4 compute passes a steady frame. A refresh records no texture
  write, no copy between a buffer and a texture and no mappable buffer
  (`refreshes_read_nothing_back_and_upload_no_texels`). Rationale: the
  cheapest design that keeps one capture, one prefilter and one sampling
  path, and never shows a torn probe (RD-2, AR-2).

- **D-74** Owner direction, 2026-11-23 (#22): DLSS and MetalFX upscaling
  are not planned. Neither runs in the browser, where WebGPU has no
  equivalent. Natively each is one more temporal upscaler beside FSR2 and
  FSR3, DLSS on NVIDIA devices and MetalFX on Apple's, adding no capability
  a game lacks; whether either looks or runs better than FSR was not
  measured. Roadmap step 12 is closed without being built; a later case for
  one of them is a new issue with that measured comparison. Rationale:
  rendering effort goes to shader and lighting quality first.


- **D-75** Decision, 2026-11-23 (#882): in the browser SGL runs wgpu on its
  own WebGPU backend, `sgl-webgpu`, a port of wgpu's
  (`wgpu/src/backend/webgpu.rs`, v30.0.2) plugged in through wgpu's
  custom-backend interface. Why: the browser side of wgpu is a translation
  layer whose gaps SGL can close itself, and asynchronous pipeline creation
  had waited on wgpu since 2023 (gfx-rs/wgpu#3794). Every type stays
  `wgpu::*`, so nothing that takes a wgpu device changes, and native stays on
  stock wgpu. Cost: the backend implements wgpu's 29 dispatch traits, which
  change between wgpu's major releases, so a wgpu upgrade ports upstream's
  changes to that file in the same change (`crates/sgl-webgpu/PROVENANCE.md`),
  and a browser game creates its instance with `sgl_webgpu::instance`. wgpu
  declares one of those traits' methods only under its own `webgpu` feature,
  so `sgl-webgpu` enables that feature and wgpu's own backend is compiled
  into a browser build beside it, unused, until wgpu stops gating a
  custom-backend method on it. Not taken: waiting for wgpu; a patched or
  renamed wgpu, which cannot publish to crates.io as a dependency and splits
  `wgpu::Device` from the one `sp-fidelity-wgpu` takes; raw twin pipelines
  (D-71). When wgpu exposes a call this crate added, the crate implements
  wgpu's trait method and its own entry point is deleted.

- **D-76** Owner direction, 2026-11-23: a feature is built, and stays, only
  if a game would plausibly use it and it adds something SGL does not
  already give. A plan says when a game would use the feature and what it
  adds over the ways SGL already reaches the same result; where the honest
  answer is "probably never", or "nothing the existing feature lacks", the
  architect declines it in the issue with that reason, and a landed feature
  that turns out so is removed with its settings, tests and records (RD-3,
  AR-11). Reusable in principle (D-49) is still required and is no longer
  enough: it says where a piece belongs, not whether it is worth having. An
  issue on a milestone is listed work, not the answer to the question.
  First applied the same day: IES light profiles (#872) were removed before
  release (a later case for them is a new issue naming the game and scene
  that needs them), as was Draco loading (#856), since an agent can
  re-export a Draco file or use Meshopt, which SGL loads; DLSS and MetalFX
  were not built (D-74), and planar reflections (#866) were kept because
  they show off-screen and occluded content that screen-space reflections
  cannot. Basis Universal transcoding (#857) was declined under the same
  rule: UASTC with Zstandard ships no fewer bytes than the BC7 with
  Zstandard SGL loads, and most of ETC1S's saving survives converting it
  offline to BC7 (`ktx transcode`), with no transcoder in a browser game's
  download. Rationale: every feature is code in every game's build and a
  surface agents must keep coherent; one no game uses costs that for
  nothing.
- **D-77** Decision, 2026-11-23 (#866): planar reflections. A planar
  reflector is a plane the scene holds and an opaque or masked material
  names; each view that shows one owns a reflected view, an ordinary
  `View` the frame renders before that view's remaining stages, whose
  composite is the picture. (1) Not a render texture (D-70): one material
  would name one texture for every viewer, and a mirror's reflection
  depends on the eye, so the picture is the view's, as three.js r187
  `ReflectorNode` keeps a virtual camera and a target per viewing camera;
  the scene holds only the plane. (2) The camera is three.js's: the
  mirrored eye with a proper, unmirrored view whose image is the mirror
  image flipped left to right (view' = S V R, projection' = S P S, S the x
  flip), so winding, culling and every geometry pipeline are the
  viewer's and no second set of pipelines exists; an engine that mirrors
  the view matrix instead flips its front faces. (3) Lengyel's oblique
  near plane, in SGL3D's reversed-Z form: three.js builds it for a
  0-to-1 depth with a finite far plane (`elements[10] = clipPlane.z`),
  where under `perspective`'s infinite far plane the far corner is a point
  at infinity; `row_z = row_w − C (row_w · Q) / (C · Q)` covers both, and
  costs what Lengyel's does, the depth resolution of a standard buffer
  whose near plane is the eye's distance from the mirror. A fragment
  discard or a clip distance was rejected: the first puts a discard in
  every opaque program, the second is an optional WebGPU feature. (4) The
  picture covers the reflector's bounds on screen only, the projection
  cropped to them, where three.js and Wicked Engine 2ff1d9e render the
  whole screen (Wicked at a quarter of it): in the `planar_reflections`
  example at 1280×720 a wall mirror's picture is 160×135 texels and a
  pond's 400×180 at `Half`, of 640×360. Its size moves in eighths of the
  level's full size with a step of hysteresis, since each change of size
  reallocates the reflected view's targets. (5) One level of recursion,
  where three.js's `bounces` lets reflectors render each other: a
  reflected view shows no reflector, and one seen in it draws with its
  fallback (environment and probes), so two facing mirrors cost a viewer
  two reflected views. Wicked skips reflective objects in its reflection
  pass, which leaves holes. (6) The reflected view runs without
  antialiasing, ambient occlusion, screen-space indirect light, contact
  and ray-traced shadows, screen-space and world-space reflections,
  occlusion culling, heat distortion, motion blur, exposure and post, as
  Wicked's reflection pass runs without its screen-space effects. It
  draws its own shadows, at the viewer's quality at `Full` and at `Low`'s
  sizes below it (a quarter of the cascades' memory: a view holds its
  1024-texel cascades from when it is made, 16 MiB, where `High`'s
  2048-texel ones are 64 MiB), where Wicked samples the main view's shadow
  maps, drawn once (`DrawShadowmaps(visibility_main)`):
  a departure, and the costlier one, because cascades are fit to the
  viewer's frustum and a mirror shows what lies behind the viewer, which
  they do not cover. In the `planar_reflections` example (two cascades) a
  reflected view's cascades are about 0.1 ms of the 0.35 ms it costs
  (Apple M-series GPU, 1280×720, provisional). (7) The surface takes the picture as its traced
  lobe's reflected radiance, through the one composition formula, under
  the selected reflection method's cutoff and fade (Crystal's 0.2,
  Velvet's 0.7) whether or not that method runs, so a reflector looks the
  same with screen-space reflections on or off; Wicked adds the picture
  times Fresnel in place of its environment maps at any roughness. The
  picture is not filtered for roughness (no mip chain: D-70 (7)), which
  Unity's and Unreal's planar reflections prefilter: a later level. (8) A
  mapped normal bends the picture by its own reflected ray: the picture's
  depth at the surface point's texel gives how far the mirror direction
  reaches, and the texel sampled is where the surface's ray reaches that
  distance. Wicked adds the tangent normal's xy to the UV, a shift that
  is the same for a reflection at the waterline and one of the far shore;
  this one is exact for the plane's own normal, needs no authored scale
  and shrinks to nothing where the reflected thing touches the surface.
  Its guard places a marker where a normal leaning 0.03 rad sends the ray
  (`a_leaning_normal_bends_the_reflection`). (9) A reflected view is kept,
  unrendered, for 120 of its viewer's frames after the last that showed
  its reflector (two seconds at 60 Hz, one at 120 Hz: a camera sweep off a
  mirror and back), holding its programs and fixed-size tables, and
  released after them, as #853 keeps a view's local shadow atlases once
  made. Making the view again, as dropping it the first frame out of view
  did, took 8 to 15 ms of CPU per reflector in the frame it returned,
  since a `View` builds its stages' programs when it is made (the
  `planar_reflections` example's `--measure`: `Renderer::render` 0.3 to
  0.7 ms with a mirror and a pond in view, 27 to 34 ms in the frame both
  returned, #888). Its picture's targets (72 bytes a texel: 2.5 MiB for a
  200×180 picture, 40 MiB for 800×720) and shadow maps (16 MiB of `Low`
  cascades below `Full`, the viewer's at `Full`, 64 MiB at `High`), and
  the viewer's planar method's targets (10 bytes a viewer pixel), are
  released while it is kept and made again on return: 0.3 to 0.7 ms for
  the targets and 0.1 to 0.2 ms for the cascades when measured alone, and
  the return frame took 0.7 to 1.6 ms against 0.3 to 0.7 steady, where
  keeping them too gave 0.3 to 0.8 against the same. Kept whole, the
  pair of views would have held 30 to 120 MiB while hidden. `Off`, a
  removed reflector and a change of the viewer's render size or level
  release at once. Apple M-series GPU, 1280×720, shared machine,
  provisional. (10)
  The level's default is `Half`, not `Off`: a reflector is content a game
  adds, and S3D-6 asks that a feature's level need not be set. (11)
  Blended materials cannot name a reflector yet: a receiver's traced lobe
  composes in the transparent stage, a later slice. (12) Nothing caps the
  reflected views a viewer renders: every reflector in its frustum and
  visibility mask whose plane its eye is in front of costs one, occlusion
  untested as for an in-world screen (D-70 (3)) and however few pixels it
  covers. A cutoff by coverage would be a threshold of SGL3D's on the
  game's content; which surfaces are reflectors, and in which views, is
  the game's (the visibility mask). (13) The surface is marked for TAA and
  FSR only in the views and frames that show its picture, so a scene's
  reflectors hold no view's temporal masks while out of view or at `Off`.
  (14) Only a perspective viewer shows a reflector: the clip and the
  lookup take view rays from an eye. (15) The picture's fog and air begin at
  the reflector's plane, and the viewer's own reach the surface, so each
  stretch of the reflected path is fogged once, as Unreal's 4.13 preview
  notes say its height fog's ray is clipped at the reflection plane (as a
  user quoted them on Epic's forum; #887); three.js and Wicked fog the path
  from the mirrored eye and the surface again. As first landed, the
  picture was fogged from the mirrored eye: in a uniform absorbing medium
  of 0.08 per metre the reflection of an emitter 10 m along the path
  measured 1.121 of optical depth (the path's is 0.802, the doubled
  stretch's 1.123), and a fog volume behind the mirror dimmed it to a
  twelfth; with the plane it measures 0.812. The sky-view table is not
  clipped, and the air is taken from the mirrored eye's height clamped
  to 1 m above the ground, which for a reflector at ground height seen
  from h metres up evaluates the reflected air about h + 1 m too high.
  Measured numbers are in the change that landed it.
- **D-78** Decision, 2026-11-23 (#884): SGL3D builds a decoded RGBA8 image's
  mip chain on the GPU, a render pass a level drawn in the texture's own
  format, as three.js r187 (`WebGPUTexturePassUtils`) and Filament ef1a133
  (`WebGPURenderPassMipmapGenerator`) do, in place of a CPU chain built when
  the image was added ([Material mip
  chains](sgl3d-architecture.md#designs-that-span-stages)). Why: the CPU
  chain blocked the thread that adds materials for most of a browser game's
  start. On #852's opening scene (16 materials of 512² PNGs; Apple M5,
  Chromium 149 on `sgl-webgpu`, shared machine, provisional, median of 3,
  main at f386a239 against it with this change) `asset_upload` fell from 465
  to 15 ms cold and 451 to 15 warm, of which `Scene::add_materials` from 453
  to 2.9 ms, and the first visible world from 761 to 301 ms cold and 634 to
  192 warm (three.js r187: 244 to 252 and 206 to 214 in the same runs); the
  first frame's encode stayed 14 ms and its GPU completion, the chains now
  included, went from 110 to 113 ms cold and 27 to 26 warm. The minimal
  scene's `asset_upload` fell from 20 to 6 ms and its first visible world
  from 183 to 170 ms cold and 115 to 99 warm. Natively (Metal, Intel
  Sponza's 80 images at 1024², three runs each, interleaved) `add_materials`
  fell from 748 to 828 ms to 27 to 33; the first frame after it encodes the
  chains, 25 to 31 ms of CPU. That frame's whole encode and GPU wait varied
  too much between runs on the shared machine (35 to 154 ms) to compare with
  main's; the chains took 22 to 24 ms of GPU time, their timing group `mip
  chains`. The filter changes from `image`'s Triangle resize, a tent four
  texels wide at each halving (weights 1, 3, 3, 1 over 8), to the 2×2 mean
  three.js and Filament build at run time by default, Bevy's opt-in
  downsampler takes and Godot's importer takes, weighted along an odd axis
  by the area each texel of the level above covers, which none of them does
  (three.js's and Filament's one bilinear tap and Godot's loop leave part of
  an odd level out): levels are sharper, and a high-frequency pattern
  aliases a little more as it recedes. Against the old chains on six of
  Sponza's textures of each kind (the CPU builder's levels; built whole on
  all 80 images, the GPU's chains differ from the CPU's by at most 3 steps,
  on average 0.02 to 0.17 over colour levels 1 to 10 and 0.04 to 0.41 over
  data levels, their rounding compounding down the chain), the mean
  difference over levels 1 to 10 was 0.5 to 1.9 8-bit steps for colour, 0.7
  to 2.2 for normal maps and 0.6 to 1.8 for data, and the largest 36, 60 and
  70 at hard edges in the first levels, where the tent blurred across them.
  The scenarios that draw minified decoded textures (`scenario_history_taa`
  `a_channel_flat_at_rest_leaves_the_others_alone`,
  `scenario_temporal_antialiasing`
  `scrolling_normals_at_rest_keep_their_motion`) pass before and after; no
  oracle moved, as none pins a level's values. Textures of one format and
  size draw a level together, four to a pass into four targets, as many as
  WebGPU's default 32 bytes a sample hold: on Metal, encoding Sponza's 80
  chains took 93 to 183 ms the first time with a pass a texture, nearly all
  of it the driver's first allocations for each render pass, and 25 to 31 ms
  with four a pass; eight a pass, which needs a raised limit, took 20 to 31,
  within the noise, so one program serves every device. Normal maps are not
  renormalised, as before and as three.js and Filament do not; Godot does at
  import. Alpha coverage of masked materials is not preserved, as before: an
  image is shared by materials of different cutoffs and the correction is
  per cutoff (Godot's `preserve_alpha_test_coverage`, off by default), so it
  would be a choice of the image's, not a free addition.
  `Rgba8MipChain::new` keeps the CPU form of the same filter, from tables
  (2.8 to 3.4 times the old builder's speed natively, and the browser
  baseline's prepared chains 157 to 31 ms), for a tool that compresses
  levels or a game that hands the scene its own chains. Not taken: Bevy's
  single-pass compute downsampler, which binds at least six storage textures
  where WebGPU's default is four and cannot write an sRGB format; a faster
  CPU path alone, which still blocks the thread and uploads a third more; a
  bilinear tap, which drops a row of an odd level.

- **D-79** Decision, 2026-11-23 (#869): selection outlines are composited
  after the tone map, at the output size, from a mask of the selected
  instances drawn unjittered at that size, with the line an exact distance
  within its width (a row and a column pass), not three.js r187
  `OutlineNode`'s half-resolution edge detect and separable blur before the
  output transform. The mask's occlusion reads the frame's own opaque
  depth, not a pre-pass of the other objects. Why: the game states the
  width in output pixels and the colour as displayed; a blur radius is
  neither, a colour before the tone map follows exposure and grading, a
  mask before antialiasing either jitters or is smeared by its history,
  and a pre-pass redraws the scene. Cost: a depth-only pass over the
  selected instances, and two passes of at most 2 × 32 + 1 taps a pixel
  over the selection's projected rectangle; jump flooding, the route to
  wider lines, is not needed at the 32-pixel cap. Not taken: an inverted
  hull (Godot's stencil outline), whose width is in metres and which
  misses cut-outs; outline groups of their own colours in one view, which
  need a stencil or identity target, until a game needs them.
- **D-80** Decision, 2026-11-23 (#858): a material's maps each sample one
  of two UV sets through their own `KHR_texture_transform`, resolved in
  the fragment stage by one function (`material_map_uv`; `scene_map_uv`
  for ray hits) in pipelines keyed by two bits, whether a map the pass
  samples takes the second set and whether one takes a transform
  (`Variant::uv1`, `Variant::transformed`), each map's own bit read from
  the record (`map_uv_bits`). Two sets are glTF's floor and Bevy's,
  Filament's and Godot's count. The second set is a stream a mesh keeps
  only where a vertex has one (4 bytes a vertex and a 16-byte rectangle,
  named by a thirteenth mesh-record word), not a ninth packed word, which
  would grow every vertex by an eighth for content that never reads it,
  and not the lightmap UV's word, which must stay the lightmap's. A
  binding's transform rows (32 bytes) live in group 2's existing second
  uniform and after the ray record, not in `Material`, so the record
  raster and ray code pass by value stays 240 bytes and no binding is
  added. A bit a binding in the key, as D-72 keys its in-shader sampling,
  was built first and dropped on review: a quantised file
  (gltfpack, glTF-Transform) transforms every map, so each pattern of
  textured maps would take pipelines of its own, for code (two dot
  products a map) too small to need it; the two bits keep one variant a
  material, as Filament's gltfio keeps one switch (`hasTextureTransforms`)
  with a matrix a map. Resolving per vertex would need a varying a map.
  Baking a transform into vertices at import cannot keep two maps of one
  set apart. The rotation turns as the Khronos sample renderer and
  three.js r187 turn it and the Khronos TextureTransformTest's markers
  require, not as the extension README's GLSL listing, read column-major,
  does. A normal map's frame is the frame of its sampled coordinates: an
  authored tangent is glTF's frame of the normal map's set as stored
  (B = (N × T)·w toward decreasing V), carried through the inverse of the
  transform's linear part, which the Khronos sample renderer and Filament
  leave unturned. Measured (Apple M5, Metal, shared machine,
  provisional), a probe of 16 textured materials over 7,888 cubes at
  1920×1080: on first-set content the same 16 render and 7 compute
  pipelines, 67 bind groups and 6 shader modules before and after, opaque
  geometry and lighting 2.898 and 2.895 ms median before, 2.900 and 2.893
  after; the dynamic GI example's probe rays 0.119 ms before, 0.115 and
  0.114 after; the browser baseline in Chromium (WebGPU, 0.066 ms
  timestamps) 2.294 / 2.359 ms opaque and 6.291 ms a frame, before and
  after alike. With every material's base and normal maps through a
  transform, 2.936 and 2.929 ms (2.913 with a bit a binding); on the
  second set 3.104 and 3.134, about 0.2 ms; both 3.097 and 3.094. The
  fragment's one interpolant more (`Fragment.uv1`), which pipelines that
  do not read it leave zero, is the cost first-set content keeps, accepted
  as measured above; a second program set without it was not taken for a
  difference the measurements do not show. It leaves the fragment 13 of
  WebGPU's 16 inter-stage locations (`Fragment`, vertex.wgsl).
- **D-81** Decision, 2026-11-23 (#886): in the browser SGL3D has the
  browser decode a glTF's PNG and JPEG images (`createImageBitmap` with
  `premultiplyAlpha: "none"` and `colorSpaceConversion: "none"`, as three.js
  r187's `ImageBitmapLoader` does for its `GLTFLoader`) and copies each
  into its texture on the GPU (`copyExternalImageToTexture`, unpremultiplied,
  sRGB), with no CPU texels; its level 0 reaches the ray source by a GPU
  copy from that texture ([S3D-2](sgl3d.md), `asset::load_slice_in_browser`).
  Natively the `image` crate still decodes, unchanged (D-27). Why: on
  #852's opening scene (16 PNGs at 512², Apple M5, Chromium 149, shared
  machine, provisional) the `image` crate's decode was about 45 of the
  53 ms `load_slice` blocked the page's thread for (7–8 ms glTF parse, the
  rest images); with this change `asset_decode` fell from 57 to 20 ms cold
  and warm (three.js 20 to 21), the longest the page went without an
  animation frame during it from 57 to 12 ms (three.js 8), `asset_upload`
  rose from 14 to 24 ms cold and 21 warm, of which `add_materials`' CPU
  time 2.7 to 3.5 ms and the rest the browser's copies in the queue it
  waits on, and the first visible world fell from 281 to 265 ms
  cold and 176 to 150 warm (median of 2; three.js 236 to 240 and 198 to
  201 in the same runs). The minimal scene (one 256² PNG) moved within
  noise: `asset_decode` 8.4 to 7.5 ms, first visible world 172 to 172 ms
  cold, 99 to 106 warm, the warm difference in phases the change does not
  touch. Measured in Chromium 149: the browser's texels equal the `image`
  crate's for PNG with alpha 0 to 255 and with gamma and chromaticity
  chunks, in `rgba8unorm` and `rgba8unorm-srgb`, where default options
  changed up to 3052 of 4096 bytes; a 4:4:4 JPEG differs by at most 1 and
  a 4:2:0 one from libjpeg-turbo by at most 3. Chromium applies a JPEG's
  EXIF orientation whatever `imageOrientation` asks, so the `image` crate
  decodes an image with one, as it does one the browser refuses or decodes
  to another size; every file a native load accepts then loads, with the
  native texels, and a failing load fails with the native errors, though
  where several images fail it may name a later one.
  Not taken: decoding in the browser and reading the texels back, which a
  2D canvas cannot do exactly for alpha (it stores premultiplied) and which
  adds a copy and a write per image; decoding in a worker, which keeps the
  `image` crate's ~45 ms of work, adds a module instantiation and a
  transfer, and has the game own the worker.
- **D-82** Decision, 2026-11-23 (#803): in the browser SGL3D runs FSR2 and
  FSR3 on `sp-fidelity-wgpu`'s reduced resource sets (0.3.4; the port's
  SDK-P38), and `sgl-webgpu` maps the browser's `texture-formats-tier2` to
  `Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` and reports the
  capabilities the two format tiers give each format
  (`crates/sgl-webgpu/PROVENANCE.md`). WebGPU has no texture atomics, so
  the port keeps the reconstructed depth's atomics in a storage buffer and
  runs SPD as two dispatches; with tier2 (Chrome) it keeps the SDK's
  storage formats, without it it takes WebGPU's core formats. Why: FSR is a
  feature WebGPU can run, so it is not left native-only (D-48); every pass
  writes what the SDK's set writes (the port's `storage_tests` and oracle).
  Measured on Apple M5 in Chromium 149 at 1920×1080: tier2's set takes the
  native memory (FSR2 158 MB, FSR3 96 MB, and 8 MB of depth words) where
  the core set takes 258 and 154 MB, and FSR's GPU time on it is the core
  set's or up to 0.13 ms less (FSR2 Native AA 2.56 against 2.62 ms, FSR3
  3.34 against 3.47). Not taken: TAA in FSR's place in a browser (D-48);
  mapping `texture-formats-tier1` to `TEXTURE_FORMAT_16BIT_NORM`, which
  wgpu calls filterable where WebGPU makes `r16snorm` unfilterable;
  subgroups, whose `enable subgroups;` naga 30 does not implement, so the
  browser sets report no wave lanes and run AMD's permutations for hardware
  without 64-wide waves; a `quantizeToF16` or wider float stand-in for the
  narrow formats, which is not exact (SDK-P38).
- **D-83** Decision, 2026-11-24 (#894): SGL3D takes a normal or clearcoat
  normal map's texel in glTF's tangent space on every frame, built by one
  function (`map_uv_texel_frame`) from the coordinates' surface gradients:
  orthonormal, T the unit ∂p/∂U and B = (N × T)·w toward decreasing V
  (glTF 2.0 3.9.3, 3.7.2.1), the frame MikkTSpace builds per triangle and
  the Khronos glTF Sample Renderer's derivative fallback takes, as Filament
  and Bevy shade theirs; the shader owns it, not the importer. Why: the
  derivative and triangle frames took B along the gradient of V, glTF's
  opposite, so every material not using authored tangents was lit with its
  green channel inverted; and they kept three.js r187's proportions
  (`TangentUtils`: the gradients scaled by the longer one's inverse
  length, under a 1e-7 floor of SGL3D's own), which shaded a stretched
  unwrap flatter along its less dense axis, turned a sheared one, and at
  real pixel sizes flattened the map (about 20× at 512 px across 3 m).
  Measured: the Khronos NormalTangentTest and NormalTangentMirrorTest
  within 0.33° of their own geometry (up to 111° before); a stretched and
  sheared plane on glTF's frame (4.1° off before, 15.8° mirrored); against
  the authored tangents the Khronos WaterBottle's and Avocado's maps were
  baked on, the derivative frame at p99 0.54° (1.23° before) and p90 0.27°
  (0.60°). Not taken: three.js's negation of `normalScale.y` in its glTF
  loader, since only the shader knows which frame a binding takes and a
  material built in code passes no loader; three.js's proportions, since
  glTF's frame is unit and square whatever the unwrap. Left to #861: a
  per-triangle frame turns at triangle edges where an authored one
  interpolates, up to 52° off the Avocado's own tangents on its rim.
- **D-84** Decision, 2026-11-24 (#895): the build recipe the guides give a
  browser game, and `measure-browser`'s `web` profile, is fat LTO, one
  codegen unit, `strip = "symbols"` and `opt-level = "s"`, served brotli-
  or gzip-encoded, without `wasm-opt`. Why `"s"`: on `browser_baseline`
  it encoded frames and added assets as fast as opt-level 3 and was 172 KB
  brotli smaller. `"z"` saved another 78 KB but decoded PNGs 15–25% slower
  and added assets about 45% slower (provisional timings on a shared
  machine). `wasm-opt -Oz` shrank raw bytes but not the brotli download.
  In the same change, builds without debug assertions embed SGL's WGSL with
  comments, indentation and trailing whitespace removed at compile time
  (`sgl_post_fx::embed_wgsl!`), every line kept so naga's lines match the
  files. Debug builds embed it whole. Not taken: a build script in each
  crate, with every call site's path changed, and stripping at
  composition, which saves nothing in the binary.
- **D-85** Decision, 2026-11-24 (#890): the dynamic GI volume is
  deterministic: two renderers given the same scene, settings and frames
  hold the same probes, word for word, on one device. Two orders the GPU
  chooses had decided what they held. Each ray's random numbers, which
  draw a probe hit's light and its point on that light, were seeded by the
  ray's place in the frame's list, which an atomic counter fills in
  whatever order the probes' workgroups run, as Wicked Engine seeds them
  (4323a33 `ddgi_raytraceCS.hlsl` 47, `ddgi_rayallocationCS.hlsl` 66);
  they are seeded by the ray's slot, its probe and its index among the
  probe's rays. And the starting probes' room in the boundary bin, the shortened
  turns' spare and the budget were reserved first come, first served; the
  budget is now shared out as rooms that together never pass it, the
  turns' among them, and each room by a claim: its probes, in index order
  from one a hash of the frame chooses, are granted while their rays and
  those before them fit, an exclusive prefix sum over blocks of 256 probes
  (`claim_sums`, `claim`). Why: nothing could compare dynamic GI's output
  across a change (#879's equivalence tests left it out), nor could a game
  reproduce a frame. Measured on Apple M5 (Metal), two renderers at Low:
  before, about 2,000 probe hits' light differed on the first frame under a
  sun and a point light, and the rays of 10 to 28 of 1,024 starting
  probes; now nothing differs, in 20 runs each on the portable and the
  hardware path, with the same state in every run. A converged picture of
  the `dynamic_gi` example differs from the old one's by what two old runs
  differ by (mean 1.29–2.17 and 0.76–1.04 a channel at Low and High,
  against 1.37–1.72 and 0.84–1.19). The two dispatches cost a frame about
  0.015 ms at any size, their latency rather than their work: 0.018 ms
  timed alone; the allocation's group 0.0095–0.025 ms before and
  0.026–0.042 ms after for 320 probes at High over five runs of each,
  alternated (provisional, a shared machine). Not taken, each measured no cheaper: the claim folded into a
  threshold and an allocation each over blocks, which writes the ray list
  from a workgroup a block and cost a 320-probe volume 0.043 ms; a
  threshold over blocks that keeps each probe's claims for a per-probe
  allocation; and the claim's scan with 9 barriers in place of 25.
  Skipping the claim when no room is short, through indirect dispatches of
  no workgroups, would add the pass wgpu runs to validate each indirect
  dispatch. Nor kept: a refused request's rays for a later, smaller one,
  which only a probe-by-probe walk keeps deterministically; the prefix
  leaves a room less than one claim's rays.
- **D-86** Decision, 2026-11-24 (#879): every shader module and pipeline a
  frame's stages create is a compile job (AR-13), which completes D-71:
  after `Renderer::compile` and its jobs, neither a `compile` call nor the
  frames it readied create one on their thread. A stage holds its programs
  in the `Compiled` slot built for the geometry cache, as a set or as its
  whole lazily made bundle, and leaves a planning pass's commands out
  while they are missing; a stage whose result selects another's programs
  keeps that result outside the slot, and a program first used the frame
  after is asked for in the planning pass of the frame before. Not taken:
  splitting every stage's programs from its targets and bind groups,
  which restructures each stage for no gain over the bundle; and a list of
  variants to precompile, which restates the stages' choices apart from
  their drawing (D-71). `sgl-post-fx` gets a `PipelineCompiler` of its
  own, restoring DiligentFX's pending state (PROVENANCE DFX-2), so it
  still knows nothing of SGL3D. `sp-fidelity`'s pipelines stay the port's:
  an FSR view creates its context's (10 for FSR2, 12 for FSR3 at 0.3.3) in
  the `View::new` or `View::resize` that builds the context, and a few
  helpers at that context's first dispatch, in the frame after. A
  planning pass no longer runs FSR: one that did restarted the view's
  accumulation and moved its jitter sequence, so a compiled FSR view's
  frames differed from an ordinary one's. Stages that landed while this
  was built (depth of field, planar reflections, a game's HDR effect, sprites,
  material mip chains, outlines, instance footprints)
  joined the same way. Probe captures, a live probe's
  refresh among them, still create their own programs: `compile` readies
  frames (#889; for live probes, superseded by D-90). A job's body is where #880 starts WebGPU's asynchronous
  creation through `sgl-webgpu` (D-75); `Compilation` and `CompileJob`
  keep their shape.
  As built (Apple M5, Metal; Chromium 149 through `sgl-webgpu`; shared
  machine, provisional), the `loading` example's block world needs 46
  pipelines and 9 shader modules for its first frame, 34 jobs. Warm, from
  the loop's start to the first level frame complete: 76 to 79 ms with
  four or eight job threads (45 to 48 ms compiling) and 117 to 129 ms with
  one, against 112 to 129 ms ordinary, whose first `render` takes 91 to
  105 ms on the loop's thread; the longest gap between loading-screen
  frames is 29 to 32 ms compiled (the planning pass's own frame setup)
  against 118 to 134 ms ordinary. On starts where the driver compiled
  (1.4 to 5.2 s of compiling) the longest gap stayed 29 to 41 ms, where
  D-71's first step stalled 2.5 to 5.9 s. In Chromium, warm, jobs paced on
  the queue's work-done callback: one job a batch, startup 442 to 450 ms
  and a longest gap between animation frames of 23 to 28 ms; four, 275 ms
  and 34 to 37 ms; ordinary, 217 to 225 ms and 156 to 160 ms. Jobs run
  every animation frame without that wait piled up in the browser's GPU
  process: one cold start stalled 5.0 s. The startup comparison's
  prepared path (#852's harness) readies SGL in 109 ms cold and 26 ms
  warm, its longest startup frame gap 15 to 18 ms, where three.js r187's
  `compileAsync` took 330 and 53 ms with a gap of 75 to 84 ms.
- **D-87** Decision, 2026-11-24 (#860): a material's occlusion map in an
  image, sampler, UV set or transform of its own and `KHR_materials_specular`'s
  specular and specular colour maps shade on the Extended binding tier, in
  group 2 bindings of their own (22–24, map slots 16–18); on Basic they give
  way to their factors, and an occlusion map that is the metallic-roughness
  map's slot (ORM) is read from that map's sample on both tiers. glTF 2.0
  (3.9.3) and the extension define them: occlusion 1 + strength · (red − 1);
  a dielectric's F0 min(the IOR's F0 · colour factor · the colour map's
  sRGB-decoded RGB, 1) · strength factor · the specular map's alpha, its F90
  that strength, and a metal untouched; three.js r187 and Filament ef1a133
  read the same channels in the same colour spaces. A material without
  them costs what it cost before, by construction: raster samples them in
  programs that compose their sampling provider, which only a material that
  has one draws with (`Variant::occlusion_specular`, one variant whichever
  it has); every other material's programs compose a fallback that
  references none of their bindings; ray hits sample them behind a lit
  constant only a scene that holds one sets (`LitConstants::occlusion_specular`,
  which the geometry pipelines' keys leave out); and the record keeps its
  layout, the three bindings' sets, transforms and sampler states in one
  word outside it that only their samples read (group 2's `more_maps`, the
  ray record's word after `baked`), a hit on that set taking it from its
  triangle. Compared program by program from wgpu-hal's generated MSL for
  scenes without the maps, against the base: every program but the opaque
  fragment program is the base's but for unused constants and the order of
  two functions, and that one, and the dynamic GI trace, execute the base's
  code but for multiplications by a constant 1 and a mask's value; group 2
  binds three more white textures, which those programs do not reference,
  and its transform rows grow from 528 to 624 bytes, unread. Measured
  (Apple M5, Metal, 1920×1080, alternating before/after pairs under the
  shared lock on a busy machine; a lit scene whose 16 materials sample base,
  metallic-roughness and normal maps; after − before, median and range):
  without the maps, the opaque geometry and lighting −0.10 ms (−0.42 to
  −0.02, six pairs; 3.56 ms against 3.65) and the dynamic GI probe rays
  +0.004 ms (−0.006 to +0.012, six pairs; 0.144 ms against 0.142), each
  inside the pairs' own spread. Packed occlusion draws with those programs.
  With the three maps on every material, as 256² noise images, the opaque
  geometry and lighting take +0.80 ms (+0.66 to +1.26, five pairs; 4.58 ms
  against 3.58), and on one-texel images, which stay in cache, +0.07 ms
  (−0.08 to +0.34): the rest is their texels' memory traffic. Not taken,
  each built and measured first: the sampling gated by the record's bits in
  every program, then by a pipeline
  constant over one program, which left the bindings and dead code in every
  program and cost a material without the maps about 0.03 ms of 3.0 in the
  first measurements; the new slots' bits and sampler states in the
  record's padding, which made every ray hit decode two words more and pass
  the whole record to each map sample, about 0.016 ms of 0.13 on the
  dynamic GI probe rays; a second group 2 layout for materials with them,
  which would double group 2's pipeline layouts for three textures other
  programs never read; and a packed fast path for the two specular maps,
  which neither the extension nor its exporters define. Game shaders are
  validated against the fallback programs only: the sampling provider
  differs in function bodies that name SGL3D's own, reserved declarations.
- **D-88** Decision, 2026-11-24 (#861): a glTF primitive whose material
  takes a vertex tangent frame (a normal map, anisotropy, or a clearcoat
  normal map on the normal map's set) and that carries no `TANGENT` gets
  MikkTSpace's frames at import (`LoadOptions::tangents`,
  `TangentFrames::Generate` by default; `AuthoredOnly` skips it), through
  Bevy's `bevy_mikktspace` 1.0 with its `corrected-edge-sorting` and
  `corrected-vertex-welding` features, once a mesh however many nodes use
  it; and a material takes its meshes' vertex frames for its normal maps
  while every mesh drawing it has one (`MATERIAL_BINDING_VERTEX_FRAMES`,
  which the scene sets before a frame or a probe capture draws it, so a
  record it has not reached takes the derivative and triangle frames),
  those frames otherwise. Why: glTF 2.0 3.7.2.1 defines absent tangents as
  MikkTSpace's, the frame a baked normal map expects. The per-triangle
  frame turns at triangle edges where the bake's turns smoothly: on the
  Khronos Avocado, against its own tangents, it is 22° off at ray hits
  around its cut face (52° on a few pixels of a side view) where generated
  frames are within 2.2°. An anisotropic file without tangents loaded only
  with them. Measured (Apple M5, shared machine, provisional): about 0.75
  µs a triangle, native and wasm32 in V8 alike, but the square of the
  triangles sharing one vertex (fans of 1 000, 4 000 and 8 000 about 6, 84
  and 331 ms: MikkTSpace's grouping compares a vertex's triangles
  pairwise, which `bevy_mikktspace` keeps although no angular threshold
  splits groups here), so a primitive in which more than 1 024 triangles
  share one vertex keeps the per-triangle frame and is reported
  (`Ignored::TangentFrames`, #902): 331 ms / 8 000² ≈ 5.2 ns a pair, so
  the worst case under the cap costs about 5.2 ns × 1 024² ≈ 5.5 ms, while
  a 256-segment disc's centre or cone's tip still generates; an upstream
  change in `bevy_mikktspace` skipping that comparison when no threshold
  is set would make the cap unnecessary. NormalTangentTest 7 ms (about
  35% of its 20 ms native load), DamagedHelmet 12 ms (about 3%),
  AntiqueCamera 16 ms (about 4%); splits add at most 63 vertices (2 KB
  packed). Neither #852's opening scene nor Bistro nor Sponza generates
  (no normal maps, or `TANGENT` on every primitive); the browser module
  grows from 1 344 221 to 1 356 523 B brotli (+12.3 KB, 0.9%) in the `web`
  profile, and the opening scene's first visible world moved within noise
  (319 ms cold with generation, 302 without, three.js 298–306 in the same
  runs). Not byte-exact with mikktspace.c: the corrected features change
  only the last triangle's edge sort and the welding of vertices that
  differ in NaN or in the sign of a zero (compared by `total_cmp` where
  the C compares with `==`); they gave the same tangents as the default
  features on the Khronos NormalTangentMirrorTest (Blender-written, within
  0.028° and every handedness equal), Avocado and WaterBottle, and are
  2–4× faster. The material-level rule reuses the count of frameless
  meshes the scene keeps for anisotropy, with no varying or pipeline key;
  a mesh-level bit would take a varying (14 of 16) or a sentinel in the
  tangent's interpolated handedness. A vertex MikkTSpace leaves without a
  frame (degenerate coordinates alone) takes one in its normal's plane, as
  Bevy takes MikkTSpace's default there, so its mesh keeps its vertex
  frames. Not taken: porting mikktspace.c into `content` as the Meshopt
  decoders were (#854), since Bevy's crate is that port, safe,
  dependency-free and fuzzed against the C original; Bevy's own use, which
  writes each corner's tangent into its shared vertex, so a mirrored seam
  keeps one frame where SGL3D splits.
- **D-89** Decision, 2026-11-24 (#880): in the browser a compile job
  starts WebGPU's asynchronous pipeline creation through `sgl-webgpu`
  (D-75) and returns, so the page keeps presenting while the browser
  compiles. A slot's closure creates its pipelines through the `Creator`
  it is lent and returns a `Creation`, combined with `map`, `zip`, `array`
  and `all`; on native a creation is the value itself, so native creation,
  its timings and a game that never calls `compile` are unchanged. A job
  pushes validation and internal error scopes, runs the closure and pops
  them in one task, and stays pending until the browser resolves its
  creations and scopes; `Compilation` and `CompileJob` keep their shape.
  A frame that meets a job the browser is still compiling cannot wait, so
  it creates the program again in place, as it creates any missing one,
  and drops the job's late result: a closure may run twice. `sgl-post-fx`'s
  `DescribePipeline` hands its descriptor to a sink SGL3D points at its
  creator, so the crate still knows nothing of SGL3D. Not taken: leaving
  the draw out until its pipeline is ready, as Bevy and three.js do, which
  would make drawing without `compile` differ (D-71). Amends #880's plan
  to delete pacing: starting a creation still costs the browser's GPU
  process some work, so a game may start every job at once, ready
  soonest, or a handful a frame, smoothest. As built (Apple M5, Chromium
  149, shared machine, provisional; before is this change with jobs
  creating synchronously, as D-86 left them): the `loading` example's
  block world with every job at once reaches its first level frame in 150
  to 175 ms with a longest gap between animation frames of 67 to 76 ms,
  against 233 to 258 ms and 161 to 180 ms; eight jobs a frame, 192 ms and
  15 ms against 233 to 249 ms and 67 to 68 ms; ordinary, 225 to 242 ms
  and 175 to 184 ms. A cold start compiling for 4.7 s kept its longest gap
  at 76 ms. Bistro readies in 87 ms against 147 ms, its longest gap 14 ms
  either way; the startup comparison's prepared path readies cold in 83
  ms with a longest gap of 20.5 ms against 118 ms and 37.9 ms, and warm
  in 26 to 29 ms with 16 to 19 ms either way. Native, alternating with
  `main` under the same load, did not differ: four job threads 77 to 100
  ms against 91 to 98 ms, ordinary 113 to 179 ms against 110 to 210 ms.
  The asynchronous creation's shape follows gfx-rs/wgpu#10438; the
  creator began on the `t3/evaluate-sgl3d-renderer-alternatives` branch.
- **D-90** Decision, 2026-11-24 (#889): `Renderer::compile` readies the
  first refresh of each of the scene's live probes as well as the frame,
  so after its jobs that refresh creates no shader module or pipeline on
  the thread that calls it. In its planning pass, after the views, it
  runs a planning capture for each live probe: the refresh's own
  `encode_capture`, through the primary view with its input and settings,
  at the probe's centre now and leaving out its hidden groups, all six
  faces, into the dropped encoder, with capture views, a shadow-record
  copy and a face target of its own made for the call. The probe prefilter's
  programs become one `Compiled` slot the renderer holds for every
  capture, a bake's too, kept once made as a stage keeps its programs;
  its sized targets stay in `ProbePrefilter`. A scene without live probes
  plans nothing more, and a game that never calls `compile` creates at its
  first refresh what it did. Not taken: the guide's advice until now, a
  refresh made while the level loads, which compiles the capture's
  programs one after another on the loading thread and publishes a cube
  lit before any frame (no procedural sky, no local-light shadows, which
  a refresh takes from the view's last frame); a planning capture with
  unculled faces or without the probes' hidden groups, which asks for
  variants no refresh draws; planning into the refresh's own staging and
  views, which changes what a refresh in flight keeps; planning only one
  probe, which leaves another's variants (a material only it shows) to
  its first refresh; planning bakes, which `compile` cannot foresee.
  Measured (Apple M5, Metal; Chromium 149 through `sgl-webgpu`; shared
  machine, provisional) on the `loading` example's block world with a
  live probe at the camera's start, warm unless said. Natively, the first
  refresh took 7.9 to 10.6 ms on the calling thread and created 4
  pipelines without `compile`, as on the commit before (7.8 to 8.0 ms,
  4 pipelines), and 623 to 637 ms on a start where the driver compiled;
  after `compile` it took 2.0 to 2.4 ms and created none, on such a start
  too. `compile` ran 42 jobs, against 38 with the probe removed: three are
  the capture's (the prefilter's set of two pipelines, and two geometry
  variants) and one the frames' probe culling; its calls took 1.3 to 2.0
  ms each on the loop's thread with the probe and 2.0 to 3.3 ms without,
  within the machine's noise. In Chromium, with D-89's asynchronous jobs,
  the call took 1.1 to 1.6 ms either way; without `compile` the refresh
  completed on the GPU 23 ms later and held a loading-screen frame 11 to
  12 ms while the browser compiled, and after it 15 to 16 ms later,
  holding none.
- **D-91** Decision, 2026-11-24 (#842, #901): screen-space indirect light
  places a slice's samples at `(k + noise) / n` of the projected radius
  plus XeGTAO's 1.3-pixel floor, evenly as the visibility-bitmask paper
  spaces them (`xegtao_sample_s` in `xegtao_kernels.wgsl`, which ambient
  occlusion shares, squared); it releases its pipelines, programs and bind
  groups with its targets in the first frame no view runs it, and
  `Renderer::compile` readies them again as jobs once it is back on.
  SSRT3's `radius / (steps + 1)` never sampled the last step of the
  radius. `receivers_toward_the_radius_take_coverage` reads `Low` once
  settled (16 frames): `Low`'s one slice a frame is one direction, so a
  single frame shows its noise draw. The steps planned to make `Low` cost
  what ambient occlusion does were dropped: choosing the depth mip by
  distance (XeGTAO's rule) and `Low`'s nearest-texel read showed no
  measurable saving, and `Low` at 5 steps a side, about 0.6 ms on `street`
  and nothing measurable on `bevy`, made the bounce by a wall 38% dimmer.
  The cost target was not reached; the timing was stopped at the owner's
  word after four clean rounds. On an Apple M5 at 1920x1080 on
  `bistro-daylight` (ambient occlusion at Medium, median and range of four
  interleaved runs) `Low` adds 4.1 (3.7–5.1) ms to the frame on `street`
  and 3.8 (3.5–5.2) on `bevy`, `High` 5.4 (5.1–6.2) and 4.6 (4.5–5.4), and
  the spacing fix changed neither measurably.
  Rationale: the radius a level uses is the distance out to which it finds
  a surface, and a feature that is off holds nothing.
- **D-92** Decision, 2026-11-24 (#881, closed not planned): a `View` is
  not made `Send`. #881 asked for it so a native game could run
  `Renderer::compile` on a loading thread; since then every stage's
  programs compile in jobs the game runs on threads of its choosing (D-86),
  and `compile` itself only plans. What is left on the thread that owns the
  view is FSR's own pipelines, which `sp-fidelity` creates where
  `View::new` or `View::resize` builds its context, behind the game's
  loading screen, and nothing for a view that does not select FSR. Against
  that, `View: Send` would bind every field of every later stage and
  anything a game hands a view, need a breaking release of the port for its
  shared handles, and hold on native only, since WebGPU's objects do not
  cross threads. If FSR's hold at context creation comes to matter, the
  route is narrower: `sp-fidelity` hands its pipeline creation out as jobs,
  as every stage of SGL3D does (AR-13), which removes the one exception
  without constraining the view. Not built now: no game has shown the hold
  to matter (D-76).
- **D-93** Decision, 2026-11-24 (#905): the soft shadow filter follows a
  receiver's plane no steeper than 10 m toward the light per metre across
  (`receiver_plane_normal`), DiligentFX's default
  `fReceiverPlaneDepthBiasClamp`, which #841 had left out. Unbounded, a
  receiver nearly edge on to its light compared its uphill taps past any
  occluder above it and took up to half the light through it, which
  Sponza's normal-mapped curtains showed as speckles. Known limit, accepted:
  a surface the light grazes within 5.7° of edge on counts some of its own
  texels as blockers again, so open ground under the default 0.53° sun
  keeps 0.91 of its light at 2° high and all of it from 4°, and under a sun
  widened to 6–10° keeps 0.22–0.86 from 2° to 5° high (1 unbounded; the
  fixed filters 0.81–0.89). A light of radius 0.5 m still lights points
  0.3–0.5 m beneath a roof it meets edge on at 0.08–0.17.
  Rationale: a leak of up to half the light through a solid occluder is
  worse than dimmer open ground under a sun below 6°, and the bound is the
  ported reference's own.
- **D-94** Decision, 2026-11-25 (#914): SGL has kinematic physics, in its
  own crate `sgl-physics`, for 2D and 3D: shapes, queries, worlds a game can
  answer for itself (tile and voxel grids among them), a mover, moving
  colliders, triggers and planar bumps. It has no rigid-body simulation: a
  game whose play is stacking, joints or rag dolls adds Rapier itself. The
  crate's state is plain data so that authority, prediction and replay over
  `sgl-net` are a copy and a repeat. Box against box is SGL's own
  closed-form sweep; round and rotated shapes are ported from the collision
  half of Box2D and Box3D (MIT). Porting Jolt, or all of Box2D or Box3D, was
  declined: the owner's games are kinematic, a C or C++ engine fits the Rust
  browser build badly, and whole-world snapshots (experimental in Box2D 3.2,
  not offered for rollback in Box3D) are a heavier way to roll back than
  copying a body. `sgl_core::collision` moves into the crate and is deleted
  from `sgl-core`. Supersedes D-33.
  Rationale: the games had each rewritten the same movement, collision and
  prediction code, and their networking model wants pure functions over
  plain state, identical on native and in the browser.

- **D-95** Decision, 2026-11-25 (#918, owner's direction): the line SGL
  drew at "prediction is the game's" is not a strict boundary. The
  bookkeeping moves into SGL as `sgl_core::prediction`: input history with
  reset and replay, a server input queue, a fading visual correction,
  snapshot interpolation, previous and current samples, and their counters.
  Policy stays the game's: what is predicted, the messages, who has
  authority, and when to snap. The game gives its step and interpolation
  functions; SGL owns no loop, connection or message. A correction fades
  linearly to zero over a time the game chooses, so it reaches zero exactly
  without a hidden snap threshold.
  Rationale: SGL's point is to absorb what several games can reuse, and
  things move into it as they are needed. Policy differs between games;
  the bookkeeping does not. Six of the owner's games wrote it, a hundred or
  two lines a side, and it is where their netcode bugs lived.

- **D-96** Decision, 2026-11-25 (#915): the frozen cross-target digests of
  `tests/sgl-physics/tests/parity.rs` are refrozen by each slice of the
  kinematic physics plan (#914) that adds to their levels, changes a state
  type's encoding or changes a movement rule, until the crate's first
  release. They exist to show that native and `wasm32` agree, which each
  refreeze confirms on both targets, and no game depends on their values
  yet. The commit that changes one says what changed it. #914's review
  fixes changed the collider encoding; #915 added ramps to the levels, the
  cut to the encoding, and the floor rule; #916 grew `Contacts`' encoding
  and added mover walks to the levels, and its review fixes changed the
  mover's velocity rule, floor, step-up and push-out. After the first
  release a change to one is a breaking change like any other frozen
  value.
