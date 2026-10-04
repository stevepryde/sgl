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

### Reflection history follows Wicked Engine's vicinity search

- **Scope:** `sgl-3d` temporal reprojection for reflections, which world-space
  rays (`Settings::world_space_reflections`) and `ReflectionMethod::Velvet`
  accumulate through. When the reprojected history's depth does not match the
  receiver, the 3x3 search for the closest history depth now moves its centre
  to each better candidate, as Wicked Engine's `ssr_temporalCS.hlsl` does,
  instead of offsetting every candidate from the reprojected point. Near depth
  edges, history can now be taken from more than one texel away, so
  accumulated reflections there change.
- **Migration:** no game-code changes. Afterwards, with world-space rays and
  with Velvet, move the camera so foreground objects pass over reflective
  floors or water, and look at the reflections along those objects'
  silhouettes.

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
