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

- **D-33** Owner direction, 2026-10-08: SGL has no physics engine; games
  choose their physics. Most games, and especially multiplayer ones, use
  custom arcade physics, in 2D on `sgl_core::collision`; a rigid-body engine
  (Rapier, as the game's own dependency) is for games whose play is simulated
  physics. SGL builds physics only where it brings a significant benefit.
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
