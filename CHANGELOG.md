# Changes and migrations

Migration guidance for agents maintaining games with SGL. Read all entries
after the game's current version through its target version, in version order.
`Unreleased` describes changes on Git that are not yet in a released version.
See the [update workflow](docs/README.md#updating-a-game) for exact pins and
validation in the consuming game.

Each consumer-visible change records affected crates/symbols, old and new
behavior, concrete migration steps (with before/after code when useful), and
what to exercise in the game afterward. Include settings/defaults, platform
and content-format changes even when the game still compiles. State explicitly
when no migration is needed. Keep entries concise; link to package docs for
full API details.

## Unreleased

### Crystal's rays stop at the far plane

- **Scope:** `sgl-post-fx` screen-space reflections, and so `sgl-3d`'s
  Crystal method. A ray that reaches the far plane now ends there, as AMD's
  hybrid SSSR traversal ends it, instead of descending every level of the
  depth hierarchy first. Rays toward the sky take about 6% fewer steps, and
  the `SSR intersection` pass takes about 4% less time. Reflections are
  unchanged but for one 8-bit step at a few pixels near the horizon, where
  a miss's shorter length weights spatial reconstruction.
- **Migration:** no game-code changes. Afterwards, compare the `SSR`
  timing groups on the game's route.

### Blended surfaces can receive screen-space reflections

- **Scope:** `sgl-3d` `AlphaMode::Blend` is now a struct variant,
  `AlphaMode::Blend { receives_screen_space_reflections: bool }`. Unmarked
  (`false`, as the glTF loader reads `BLEND`), a blended material renders as
  before. Marked, it is a receiver: where it is the nearest receiver it is
  the surface that Crystal and Velvet trace, that composes their result into
  its traced lobe in place of its probe and sky specular, and that TAA, FSR2
  and motion blur reproject and blur by its own depth and motion. This
  changes how a marked material is antialiased even with screen-space
  reflections off: while TAA, FSR2 or motion blur runs, a receiver pass
  (timing group `receivers`, after the opaque stage) draws the receivers'
  depth and motion, so TAA reprojects them by the receiver rather than by
  what lies behind it, and what is seen through a receiver follows the
  receiver's motion. The opaque surface under a receiver keeps its probe and
  sky specular, and world-space rays skip it. The renderer allocates two
  render-size targets (12 bytes per pixel) in the first frame whose scene
  holds a receiver and keeps them; a renderer that has never rendered a
  receiver pays nothing. The blended pipelines bind two more textures, so the device floor
  (S3D-1) rises from 17 to 19 sampled textures per shader stage; no known
  adapter offers 17 or 18 (WebGPU in Chromium reports 16 or 48, Metal, DX12
  and Vulkan 31 or more).
- **Migration:** name the flag wherever `AlphaMode::Blend` is built, and match
  it with `AlphaMode::Blend { .. }`. Keep `false` for today's behaviour:

  ```rust
  // Before
  glass.alpha = AlphaMode::Blend;
  if material.alpha == AlphaMode::Blend { /* ... */ }
  // After
  glass.alpha = AlphaMode::Blend {
      receives_screen_space_reflections: false,
  };
  if matches!(material.alpha, AlphaMode::Blend { .. }) { /* ... */ }
  ```

  Mark water and glass that should reflect the scene with `true`, place
  them as moving instances, and animate their normals in the mesh (see the
  package README's
  [blended receivers](crates/sgl-3d/README.md#blended-receivers) and the
  `water` example). A mesh
  replaced with `Scene::set_model` every frame rebuilds its ray BVH every
  frame. Devices requested with
  `graphics_device::limits` need no change. Afterwards, look at a marked
  surface with screen-space reflections on and off, in motion with TAA and
  FSR2, across a resize and a camera cut, and compare the `receivers`,
  `SSR *` or `Godot SSR *`, `reflection composition`, `blended`, `TAA` and
  `motion blur` timing groups on the game's route.

### Per-light shadow opacity

- **Scope:** `sgl-3d` `Light::shadow_opacity` and
  `DirectionalLight::shadow_opacity`, Godot's `shadow_opacity`: how dark the
  light's shadow is, 0..=1. A shadow's visibility is blended toward
  unshadowed, mix(1, shadow, opacity), on every surface (the camera's, probe
  captures' and ray hits') and in the volumetric fog, as Godot b130438's
  forward and volumetric fog shaders do; at most 0.001 draws no shadow and
  skips the lookup. 1, the default (Godot's), is the previous look.
  `Scene::add_light` and `set_light` refuse a light whose opacity is
  outside 0..=1 or not finite (`SceneError::InvalidLight`); a directional
  light's is clamped to 0..=1, and a non-finite one draws no shadow.
- **Migration:** none where a game builds its lights with
  `..Default::default()`. A `Light` or `DirectionalLight` struct literal
  that lists every field adds `shadow_opacity: 1.`. Afterwards, lower it
  where a light's shadows should let some light through (a cheap stand-in
  for bounced light) and look at those shadows on surfaces and in the fog.

### World-space reflection rays cost what they reach, not the instance count

- **Scope:** `sgl-3d` scene rays (world-space reflections). A ray walked
  every capture-visible instance in turn, and every traced frame rebuilt
  and uploaded the whole instance list, so the `world reflection rays`
  timing group grew with the scene's instance count. The ray source is now
  two-level: each instance keeps one entry at its index, written only when
  it is added, re-posed or its model's geometry is replaced, and two
  instance BVHs, one over the static and one over the moving instances,
  are built on the CPU, the moving one every traced frame and the static
  one after a static edit. A ray walks the BVH of the kind it needs, then
  the models it reaches. Rays see the same geometry as before (capture-visible,
  non-deforming instances), so reflections look the same. Adding an
  instance also reserves room for its kind's instance BVH in the ray
  source, about 30 bytes an instance in doubling steps, so `add_instance`
  can return `SceneError::DeviceLimit` when the ray source is nearly full.
- **Migration:** no game-code changes. Afterwards, compare the
  `world reflection rays` timing group on the game's route; scenes with many
  instances should see it fall.

### TAA finds the closest motion vectors in its resolve

- **Scope:** `sgl-post-fx` `TemporalAntiAliasing` and `PostFXContext`, and
  so `sgl-3d` TAA. `PostFXContext::execute` computed the closest motion
  vectors in a full-screen pass for TAA, and TAA copied them each frame for
  the next frame's history rejection. TAA now finds them in its resolve, as
  Godot's TAA resolve finds the velocity of the closest depth, and writes
  them beside its accumulated frame for the next frame: one full-screen
  pass, one texture and the per-frame copy fewer. Resolved frames are
  unchanged, but for the one after a frame that copies its input (the first
  of a new TAA feature set, DFX-2), which compares with that frame's motion
  vectors rather than their closest: different only within a pixel of a
  depth edge. Removed: `post_fx_context::CreateInfo::compute_closest_motion`,
  `PostFXContext::get_closest_motion_vectors` and
  `post_fx_context::RenderAttributes::motion_vectors_srv`. Added:
  `temporal_anti_aliasing::RenderAttributes::depth_buffer_srv` and
  `motion_vectors_srv`, the depth buffer and motion vectors the context
  took; the motion vectors must be a filterable float format, such as
  `Rg16Float`. SGL3D's `DiligentFX closest motion` timing group is gone;
  that work is now in `TAA`.
- **Migration:** games using SGL3D need no code changes; a game that reads
  timing groups by name stops reading `DiligentFX closest motion`. Code
  calling `sgl-post-fx` directly passes the motion vectors to TAA instead of
  the context and drops `compute_closest_motion`:

  ```rust
  // Before
  let mut context = PostFXContext::new(&device, &queue, CreateInfo {
      transition_duration: 0.,
      compute_closest_motion: true,
  });
  context.execute(&mut post_fx_context::RenderAttributes {
      curr_depth_buffer_srv: &depth,
      prev_depth_buffer_srv: &previous_depth,
      motion_vectors_srv: &motion,
      /* ... */
  });
  taa.execute(&mut temporal_anti_aliasing::RenderAttributes {
      color_buffer_srv: &color,
      /* ... */
  });
  // After
  let mut context = PostFXContext::new(&device, &queue, CreateInfo {
      transition_duration: 0.,
  });
  context.execute(&mut post_fx_context::RenderAttributes {
      curr_depth_buffer_srv: &depth,
      prev_depth_buffer_srv: &previous_depth,
      /* ... */
  });
  taa.execute(&mut temporal_anti_aliasing::RenderAttributes {
      color_buffer_srv: &color,
      depth_buffer_srv: &depth,
      motion_vectors_srv: &motion,
      /* ... */
  });
  ```

  Afterwards, compare the `TAA` timing group with the former `TAA` and
  `DiligentFX closest motion` on the game's route.

### Rays decode compressed material images from their stored blocks

- **Scope:** `sgl-3d` scene rays (world-space reflections). A BC7 material
  image (`asset::Image::Compressed`) kept a CPU-decoded RGBA8 copy of its
  level 0 in the ray source, four bytes a texel; the ray source now holds
  level 0's stored blocks, one byte a texel, and a ray decodes each texel it
  samples (bcdec's BC7 decoder, as Godot vendors it). Such an image takes a
  quarter of the ray memory it took (a 2048² image: 16 MiB, now 4 MiB), and
  adding one no longer decodes it on the CPU. Rays read the same texels as
  before, raster's level 0, so reflections look the same. A texel decoded
  from BC7 costs more shader time than an RGBA8 one, so the
  `world reflection rays` timing group may rise where reflection rays hit
  moving objects, or pass masked materials, with compressed images. RGBA8
  images are unchanged. `sgl-3d` now carries bcdec's MIT notice
  (`src/LICENSE-bcdec.txt`), from which the shader's decoder derives.
- **Migration:** no game-code changes. Regenerate the game's distribution
  notices for the new bundled notice. Afterwards, compare GPU memory and the
  `world reflection rays` timing group on the game's route.

### Crystal's reflections no longer smear under fast motion

- **Scope:** `sgl-post-fx` `ScreenSpaceReflection`, and so `sgl-3d`
  `ReflectionMethod::Crystal`. Its temporal pass reprojected reflections by a
  virtual point placed too far behind the surface and kept surface-motion
  history that no longer matched the reflection, so on glossy floors in fast
  motion reflections of bright fixtures smeared into streaks over several
  frames. The virtual point now lies the hit distance beyond the surface along
  the view ray, a surface history far from the current neighbourhood is
  rejected, both as AMD's reflection denoiser does, and history is clamped to
  Wicked Engine's 2 standard deviations instead of 2.5. Still reflections
  measured unchanged (the 2-deviation box applies to them too).
  `sgl-post-fx` now carries AMD FidelityFX Denoiser's MIT notice
  (`LICENSE-amd-fidelityfx-denoiser.txt`).
- **Migration:** no game-code changes. Regenerate the game's distribution
  notices for the new bundled notice. Afterwards, look at Crystal's
  reflections on glossy floors while the camera moves fast.

### An example writes the example grate

- **Scope:** `sgl-3d` adds the `export_grate` example, which writes
  `examples/grate.ktx2`, the BC7 grate of the `offscreen` and `browser_smoke`
  examples (`cargo run -p sgl-3d --example export_grate`). It replaces the
  ignored test `export_example_grate`; the file is unchanged.
- **Migration:** no game-code changes.

### FSR2 sharpening, SMAA quality and anisotropic filtering are settings

- **Scope:** `sgl-3d` adds four `settings::Settings` fields, each defaulting
  to what SGL3D rendered before:
  - `fsr2_sharpening: bool` (true) and `fsr2_sharpness: f32` (0.8): AMD's
    RCAS sharpening of FSR2's output, a slider AMD's FSR2 guide asks games
    to offer; 0 sharpens least, 1 most; values outside 0..=1 are clamped,
    NaN to 0.
  - `smaa_quality: SmaaQuality` (`Low`, `Medium`, `High`, `Ultra`; Medium):
    SMAA 2.8's presets. Medium is unchanged; Low searches less at a higher
    threshold; High and Ultra search further and add SMAA's diagonal and
    corner detection, and Ultra a lower threshold.
  - `anisotropic_filtering: AnisotropicFiltering` (`Off`, `X2`, `X4`, `X8`,
    `X16`; X8): the material textures' anisotropic filtering, as Godot's
    levels.

  A change to the SMAA quality or the anisotropic filtering applies on the
  next rendered frame, which rebuilds SMAA's two preset pipelines or every
  material's sampler and group. `Settings` deserialises older saved values
  with these defaults.
- **Migration:** no game-code changes where `Settings` is built from
  `Settings::default()` or `..Settings::default()`; a struct literal that
  lists every field adds the four. To offer them:

  ```rust
  use sgl_3d::settings::{AnisotropicFiltering, Settings, SmaaQuality};
  let settings = Settings {
      fsr2_sharpness: 0.5,
      smaa_quality: SmaaQuality::High,
      anisotropic_filtering: AnisotropicFiltering::X16,
      ..Settings::default()
  };
  ```

  A game that offers these should check SMAA High and Ultra on its diagonal
  edges, FSR2 sharpening on fine detail, and distant floors and walls with
  each filtering level.

### TAA keeps history at racing speed, as Godot's TAA

- **Scope:** `sgl-post-fx` `TemporalAntiAliasing`, and so `sgl-3d` TAA
  (`Antialiasing::Taa`, High's default). TAA dropped all history where a
  pixel's motion differed from the previous frame's by more than 1/256 of the
  screen height (about 4 pixels at 1080p), which under forward motion left
  most of the frame, and nearly all of its lower third, unantialiased at
  racing speed. It now follows Godot's TAA (`taa_resolve.glsl`, `taa.cpp`).
  Beyond 2.5 pixels of motion difference, each pixel moves 1 % of the
  frame's weight from history to the current frame, so a steady difference
  keeps Godot's 0.9375 − 0.01 × (difference − 2.5) of history: 0.69 at 27.5
  pixels, none from about 96 pixels. History is clipped towards the 3×3
  neighbourhood's mean within a box of clamp(1188 / height, 0.75, 1)
  standard deviations at rest (1 up to 1188 rows) that narrows to none at
  2 % of the screen per frame. Still pixels keep their longer history and
  skip the depth test (DFX-19), but take this box instead of 2.5
  deviations: at most 1, Bevy's clip for still pixels. On screen, fast-moving
  surfaces look antialiased and softer instead of aliased, with slightly
  more trailing at speed (on Hyperdrive's route, from about 0.05 % to 0.3 %
  of the previous frame at 30 and 60 Hz); trails behind objects moving over
  still backgrounds clear sooner; and with the camera stopped, animated
  effects without motion vectors follow sooner, while history on fine
  jittered detail is clipped harder. `sgl-post-fx` now carries Spartan
  Engine's MIT notice (`LICENSE-spartan.txt`), from which Godot's TAA
  resolve derives.
- **Migration:** no game-code changes. Regenerate the game's distribution
  notices for the new bundled notice. Afterwards, with TAA on, look at fast
  camera motion (the near field at speed, for softness and trails), at a
  stopped camera with moving objects, and at a stopped camera's animated
  effects and fine detail for shimmer, especially at 1440p and above, where
  the still box is under 1 deviation.

### A directional shadow's distance no longer turns it off

- **Scope:** `sgl-3d` `DirectionalShadow::distance` is only the shadow's
  reach in metres (AR-5: no floats as flags). A distance that was
  nonfinite or no farther than the camera's near plane (a probe capture's
  centre) cast no shadow; the light now always casts its shadow while
  `DirectionalLight::shadow` is `Some`, and SGL3D keeps the distance at
  least 1 mm beyond where the cascades start and at most 8192 m (NaN as
  0), as Godot's `_light_instance_setup_directional_shadow` keeps its
  distance 1 mm beyond the near plane, with the top of its
  `directional_shadow_max_distance` range (b130438). A distance within
  that range shadows as before.
- **Migration:** a game that set a nonpositive or nonfinite distance to
  switch a directional light's shadow off sets the shadow to `None`:

  ```rust
  // Before
  light.shadow = Some(DirectionalShadow { distance: 0., ..DirectionalShadow::DEFAULT });
  // After
  light.shadow = None;
  ```

  A finite distance beyond 8192 m, which used to reach that far, now stops
  at 8192 m. A game whose distances stay within that range needs no
  changes.
  Afterwards, check scenes that used a zero distance, and probe captures
  with a shadow distance of 0.

### One shadow-quality setting sets the shadow maps' sizes and filter

- **Scope:** `sgl-3d` adds `Settings::shadow_quality`
  (`settings::ShadowQuality`: `High`, the default, or `Low`), Godot's
  desktop and mobile shadow defaults (b130438 `directional_shadow/size`,
  `positional_shadow/atlas_size`, `soft_shadow_filter_quality` and their
  `.mobile` overrides). `High` is what every shadow had before: 2048-texel
  directional cascades, a 4096-texel local-light atlas, and the camera's
  surfaces filtered with Jimenez's spiral under TAA or FSR2, else Castaño's
  kernel. `Low` takes 1024-texel cascades, a 2048-texel atlas with slots of
  half the size, and one hardware 2×2 comparison for the camera's surfaces
  (Godot's hard filter). Probe captures and ray hits keep Castaño's kernel,
  and the fog its one tap, at either. A change reallocates the maps on the
  next frame (or probe capture), which places and draws every shadow
  again. Saved settings without the field load as `High`. Also fixed: the
  camera's local-light shadows took Castaño's kernel under TAA or FSR2 in
  frames where no directional light cast a shadow; they now take the
  spiral, as the package README says.
- **Migration:** no game-code changes where `Settings` comes from
  `Settings::default()`, `..Settings::default()` or deserialization; an
  exhaustive `Settings { .. }` literal adds `shadow_quality`. A game may
  offer it to players ([settings](crates/sgl-3d/docs/settings.md)).
  Afterwards, look at the game's shadows at Low, and at local-light shadows
  under TAA in scenes without a shadowed directional light.

### World-space reflection rays reach 1000 m, as Wicked Engine's

- **Scope:** `sgl-3d` world-space reflections
  (`Settings::world_space_reflections`). Each ray looked for moving objects
  up to 100 m from its receiver; it now reaches 1000 m, Wicked Engine's
  default `Postprocess_RTReflection` range (4323a33 wiRenderer.h), so
  moving objects between 100 m and 1000 m away now appear in reflections
  where screen-space reflections miss them. Rays that hit nothing within
  100 m traverse farther, so the `world reflection rays` timing group may
  rise in large scenes with distant moving objects.
- **Migration:** no game-code changes. Afterwards, look at reflections of
  distant moving objects, and compare the `world reflection rays` timing
  group on the game's route.

### glTF loading takes the game's images instead of decoding them

- **Scope:** `sgl-3d` `asset::LoadOptions` gains `images` and `nodes` and a
  lifetime (`LoadOptions<'a>`), with the new `asset::GltfImage` and
  `asset::ImageSource`; `asset::load_slice_filtered` is removed. The loader
  asks `images` once per glTF image (its
  index, name and, for an external file, its URI) whether to decode it
  (`ImageSource::Decode`) or take the game's `Image`
  (`ImageSource::Supplied`), which it places at that image's index; a
  supplied image is never read or decoded, as Bevy's glTF loader leaves an
  external image to its asset server (9d12036
  `crates/bevy_gltf/src/loader/mod.rs` `load_image`). An embedded glTF may
  now refer to external image files when the game supplies them. Without
  `images` every image decodes as before. A game that replaced a loaded
  asset's images with BC7 chains paid for decoding the PNGs it discarded:
  a GLB with four PNGs (three 2048² and one 1024²) loaded in about 100 ms
  decoding them and 1 ms with all four supplied (Apple M5, release build).
  Mesh node selection moves from `load_slice_filtered` into
  `LoadOptions::nodes`, so a file load selects nodes as bytes did and a
  selection combines with supplied images and the emissive cap; it selects
  as before. Both callbacks are borrowed and `Sync`, so `LoadOptions` stays
  `Send` and `Sync` and one value can serve loads on several threads.
- **Migration:** a `LoadOptions` literal that names every field gains
  `..LoadOptions::default()`:

  ```rust
  // Before
  LoadOptions { emissive_strength_cap: Some(1.5) }
  // After
  LoadOptions { emissive_strength_cap: Some(1.5), ..LoadOptions::default() }
  ```

  A game that replaces images after loading can supply them instead:

  ```rust
  // Before
  let mut asset = asset::load(&path)?;
  asset.images[0] = Image::Compressed(CompressedImage::from_ktx2(&albedo_ktx2)?);
  // After
  let sources = |image: GltfImage<'_>| -> asset::Result<ImageSource> {
      Ok(if image.index == 0 {
          ImageSource::Supplied(Image::Compressed(CompressedImage::from_ktx2(&albedo_ktx2)?))
      } else {
          ImageSource::Decode
      })
  };
  let asset = asset::load_with_options(
      &path,
      LoadOptions { images: Some(&sources), ..LoadOptions::default() },
  )?;
  ```

  A filtered load passes its predicate as `nodes`:

  ```rust
  // Before
  let part = asset::load_slice_filtered(&bytes, |name| name == Some("head"))?;
  // After
  let head = |name: Option<&str>| name == Some("head");
  let part = asset::load_slice_with_options(
      &bytes,
      LoadOptions { nodes: Some(&head), ..LoadOptions::default() },
  )?;
  ```

  A `LoadOptions` that a game stored without a lifetime now needs one:
  build it where it is used, or store a `LoadOptions<'static>` whose
  callbacks are `static` or leaked. A callback that mutates state uses a
  `Mutex` or an atomic, as `Sync` requires. Games that use none of these
  need no changes. Afterwards, check the game's asset load times, its
  compressed materials and its rigid parts.

### AgX looks are a colour grading choice

- **Scope:** `sgl-3d` adds `AgxLook` (`None`, `Punchy`, `Golden`) and
  `ColorGrading::agx_look`: Filament's AgX looks (ef1a133
  `filament/src/ToneMapper.cpp` `agxLook`), a contrast and saturation
  within AgX after its curve. Punchy has more contrast and saturation;
  Golden a golden, slightly washed-out tint. The default, `None`, is the
  look SGL3D rendered before.
- **Migration:** no game-code changes where `ColorGrading` is built with
  `ColorGrading::default()` or `..Default::default()`; a struct literal that
  lists every field adds `agx_look: AgxLook::None`. To choose a look:

  ```rust
  input.color_grading = ColorGrading {
      agx_look: AgxLook::Punchy,
      ..Default::default()
  };
  ```

### The output is dithered

- **Scope:** `sgl-3d`'s tone map always dithers the output with Bevy's deband
  dither (9d12036 `DebandDither::Enabled`, `screen_space_dither`): up to half
  an 8-bit step per channel, in a 2.2 gamma, the same pattern every frame,
  with no control. Smooth gradients such as sky, fog and dark falloffs no
  longer band on 8-bit surfaces. Every frame's output changes slightly: an
  8-bit code may differ by one from before, and RGBA16F outputs carry the
  same noise. The diagnostics tone-mapped capture
  (`DiagnosticTarget::ToneMapped`) stays undithered.
- **Migration:** no game-code changes. A game that compares rendered output
  exactly against stored images re-captures them or compares within one
  8-bit code value. Afterwards, look at gradients in the game's dark scenes,
  sky and fog.

### Bloom's halo shape is SGL3D's

- **Scope:** `sgl-3d` `BloomParameters::low_frequency_boost`,
  `low_frequency_boost_curvature` and `high_pass_frequency` are removed
  (S3D-6: how a feature is done is SGL3D's). SGL3D shapes the halo with
  Bevy's `Bloom::NATURAL` values, 0.7, 0.95 and 1 (9d12036
  `crates/bevy_post_process/src/bloom/settings.rs`), the values
  `BloomParameters::default()` set. `intensity` stays the game's. A bloom
  that used the defaults looks the same; one that set other values now
  takes these.
- **Migration:** delete the three fields from game code:

  ```rust
  // Before
  input.bloom = BloomParameters {
      intensity: 0.1,
      low_frequency_boost: 0.7,
      ..Default::default()
  };
  // After
  input.bloom = BloomParameters { intensity: 0.1 };
  ```

  `..Default::default()` after `intensity` now updates nothing; Clippy's
  `needless_update` flags it. Afterwards, check the game's bright emitters
  and highlights.

### Scenes without decals compile the decal path out

- **Scope:** `sgl-3d` decals (`Scene::add_decal`, `remove_decal`). Every lit
  raster pass and world-space ray hit ran the decal path, so a scene
  without decals paid a fixed per-pixel cost for it. The lit pipelines and
  the world-space reflection trace now apply decals only while the scene
  holds one, as they already shade rectangle lights only while it holds
  one. Frames are unchanged; adding the scene's first decal, or removing its
  last, compiles the lit pipelines for the other case before the next
  frame draws.
- **Migration:** no game-code changes. A game that adds its first decal
  mid-play and cannot afford that compile adds its decals at load.
  Afterwards, compare the `opaque geometry + lighting` timing group (or
  `geometry` and `opaque lighting` where the pass is split) in a scene
  without decals.

### Glow kinds are typed and the tapered profile is the game's

- **Scope:** `sgl-3d` `effects::Glow`. `Glow::kind: f32` (0 uniform, 1
  tapered, 2 line) is now `GlowKind`, whose variants carry what each kind
  reads: `Uniform`; `Tapered { uv, profile }`; `Line { other, offset }`.
  `Glow::uv` and `Glow::other` are removed (a line's `uv[0]` is now
  `offset`; `uv[1]` was unused). The tapered kind's profile, a tapered sine
  hard-coded in the shader, is the new `GlowProfile`: `taper` (alpha ×
  (1 − v)^taper), `ripple_frequency` (radians per unit of u and v) and
  `ripple_amplitude`. `GlowProfile::default()` is the old profile (2,
  [62.83, 18], 0.3), so glow that keeps it looks the same. `Glow` is no
  longer `bytemuck::Pod` or `Zeroable`; it gains `Debug` and `PartialEq`.
- **Migration:** map each kind to its variant and move its values in:

  ```rust
  // Before
  Glow { position, uv: [u, v], color, kind: 1., other: [0.; 3], soft_distance }
  Glow { position: a, uv: [offset, 0.], color, kind: 2., other: b, soft_distance: 0. }
  Glow { position, color, ..Default::default() } // kind 0
  // After
  use sgl_3d::effects::{Glow, GlowKind, GlowProfile};
  Glow { position, color, soft_distance, kind: GlowKind::Tapered { uv: [u, v], profile: GlowProfile::default() } }
  Glow { position: a, color, soft_distance: 0., kind: GlowKind::Line { other: b, offset } }
  Glow { position, color, ..Default::default() } // GlowKind::Uniform
  ```

  A game that cast `&[Glow]` to bytes itself must build the values instead.
  Afterwards, look at the game's tapered and line glow.

### Fog sky affect

- **Scope:** `sgl-3d` `Fog::sky_affect`, Godot's
  `volumetric_fog_sky_affect`: how much of its fog the sky takes, 0..=1.
  Source completion mixes the sky with its fogged self by it (Godot
  b130438 `sky.glsl`); 1, the default (Godot's), is the previous look, in
  which the sky took the whole fog; 0 leaves the sky clear behind fogged
  surfaces.
- **Migration:** none where a game builds `Fog` with `..Fog::default()`. A
  struct literal that lists every field, as the README's example did, adds
  `sky_affect: 1.` (or `..Fog::default()`). Afterwards, set it where the
  game wants a clearer sky and look at its fogged skies.

### Mist drift is the game's

- **Scope:** `sgl-3d` `Mist::drift`: how fast and which way the mist's
  noise moves across each billboard, in billboard widths per second
  rightward and heights per second upward on screen. It was hard-coded;
  `Mist::default()` keeps the old motion, slowly up and to the left. The
  noise's scale, edge and threshold stay SGL3D's.
- **Migration:** none where a game builds `Mist` with `..Default::default()`
  or from `Mist::default()`; a struct literal that lists every field adds
  `drift: Mist::default().drift`. Afterwards, set the drift the game's wind
  wants and look at its mist.

### Auto exposure's histogram range, filter and blend are SGL3D's

- **Scope:** `sgl-3d` `AutoExposure::min_log_luminance`,
  `max_log_luminance`, `filter_low`, `filter_high` and
  `exponential_transition_distance` are removed (S3D-6: how a feature is
  done is SGL3D's). SGL3D meters a histogram of log2 luminance from -8 to
  8, ignores the darkest and brightest 10% of samples, and turns the
  adaptation exponential within 1.5 stops of its target: Bevy's
  `AutoExposure` defaults (9d12036), the values `AutoExposure::default()`
  set. Metering follows `Exposure::stops`, so the fixed range serves any
  scene. `speed_brighten`, `speed_darken`, `correction_min`,
  `correction_max`, `compensation` and `metering_mask` stay the game's. Auto
  exposure that used the defaults looks the same; one that set other values
  now takes these.
- **Migration:** delete the five fields from game code:

  ```rust
  // Before
  input.exposure.automatic = Some(AutoExposure {
      min_log_luminance: -10.,
      max_log_luminance: 6.,
      filter_low: 0.2,
      speed_darken: 2.,
      ..AutoExposure::default()
  });
  // After
  input.exposure.automatic = Some(AutoExposure {
      speed_darken: 2.,
      ..AutoExposure::default()
  });
  ```

  A game that set a custom range covering scenes outside −8..8 moves them
  into −8..8 with `Exposure::stops = s`, which metering follows and the
  correction adds to; to keep its look it also moves its compensation
  curve's x-coordinates by +s and `correction_min` / `correction_max` by −s.
  Afterwards, check the game's auto-exposed scenes, their brightest and
  darkest especially.

### Ambient occlusion's radius no longer turns it off

- **Scope:** `sgl-3d` `FrameInput::ambient_occlusion_radius` is only the
  occlusion's reach in metres (AR-5: no floats as flags). A zero, negative
  or nonfinite radius turned ambient occlusion off; XeGTAO now runs whenever
  `Settings::ambient_occlusion` is not `Off` (with a `perspective` camera),
  and the radius is clamped to 0.01–10000 m (NaN to 0.01): the low end of
  XeGTAO's expected radius range (`XeGTAO.h` `GTAOImGuiSettings`, as Godot's
  `Environment::ssao_radius`) and the upper end its settings clamp to. A
  radius within that range renders as before; a positive radius below 0.01 m
  or above 10000 m, which used to reach the pass unchanged, is now clamped.
- **Migration:** a game that set a nonpositive or nonfinite radius to switch
  ambient occlusion off sets the setting instead:

  ```rust
  // Before
  input.ambient_occlusion_radius = 0.;
  // After
  settings.ambient_occlusion = AmbientOcclusionQuality::Off;
  ```

  `Renderer::render` takes `Settings` every frame, so a scene or camera that
  should have no ambient occlusion passes settings with it `Off`. A game
  whose radius stays within 0.01–10000 m needs no changes. Afterwards, check
  scenes that used a zero radius.

### Reflection source completion is built at renderer creation

- **Scope:** `sgl-3d` `Renderer::new`. It built reflection source completion
  and composition for no screen-space method and no ambient occlusion, so a
  renderer whose settings turn on `Settings::screen_space_reflections` or
  `Settings::ambient_occlusion` compiled a shader module, a compute and a
  render pipeline during its first frame. `Renderer::new` now builds them for
  its settings as a `perspective` camera's frame takes them, and turning
  either setting on or off later still rebuilds them on the next frame.
  Rendered frames are unchanged.
- **Migration:** no game-code changes. Renderer creation now takes that
  compile instead of the first frame.

### The fog's detail spread and history weight are SGL3D's

- **Scope:** `sgl-3d` `Fog::detail_spread` and `Fog::temporal_reprojection`
  are removed (S3D-6: how a feature is done is SGL3D's). SGL3D spaces the
  froxel volume's slices with Godot's default detail spread, 2, and keeps
  Godot's default 0.9 of the last frame's volume where a froxel reprojects
  (b130438 `scene/resources/environment.h`), the values `Fog::default()`
  set. `Fog::length` and the medium stay the game's. A fog that used the
  defaults looks the same; one that set other values now takes these.
- **Migration:** delete both fields from game code:

  ```rust
  // Before
  input.fog = Fog {
      density: 0.02,
      length: 300.,
      detail_spread: 2.,
      temporal_reprojection: 0.9,
      ..Fog::default()
  };
  // After
  input.fog = Fog {
      density: 0.02,
      length: 300.,
      ..Fog::default()
  };
  ```

  A game that set its own spread or history weight now gets Godot's (a
  history weight of 0 no longer turns history off); fog history smearing
  under fast camera motion is SGL3D's to fix (#80). Afterwards, check the
  game's fogged scenes.

### Crystal's tracing and denoising parameters are SGL3D's

- **Scope:** `sgl-3d` `FrameInput::crystal` and `CrystalParameters` are
  removed. Their fields (depth-buffer thickness, roughness threshold, most
  detailed mip, traversal budget, GGX importance-sample bias, spatial
  reconstruction radius, the two temporal stability factors and the
  bilateral cleanup's sigma) are how Crystal
  (`settings::ReflectionMethod::Crystal`) traces and denoises, which S3D-6
  makes SGL3D's. SGL3D keeps their previous defaults at both
  `Settings::screen_space_reflections` levels, so a game that left them at
  their defaults sees no change. A game controls Crystal through
  `Settings::screen_space_reflections` and `Settings::reflection_method`.
  With the `diagnostics` feature, `diagnostics::crystal_roughness_threshold()`
  reports the roughness at which Crystal stops tracing.
- **Migration:** delete the field and type from game code:

  ```rust
  // Before
  use sgl_3d::{CrystalParameters, FrameInput};
  let mut input = FrameInput::new(camera);
  input.crystal = CrystalParameters { roughness_threshold: 0.3, ..CrystalParameters::default() };
  // After
  use sgl_3d::FrameInput;
  let input = FrameInput::new(camera);
  ```

  A game that changed a value gets the defaults instead; for reflections
  that blur with roughness and reach rougher surfaces, use
  `ReflectionMethod::Velvet`. Afterwards, look at the game's glossy
  reflections with Crystal.

### The fog filter loads each froxel once per run

- **Scope:** `sgl-3d` volumetric fog with `Settings::fog_filter` (on by
  default). Each invocation of the filter's x and y passes filtered one
  froxel from 7 loads; it now filters a run of 8 froxels along its pass's
  axis from the 14 their taps reach, so a pass loads 1.75 froxels per froxel
  instead of 7, and the `fog filter` timing group falls. Godot's weights,
  axes, edge clamping and order, the RGBA16F volume between the passes and
  the unfiltered history the next frame reprojects are unchanged: each
  filtered froxel is the same sum as before.
- **Migration:** no game-code changes. Afterwards, compare the `fog filter`
  timing group on the game's route.

### SGL3D places the directional shadow's splits, as Godot does

- **Scope:** `sgl-3d` `DirectionalShadow` loses `first_split`; a game sets
  only `distance` and `cascades` (S3D-6), and `DirectionalShadow::DEFAULT`
  and `Default` stay 150 m and 4 cascades. The splits were Bevy's: the
  first cascade ended at `first_split` and the others at depths spaced
  geometrically from there to `distance`. They are now Godot's
  `DirectionalLight3D` default splits: each cascade but the last ends 0.1,
  0.2 and 0.5 of the way from the camera's near plane to `distance`, and
  the last at `distance`; 2 cascades split at 0.1, 3 at 0.1 and 0.2. This
  changes the look. With a 0.1 m near plane:
  - the default (150 m, 4 cascades) splits at 15.1, 30.1 and 75 m (was 10,
    24.7 and 60.8 m): the first cascade reaches 1.5 times as deep and the
    next two about 1.2 times, so their texels are that much larger and
    shadows near the camera a little softer; the last is unchanged;
  - 200 m in 4 cascades with a 12 m first split splits at 20.1, 40.1 and
    100 m (was 12, 30.7 and 78.3 m);
  - 40 m in 2 cascades with a 10 m first split splits at 4.1 m;
  - SGL3D's examples (20 m in 2 cascades with a 6 or 8 m first split, 15 m
    with 6 m) split at 2.1 and 1.6 m.

  A probe capture's cascades start at its centre: they end at 0.1, 0.2 and
  0.5 of `distance`. A short distance now always splits (a first split
  within the near plane gave one cascade).
- **Migration:** delete `first_split` from every `DirectionalShadow`,
  `const`s included, and `pancake_size` where a game took it from Git
  between releases (SGL3D keeps Godot's 20 m pancake):

  ```rust
  // Before
  const COURSE_SHADOW: DirectionalShadow =
      DirectionalShadow { distance: 200., cascades: 4, first_split: 12. };
  shadow: Some(DirectionalShadow { distance: 40., cascades: 2, first_split: 10. }),
  // After
  const COURSE_SHADOW: DirectionalShadow = DirectionalShadow { distance: 200., cascades: 4 };
  shadow: Some(DirectionalShadow { distance: 40., cascades: 2 }),
  ```

  `distance` and `cascades` remain the game's controls: a shorter distance
  gives finer shadows in every cascade. Afterwards, look at shadows near
  the camera and across cascade transitions under the shadowed light,
  especially in close scenes with few cascades. Existing probe captures
  stay valid; a new capture may differ slightly where the light is
  shadowed.

### The atmosphere is off by default, as Godot's fog

- **Scope:** `sgl-3d` `FrameInput::atmosphere`, which turns the volumetric
  fog (`FrameInput::fog`, fog volumes) and mist (`Scene::update_mist`,
  `FrameInput::mist`) on for the frame. `FrameInput::new` set it to `true`,
  so a frame with a fog medium, fog volumes or mist drew them unless the
  game opted out; it is now `false`, as Godot's `volumetric_fog_enabled` and
  `fog_enabled` default to false (b130438 `scene/resources/environment.h`).
  A frame from `FrameInput::new` draws no fog or mist until the game turns
  its atmosphere on. `Settings::atmosphere`, which allows it, stays
  `true` by default, so the game needs no settings change.
- **Migration:** set the frame's atmosphere where the game wants fog or
  mist:

  ```rust
  // Before
  let mut input = FrameInput::new(camera);
  input.fog = Fog { density: 0.02, ..Fog::default() };
  // After
  let mut input = FrameInput::new(camera);
  input.atmosphere = true;
  input.fog = Fog { density: 0.02, ..Fog::default() };
  ```

  A game that already sets `atmosphere` on every frame needs no change.
  Afterwards, check that the game's foggy and misty scenes still show
  them.

### The fog scatters no ambient light by default, as Godot's

- **Scope:** `sgl-3d` `Fog::ambient` (`FrameInput::fog`), the share of the
  frame's ambient light (hemisphere fill and environment diffuse) the
  medium scatters. `Fog::default()`, and so `FrameInput::new`, set it to 1;
  it is now 0, Godot's `volumetric_fog_ambient_inject` default (b130438
  `scene/resources/environment.h`). With the default, only the directional
  and scene lights light the fog: fog outside their reach and in their
  shadows only dims what lies behind it instead of glowing with the ambient
  light. A game that sets `ambient` sees no change.
- **Migration:** to keep the previous look, set the share explicitly where
  the game builds its fog:

  ```rust
  // Before
  frame.fog = Fog { density: 0.02, ..Fog::default() };
  // After
  frame.fog = Fog { density: 0.02, ambient: 1., ..Fog::default() };
  ```

  Fog volumes (`Scene::update_fog_volumes`) scatter this share too: a game
  that only adds volumes sets `frame.fog.ambient = 1.` to keep their look.
  Afterwards, look at fog in shadow and away from lights, and at distant
  fog against the sky.

### The fog takes one directional shadow cascade and one tap, as Godot's fog

- **Scope:** `sgl-3d` volumetric fog (`FrameInput::fog`) lit by a
  directional light with a shadow (`DirectionalLight::shadow`). Each froxel
  took the camera surfaces' lookup: the cascade at its view depth with a
  2 cm offset toward the light, one hardware 2×2 comparison tap, and a
  second cascade's tap blended in across their 20 % overlap. It now takes
  Godot's volumetric fog lookup: the one cascade at its view depth, no
  offset, one linear tap of the occluder's depth, and the light faded by
  exp(−10 × the metres the froxel lies behind its occluder) rather than cut
  off, so fog fades into an occluder's shadow over its first 10–30 cm (61 %
  of the light 5 cm behind, 37 % 10 cm behind). The metres count from the
  occluder's depth in the cascade's map, so an occluder beyond the
  cascade's 20 m pancake toward the light counts from the pancake's edge.
  Surfaces' shadows, local lights' shadows in the fog, and fog beyond the
  shadow distance (unshadowed) are unchanged.
- **Migration:** no game-code changes. Afterwards, look at light shafts and
  at fog behind thin occluders (railings, foliage, window frames) under the
  shadowed directional light.

### Directional shadow cascades reach a pancake toward the light

- **Scope:** `sgl-3d` directional shadows. `DirectionalShadow` implements
  `Default` (Bevy's 150 m and 4 cascades). Each cascade's near plane sat on
  the top of its slice of the view, toward the light, and every caster
  between it and the light was recorded at that plane's depth. The near
  plane now lies 20 m beyond (Godot's default
  `directional_shadow_pancake_size`, which SGL3D owns), as Godot's does, so a
  caster within that margin is recorded at its own depth and only one
  farther away at the margin's edge. The camera's surfaces are shadowed as
  before (their test only asks whether a caster lies in front); each
  cascade's depth spans 20 m more, which `Depth32Float` holds to well under
  a millimetre. Probe captures and world-space ray hits take the first
  cascade whose map holds a surface, which may now be a nearer, finer one
  for a surface toward the light from a cascade's part of the view, so
  captures and reflections can differ slightly; no re-capture is required.
- **Migration:** no game-code changes. Afterwards, check shadows near the
  camera under the key light.

### Fog volumes cost only the froxels they reach

- **Scope:** `sgl-3d` volumetric fog with scene fog volumes
  (`Scene::update_fog_volumes`, `FogVolume`). The fog injection tested every
  fog volume in every froxel; each frame now bounds the froxels each volume
  may reach from its corners (Godot's per-volume froxel bounds), leaves out
  volumes behind the camera, beyond `Fog::length`, or wholly in front of the
  camera and beside the frame, and sums in each froxel only the volumes
  whose bounds hold it, in the same order. A volume that holds or crosses
  the camera's plane is bounded by the whole frame up to its far end. Each
  froxel's density and albedo-weighted scattering are unchanged; the
  `fog injection` timing group falls by the volumes a froxel no longer
  evaluates.
- **Migration:** no game-code changes. A long fog volume that holds or
  crosses the camera's plane, such as a tunnel's, still costs every froxel
  up to its far end; split it into segments to bound it more tightly.

### Every SGL3D scene and frame value has a default or a constructor

- **Scope:** `sgl-3d`, additive. These now implement `Default`:
  `EnvironmentLight` (unturned at intensity 1, as Three.js's scene
  environment), `Mist` (hidden: black, opacity 0, 1 m billboards),
  `FogVolume` (Godot's: a 2 m cube at the origin of density 1, white albedo
  and edge fade 0.1), `asset::Material` (glTF 2.0's default material, which
  the loader already gave a primitive without one), `SurfaceMaterial` (that
  material's values, `environment_scale` 1) and `effects::Glow` (all zero:
  uniform, hard-edged and colourless). New constructors take the values
  that have no default: `Decal::new(base_color)` (Godot's `Decal` defaults:
  a 2 m cube at the origin, white `color`, `base_color_mix` 1, fades of 0.3
  and no normal fade) and `InstanceState::new(model)` (identity pose,
  `visible` and `capture_visible`). `DirectionalShadow::DEFAULT` is
  `DirectionalShadow::default()` as a `const`. `FrameInput::new` and the glTF
  loader take their values from these defaults, so no image changes.
- **Migration:** no game-code changes. To keep compiling when a value is
  added, build these from their defaults and set only what differs; a
  `const` cannot call `Default::default()`, so it builds from
  `DirectionalShadow::DEFAULT`:

  ```rust
  // Before
  const COURSE_SHADOW: DirectionalShadow =
      DirectionalShadow { distance: 200., cascades: 4, first_split: 12. };
  let decal = Decal { position, rotation: Quat::IDENTITY, size, base_color: paint, normal: None,
      metallic_roughness: None, color: [1.; 4], base_color_mix: 1., upper_fade: 0.3,
      lower_fade: 0.3, normal_fade: 0.5 };
  let state = InstanceState { model, pose: Mat4::IDENTITY, visible: true, capture_visible: true };
  // After (`first_split` goes: see "SGL3D places the directional shadow's
  // splits, as Godot does")
  const COURSE_SHADOW: DirectionalShadow =
      DirectionalShadow { distance: 200., ..DirectionalShadow::DEFAULT };
  let decal = Decal { position, size, normal_fade: 0.5, ..Decal::new(paint) };
  let state = InstanceState::new(model);
  ```

### Per-light fog energy; `Light` and `DirectionalLight` implement `Default`

- **Scope:** `sgl-3d` `Light` and `DirectionalLight` gain
  `fog_energy: f32` (Godot's `light_volumetric_fog_energy`), which scales the
  light each scatters in the volumetric fog: 1 is the look so far, 2 doubles
  it, and at most 0.001 (Godot's cutoff) leaves the light out of the fog, so
  fog injection skips its attenuation and shadow lookup. Surfaces are lit
  alike at any value, and a `baked` light's light in the fog scales too.
  `Scene::add_light` and `set_light` refuse a negative or non-finite
  `fog_energy` with `SceneError::InvalidLight`; a directional light's is
  taken as 0. Both types now implement `Default` with Godot's
  light defaults: `Light` is a white point light at the origin of π candela
  reaching 5 m, live, specular 1, fog energy 1 and no shadow;
  `DirectionalLight` is white, shines along -Z at π lux, with no shadow and
  fog energy 1.
- **Migration:** a `Light` or `DirectionalLight` struct literal must name
  the new field. Add `..Default::default()` (fog energy 1, the look so far)
  or `fog_energy: 1.`; the image is unchanged either way.

  ```rust
  // Before
  Light { position, shape, color, intensity, range, baked: false, specular: 1., casts_shadow: true }
  DirectionalLight { direction, color, illuminance, shadow: None }
  // After
  Light { position, shape, color, intensity, range, baked: false, specular: 1., casts_shadow: true, ..Default::default() }
  DirectionalLight { direction, color, illuminance, shadow: None, ..Default::default() }
  ```

  To cut fog cost, set `fog_energy: 0.` on lights whose light the fog does
  not need (for example many small shadowed fixtures).
- **Validate:** with the fog on, a light at fog energy 0 leaves no glow or
  shaft in the medium but still lights surfaces, and the `fog injection`
  timing falls with the lights taken out.

### Volumetric fog filters its froxels, as Godot's does by default

- **Scope:** `sgl-3d` volumetric fog, and the new `Settings::fog_filter`
  (`bool`, default `true`). The fog stage now runs Godot's filter (b130438
  `volumetric_fog_process.glsl` MODE_FILTER and `fog.cpp`): a 7-tap
  Gaussian across x and then y of each slice of the froxel volume, after
  injection and before integration, as Godot's default
  `rendering/environment/volumetric_fog/use_filter` does. Fog at default
  settings is therefore blurred across neighbouring froxels, with softer
  light shafts and shadow edges in it; the volume the next frame reprojects
  stays unfiltered, as Godot's. `fog_filter: false` gives the previous
  image. A new timing group, `fog filter`, covers the two passes. Saved
  settings without the field load with the filter on.
- **Migration:** no game-code changes where `Settings` comes from
  `Settings::default()`, `..Settings::default()` or deserialization; an
  exhaustive `Settings { .. }` literal adds `fog_filter`. To keep the
  unfiltered fog, set `fog_filter: false`; a game may offer it as a setting
  ([settings](crates/sgl-3d/docs/settings.md)). Afterwards, look at the
  game's fogged scenes with light shafts, in motion.

### One coat Fresnel cosine for every term

- **Scope:** `sgl-3d` shading of coated materials (`clearcoat` above 0).
  The coat's Fresnel toward the view, which dims the light beneath the coat,
  now uses one cosine clamped to [0, 1] (Three.js 0.185.1) for direct,
  ambient, environment and emitted light, and for source completion's
  environment specular and screen-space reflection composition. Before, only
  direct light clamped it at 1. Results change only where rounding put
  the coat normal's dot product with the view above 1, and then by far less
  than a half-float step.
- **Migration:** no game-code changes.

### One motion rule for predecessors behind the camera

- **Scope:** `sgl-3d` motion vectors (the G-buffer's motion target), read by
  TAA, FSR2, screen-space and world-space reflections and motion blur.
  Where a pixel's position in the last submitted frame was on or behind that
  frame's camera, geometry wrote an unbounded offset (which can overflow the
  half-float target) and the sky wrote zero motion, so the sky kept stale
  history. Both now write motion two screens long along its longer axis, in
  the direction the point moved into view, so every temporal effect drops the
  history it reprojects by motion there and motion blur streaks along the
  turn. All motion is now capped at two screens along its longer axis, with
  its direction kept. Motion beyond the cap was already off-screen for every
  temporal effect, but there FSR2's reactive weighting and the reflections'
  choice between motion and hit history can change.
- **Migration:** no game-code changes. Afterwards, check fast camera turns
  (a quarter turn or more within a frame, without `FrameInput::camera_cut`)
  and objects passing close beside the camera, with TAA or FSR2 and motion
  blur on.

### FSR2 runs below a 64-pixel scene size

- **Scope:** `sgl-3d` with `Antialiasing::Fsr2`, through the `sp-fidelity`
  and `sp-fidelity-wgpu` 0.1.1 dependencies. When the scene size (FSR2's
  maximum render size) had no side of 64 pixels or more, the first FSR2 frame
  failed wgpu validation in the luminance pyramid: wgpu's default error
  handler panicked when the frame was submitted, and with a game's own
  `Device::on_uncaptured_error` handler that frame was lost and FSR2 fell back
  to TAA (a failed FSR2 dispatch still escapes its error scope:
  [#30](https://github.com/stevepryde/sgl/issues/30)). The backend now binds
  a luminance mip the texture lacks as its last mip, as AMD's Vulkan backend
  does, so FSR2 runs at those sizes. A scene size with a 1-pixel side still
  cannot create FSR2's context (AMD's SDK sizes a texture at half the maximum
  render size, which would be empty) and falls back to TAA with
  `Renderer::fsr2_error` set.
- **Migration:** no game-code changes. Regenerate the game's distribution
  notices for the new `sp-fidelity` versions; their licences are unchanged.
- **Validate:** with FSR2 chosen, render at a small scene size (for example a
  32×32 view) and check that `Renderer::antialiasing_in_effect` stays
  `Antialiasing::Fsr2` with no `Renderer::fsr2_error`.

### Native WebSocket servers keep accepting after an accept error

- **Scope:** `sgl-net` `NativeWebSocketServer`. Any accept error other than
  `WouldBlock` used to close the listener, so one client resetting in the
  backlog or one moment out of file descriptors ended admission for the life
  of the server while existing peers kept playing. No accept error closes the
  listener now. After `Interrupted`, `ConnectionAborted` or `ConnectionReset`
  the server accepts again at once; after any other error (out of file
  descriptors or socket buffers, a firewall refusal, a pending network error)
  it retries 100 ms later. Accepting stops only when admission stops
  (`ServerIo::stop_admission` or dropping the server). No events or errors
  change.
- **Migration:** no game-code changes.

### Native WebSocket keeps a backpressured peer when a ping or pong is due

- **Scope:** `sgl-net` `NativeWebSocketServer` and `NativeWebSocketClient`.
  A ping, or a pong reply, that met a full send buffer (a slow peer while the
  other side streams state) disconnected the peer with
  `DisconnectReason::Transport`. Both now stay buffered and go out when the
  socket takes writes again, as game frames already did; the caller-clock
  `timeout_ms` still decides when a silent peer has timed out.
- **Migration:** no game-code changes.
- **Validate:** throttle or stall a client while the server streams state;
  it stays connected until the timeout (or reliable overflow) rather than
  dropping as `Transport` at the next ping.

### `sgl_2d::ui::edit_apply` removed

- **Scope:** `sgl-2d`. The public helper `ui::edit_apply` (backspace, then
  append printable characters up to a character limit) is removed. No widget
  used it: `UiFrame::line_edit` edits at its caret, with selection and
  clipboard.
- **Migration:** for a text field, use `UiFrame::line_edit`. A game that
  called `edit_apply` on its own string keeps the same behaviour by copying it
  into game code:

  ```rust
  fn edit_apply(buf: &mut String, chars: &[char], backspace: bool, max_len: usize) -> bool {
      let mut changed = backspace && buf.pop().is_some();
      for &c in chars.iter().filter(|c| !c.is_control()) {
          if buf.chars().count() >= max_len {
              break;
          }
          buf.push(c);
          changed = true;
      }
      changed
  }
  ```

### Reflection history follows Wicked Engine's vicinity search

- **Scope:** `sgl-3d` temporal reprojection for reflections, which world-space
  rays (`Settings::world_space_reflections`) and `ReflectionMethod::Velvet`
  accumulate through. When the reprojected history's depth does not match the
  receiver, the 3x3 search for the closest history depth now moves its centre
  to each better candidate, as Wicked Engine's `ssr_temporalCS.hlsl` does,
  instead of offsetting every candidate from the reprojected point. Near depth
  edges, history can now be taken from more than one texel away, so
  accumulated reflections there change.
- **Migration:** no game-code changes.
- **Validate:** with world-space rays and with Velvet, move the camera so
  foreground objects pass over reflective floors or water, and look at the
  reflections along those objects' silhouettes.

### Distribution notices name path packages without versions

- **Scope:** `scripts/distribution-notices.ts`. Path packages (a workspace's
  own crates, or SGL from a local checkout) are now listed by name only, so a
  version bump no longer changes the generated notices. Registry and Git
  packages keep their versions.
- **Migration:** no game-code changes. A game's next regeneration drops the
  versions from its path packages.

## 0.2.0 — 2026-10-04

### One version for every SGL crate

- **Scope:** `sgl-core`, `sgl-net`, `sgl-input`, `sgl-2d`, `sgl-3d`, and
  `sgl-post-fx` move to `0.2.0` together; SGL crates share one version.
  `sgl-2d` and `sgl-3d` have breaking changes below. The other crates have no
  API changes and are versioned with them.
- **Migration:** change every SGL requirement to `0.2.0` (or `=0.2.0`) in
  one step, then apply the entries below. Do not mix `0.1` and `0.2` SGL
  crates.

### One 2D renderer: `sgl_2d::render` removed

- **Scope:** `sgl-2d`. The compact `render` module is removed with all its
  symbols: `Renderer`, `Sprite`, `SpriteBatch`, `TextureId`, `PixelRect`,
  `FrameOutcome`, `RenderError`, `SpriteError`, `TextureUploadError`,
  `MAX_SPRITES` and `render::RendererInitError`. `canvas` is the only
  renderer; `sgl_2d::canvas::RendererInitError` is unchanged. Canvas APIs and
  output do not change, so games already on `canvas` need no changes.
- **Migration:**
  - Bring-up: replace `render::Renderer::new(window).await?` with
    `canvas::Context::try_new_async(window, vsync).await?` (`try_new` on
    native), then `canvas::Renderer::new(&context, logical_w, logical_h,
    clear_srgb)`. The canvas renders at the logical size and letterboxes to
    the window; call `Renderer::set_target_size` with the surface size to
    render at native resolution. `Renderer::resize` becomes
    `Context::resize`.
  - Textures: replace `upload_rgba8` and `TextureId` with an
    `assets::Texture` in `Assets<Texture>`, uploaded with
    `Renderer::upload_texture(&context, handle, &texture)` and drawn by its
    `Handle<Texture>`. `Renderer::white_texture` supplies the flat-quad
    texture. Sprites with a texture that was never uploaded are skipped, not
    reported as an error.
  - Sprites: replace `SpriteBatch::push(Sprite { .. })` with
    `DrawList::push(SpriteInstance { .. })` (`push_screen` for UI).
    `source: PixelRect::new(x, y, w, h)` becomes
    `src: Some(Rect::new(x, y, w, h))`. `size` becomes
    `scale = size / source size`, multiplied by `pixels_per_unit` under a
    world-unit camera. `position` locates the center rather than `pivot`, so
    offset non-center pivots by `(0.5 - pivot) × size`, rotated with the
    sprite. `rotation_radians` becomes `rot` with the same direction. Order
    by `z`; equal `z` keeps push order.
  - Camera: replace the `view_projection` matrix with `canvas::Camera`.
    `Camera::new(w, h)` gives y-down logical pixels;
    `with_units(WorldUnits { pixels_per_unit, y_up })` gives world units, and
    `center` and `zoom` move it. Camera rotation and custom projections are
    not supported.
  - Colour: `render` read textures as sRGB and took linear `tint` and clear
    colours. The canvas default (`LightingSpace::Gamma`) takes sRGB `color`
    and clear colours: convert with `canvas::linear_to_srgb`. Alternatively,
    pass `LightingSpace::Linear` to `Renderer::with_lighting` to keep
    world-channel colours linear; the clear colour is still sRGB.
  - Frames: replace `renderer.render(clear, view_projection, &batch)` with:

    ```rust
    if let Some(frame) = context.acquire() {
        renderer.render(&context, &frame, &mut draw_list, &camera);
        window.pre_present_notify();
        frame.present();
    }
    ```

    `acquire` returning `None` replaces `FrameOutcome::Skipped`.
  - Device: the canvas requests wgpu's default limits; `render` requested
    downlevel limits. On adapters limited to downlevel limits (some older or
    GL-only GPUs), `Context` bring-up fails with `RendererInitError::Device`.
  - [`examples/direct-game`](examples/direct-game/src/main.rs) is the minimal
    port: a game-owned winit loop drawing one sprite on native and browser.
- **Validate:** on each native and browser target, check sprite size,
  position, orientation and colour; resize and minimize the window.
  No content formats change.

### Shared workspace dependencies and math types

- **Scope:** all dependency versions now live in the root workspace manifest.
  `sgl_3d::glam` moves from glam `0.30` to the workspace's `0.33`, matching
  `sgl_core::math` and `sgl-2d`. No other dependency versions change.
- **Migration:** games with a direct glam requirement used for SGL3D should
  use the workspace requirement (at least `0.33.2` for the camera API), or use
  `sgl_3d::glam`. Matching 2D/3D math values can now
  be passed directly; array conversions used only to bridge glam versions can
  be removed. Keep conversions required by data layouts or coordinate spaces.
- **Camera calls:** replace deprecated `Mat4::look_at_rh` / `look_to_rh` with
  `glam::camera::rh::view::look_at_mat4` / `look_to_mat4`. Projection constructors
  move to `glam::camera::rh::proj::directx` as `perspective`, `orthographic`,
  and `perspective_infinite_reverse`, with the same arguments. These are the
  right-handed, Y-up, zero-to-one depth variants used by WebGPU. SGL's
  `perspective` helper still supplies an infinite reversed-Z projection.
- **Validate:** compile the game and exercise camera movement, shadows, picking
  and shared 2D/3D math on its native/browser targets. No baked asset format or
  rendering setting changes; no bake regeneration is needed.

### Compatibility policy and upgrade guidance

- **Scope:** all SGL crates. Releases may intentionally break compatibility;
  the supported workflow is agent-assisted migration using this changelog.
  Breaking changes during `0.x` advance the minor version.
- **Migration:** no API, runtime behavior, or content format changes in this
  documentation update. Games needing stability should change permissive
  requirements such as `sgl-3d = "0.1.0"` to `sgl-3d = "=0.1.0"` for each
  direct SGL dependency and commit `Cargo.lock`. Git consumers should pin a
  full commit `rev`. When adopting a later release, follow all intervening
  migration entries and validate the affected game workflows.

### SGL3D roadmap status

- **Scope:** `sgl-3d` documentation. The roadmap now separates implemented
  foundations and features from planned dynamic diffuse GI, DLSS/MetalFX,
  hardware ray tracing, and GPU-driven/occlusion culling. Old private issue
  numbers are removed; stable roadmap labels remain.
- **Migration:** none. This corrects status documentation; no rendering APIs,
  settings, defaults, or content formats changed. Use the
  [feature guide](crates/sgl-3d/docs/features.md) for available capabilities.

### Documentation cleanup

- **Scope:** consumer and contributor guides. Dependency requirements now
  point to Cargo manifests instead of repeating current glam/wgpu versions.
  Removed the redundant dependency inventory and contributor publishing section.
- **Migration:** no game-code or dependency changes. Continue using
  SGL math re-exports and the workspace manifest's dependency requirements.

### Bundled third-party notices

- **Attribution:** use Stephen Pryde in copyright notices and generated
  distribution notices. This updates the holder's name, not the licence terms.

- **Scope:** repository notices and contributor tooling. Notices now cover
  copied/ported code and bundled assets, including the IBM Plex test font.
  Removed the generated Cargo dependency licence inventory and its machinery;
  original bundled licences and source attribution remain intact.
- **Migration:** no game-code changes. Game agents must follow the new
  [distribution workflow](docs/licensing.md): generate notices from the game's
  locked graph and build selection, preserve asset notices, and include them
  in native/browser packages. SGL supplies a combined convenience bundle,
  package-local port notices, and explicit upstream refresh tooling. Carry
  the distribution rules into the game's own `AGENTS.md`.

## 0.1.0 — Initial public baseline

- **Scope:** `sgl-core`, `sgl-net`, `sgl-input`, `sgl-2d`, `sgl-3d`, and
  `sgl-post-fx`, all at `0.1.0`. This is the public repository's starting
  snapshot; it does not assert that a crates.io upload has taken place.
- **Migration from the private repository:** update Git dependency URLs from
  `stevepryde/stevegame` to `stevepryde/sgl` and select a revision from the new
  repository. Its history starts fresh, so old commit pins do not exist there.
  The import changed repository metadata and package versions, without
  changing game APIs or baked formats. Existing game code needs no API rewrite
  solely for this import. Build the game against the selected dependency
  revision before committing its updated lockfile.
