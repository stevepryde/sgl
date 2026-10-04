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
  occluder's depth in the cascade's map, so an occluder beyond the shadow's
  `pancake_size` toward the light counts from the pancake's edge.
  Surfaces' shadows, local lights' shadows in the fog, and fog beyond the
  shadow distance (unshadowed) are unchanged.
- **Migration:** no game-code changes. Afterwards, look at light shafts and
  at fog behind thin occluders (railings, foliage, window frames) under the
  shadowed directional light.

### Directional shadow cascades reach a pancake toward the light

- **Scope:** `sgl-3d` directional shadows. `DirectionalShadow` gains
  `pancake_size: f32` (Godot's `directional_shadow_pancake_size`, metres)
  and implements `Default` (Bevy's 150 m, 4 cascades and 10 m first split,
  with Godot's 20 m pancake). Each cascade's near plane sat on the top of its
  slice of the view, toward the light, and every caster between it and the
  light was recorded at that plane's depth. The near plane now lies
  `pancake_size` beyond, as Godot's does, so a caster within that margin is
  recorded at its own depth and only one farther away at the margin's edge.
  The camera's surfaces are shadowed as before (their test only asks
  whether a caster lies in front); each cascade's depth spans that many more
  metres, which `Depth32Float` holds to well under a millimetre. Probe
  captures and world-space ray hits take the first cascade whose map holds a
  surface, which may now be a nearer, finer one for a surface toward the
  light from a cascade's part of the view, so captures and reflections can
  differ slightly; no re-capture is required, and `pancake_size: 0.` gives
  the previous fit.
- **Migration:** a `DirectionalShadow { .. }` literal that names every field
  adds `pancake_size: 20.` or ends with `..Default::default()`:

  ```rust
  // Before
  shadow: Some(DirectionalShadow { distance: 150., cascades: 4, first_split: 10. }),
  // After
  shadow: Some(DirectionalShadow { distance: 150., cascades: 4, first_split: 10., ..Default::default() }),
  ```

  No other game-code changes. Afterwards, check shadows near the camera
  under the key light.

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
