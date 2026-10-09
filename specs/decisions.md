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
  `crates/sgl-core/tests/parity.rs` was regenerated with whole-nanosecond
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
