# Changes and migrations

Migration guidance for agents maintaining games with SGL. Read all entries
after the game's current version through its target version, in version order.
`Unreleased` describes changes on Git that are not yet in a released version.
See the [update workflow](docs/README.md#updating-a-game) for exact pins and
validation in the consuming game.

One bullet per change: the crate and symbol, the old and new behaviour in a
clause, and the migration in a line or a short code sample. Record changes
that still compile but alter what a game sees (defaults, settings, units,
platforms, asset and bake formats), and say when no game-code change is
needed. Rationale, measurements and full API details belong in the package
docs and specs the entry links.

## Unreleased

- `sgl-3d` glTF loading: an unsupported extension listed only in
  `extensionsUsed` (an anisotropy texture's extensions included) no longer
  fails the load; it is left out and listed in the new `Asset::ignored`
  (`asset::Ignored`). Only unsupported `extensionsRequired` fail. Migration:
  `Asset` literals add `ignored: Vec::new()`; read `asset.ignored` after a
  load to see what was left out.
- `sgl-3d` occlusion maps, a load error before: one packed in the
  metallic-roughness image's red channel on `TEXCOORD_0` (ORM) occludes
  ambient diffuse and environment specular, taking the lesser of it and
  `Settings::ambient_occlusion`'s visibility; one in its own image or on
  another UV set loads unsampled (`Ignored::OcclusionMap`). New
  `asset::Material::occlusion_texture` (`None`), `occlusion_strength` (1)
  and `SurfaceMaterial::occlusion_strength`.
- `sgl-3d` `KHR_materials_ior` and `KHR_materials_specular`, a load error
  before: their factors set a dielectric's F0 through new `ior` (1.5),
  `specular` (1) and `specular_color` (`[1.; 3]`) on `asset::Material` and
  `SurfaceMaterial`; the defaults keep F0 0.04. Specular textures load
  unsampled (`Ignored::SpecularMap`). Migration: exhaustive material
  literals add the fields or take `..Default::default()`.
- `sgl-3d` shading: reflectance at grazing incidence now follows F0, so
  surfaces with F0 under 0.02 (near-black metals; with the new fields,
  `specular` under 0.5 or an IOR under 1.333) lose grazing reflection with
  it; F0 0.02 and up looks as before. No game-code change; look again at
  near-black metallic materials.
- `sgl-3d` `Scene::add_materials` and `set_material` refuse an IOR below 1,
  `specular` outside 0..=1 or a specular colour that is negative or not
  finite (`SceneError::InvalidReflectance`), and `occlusion_strength` outside
  0..=1 (`SceneError::InvalidOcclusion`). Migration: exhaustive matches on
  `SceneError` add both variants.

## 0.2.1 — 2026-10-07

- Move every SGL crate to `0.2.1` together; no API changes and no game-code
  changes.
- `Settings::ambient_occlusion` costs less GPU time; its output is
  unchanged.

## 0.2.0 — 2026-10-06

No baked or exported asset format changed: nothing needs re-baking. SGL3D's
[agent guide](crates/sgl-3d/docs/README.md),
[features](crates/sgl-3d/docs/features.md) and
[settings](crates/sgl-3d/docs/settings.md) describe 0.2.0.

### Upgrade steps

- Move every SGL crate to `0.2.0` (or `=0.2.0`) together; never mix 0.1 and
  0.2 crates. `sgl-core` and `sgl-input` have no API changes.
- Require wgpu and naga `30.0.1` (30.0.0 panics on WebGPU under current
  `wasm-bindgen`) and `sp-fidelity`/`sp-fidelity-wgpu` `0.2`, as caret
  requirements, not `=` pins. In the browser, require `wasm-bindgen` 0.2.127
  or later, with a `wasm-bindgen-cli` matching the game's `Cargo.lock`
  exactly: `cargo install wasm-bindgen-cli --version 0.2.129 --locked` for
  SGL's lockfile.
- glam 0.33: `sgl_3d::glam` moves from 0.30 to the glam of `sgl_core::math`
  and `sgl-2d`; drop conversions that only bridged them. A direct glam
  requirement needs 0.33.2. `Mat4::look_at_rh`/`look_to_rh` become
  `glam::camera::rh::view::look_at_mat4`/`look_to_mat4`, and projections
  `glam::camera::rh::proj::directx::{perspective, orthographic,
  perspective_infinite_reverse}` (same arguments).
- Game code calling wgpu, such as a 3D game's device and surface setup
  ([wgpu 30 changes](https://github.com/gfx-rs/wgpu/blob/v30.0.0/CHANGELOG.md)):
  `surface_texture.present()` becomes `queue.present(surface_texture)`;
  `SurfaceConfiguration` adds `color_space: wgpu::SurfaceColorSpace::Auto`
  (the old output) and `RequestAdapterOptions` `apply_limit_buckets: false`;
  `get_mapped_range` returns a `Result`; `VertexState::buffers` takes
  `&[Option<VertexBufferLayout>]`; `TextureUsages::TRANSIENT` is
  `TRANSIENT_ATTACHMENT`; WGSL integer vertex outputs declare
  `@interpolate(flat)`; Metal refuses a stage with more than 29 buffers and
  acceleration structures together.
- Regenerate the game's [licence notices](#licence-notices).

### sgl-2d

- `sgl_2d::render` (`Renderer`, `Sprite`, `SpriteBatch`, `TextureId` and the
  rest) is removed: port to the unchanged `canvas`, as
  [`examples/direct-game`](examples/direct-game/src/main.rs) does.
  - `render::Renderer::new(window)` becomes
    `canvas::Context::try_new_async(window, vsync)` (`try_new` natively) and
    `canvas::Renderer::new(&context, logical_w, logical_h, clear_srgb)`
    (`set_target_size` for native resolution); `resize` moves to `Context`.
    The canvas needs wgpu's default limits, not downlevel ones.
  - Textures become `assets::Texture`s in `Assets<Texture>`, uploaded with
    `Renderer::upload_texture` and drawn by handle (`white_texture` for flat
    quads).
  - `SpriteBatch::push(Sprite)` becomes `DrawList::push(SpriteInstance)`
    (`push_screen` for UI): `src: Some(Rect)`, `scale = size / source size`
    (× `pixels_per_unit` in world units), `position` at the centre (offset
    other pivots by `(0.5 - pivot) × size`, rotated), `rot`, ordered by `z`.
  - `view_projection` becomes `canvas::Camera` (`with_units`, `center`,
    `zoom`; no rotation). Tints and clear colours are sRGB
    (`canvas::linear_to_srgb`), or tints stay linear with
    `Renderer::with_lighting(LightingSpace::Linear)`.
  - A frame is `context.acquire()` (`None` replaces `FrameOutcome::Skipped`),
    `renderer.render(&context, &frame, &mut draw_list, &camera)`,
    `frame.present()`.
- `ui::edit_apply` is removed: use `UiFrame::line_edit`, or keep its logic in
  the game (backspace pops a character, then non-control characters append
  up to the limit).

### sgl-net

- `NativeWebSocketServer` keeps accepting after an accept error, where it
  closed the listener; server and client keep a backpressured peer when a
  ping or pong is due, where they disconnected it with
  `DisconnectReason::Transport`. No game-code changes.

### sgl-post-fx

Only for code that drives `sgl-post-fx` itself; SGL3D games need nothing.

- Pass `depth_buffer_srv` and `motion_vectors_srv` (filterable float, such
  as `Rg16Float`) to `temporal_anti_aliasing::RenderAttributes`, and drop
  `post_fx_context::RenderAttributes::motion_vectors_srv`,
  `CreateInfo::compute_closest_motion` and
  `PostFXContext::get_closest_motion_vectors`: TAA finds the closest motion.
- Set each `CameraAttribs`' clip planes with `set_clip_planes(near, far)`
  (far first for reversed-Z), or SSR and TAA keep no history.
- `ScreenSpaceReflectionAttribs::max_traversal_intersections` is capped at 256
  and `spatial_reconstruction_radius` at 8.

### sgl-3d: breaking changes

- **Settings** gains `fsr2_sharpening` (`true`), `fsr2_sharpness` (0.8),
  `smaa_quality` (`Medium`), `anisotropic_filtering` (`X8`), `shadow_quality`
  (`High`), `hardware_ray_tracing` and `ray_traced_shadows` (`false`),
  `ray_traced_shadow_quality` (`Preset`), `occlusion_culling` (`false`),
  `dynamic_gi` (`High`) and `fog_filter` (`true`); `Diagnostics` gains
  `dynamic_gi`. `world_space_reflections` is `WorldSpaceReflections`: `true`
  becomes `Moving`, `false` `Off`. A saved file holding the old bool fails to
  load: convert it where the game loads settings, or drop it (`Off`).
- **Struct literals** of `Settings`, `Light`, `DirectionalLight`,
  `asset::Material`, `SurfaceMaterial`, `asset::LoadOptions`, `Fog`, `Mist`
  and `ColorGrading` that name every field miss new ones (below, plus
  `Fog::sky_affect`, `Mist::drift`, `ColorGrading::agx_look`): build them
  with `..Default::default()`, and use `Decal::new(base_color)`,
  `InstanceState::new(model)` and `..DirectionalShadow::DEFAULT` in a
  `const`. New fields default to 0.1.0's look.
- **Device floor:** 21 sampled textures per shader stage (was 17; no known
  adapter offers 17–20) and `DownlevelFlags::INDIRECT_EXECUTION`, which
  `graphics_device::limits` requests; a game requesting its own must too.
- **Models:** `Scene::add_model` and `set_model` take a `PreparedModel`, not
  `Vec<ModelMesh>`: `scene.add_model(&device, &queue,
  PreparedModel::new(meshes)?)?`. `PreparedModel::new` validates and is
  `Send`, so prepare run-time geometry on worker threads.
- **Scene errors:** exhaustive matches add `TooManyLightmapCharts { .. }`,
  `TooManyLods`, `TooManySections`, `InvalidNormalLayers`, `InvalidOrigin`,
  `InvalidDynamicGiVolume`, `InvalidIrradianceVolume`,
  `InvalidIrradianceRegion` and `IrradianceRegionOutside`. Newly refused: a
  zero vertex normal (`NonFiniteGeometry`), over 65,536 distinct
  `lightmap_bounds` or 256 morph targets in a mesh, over 8 LODs a mesh
  (`set_mesh_lods`), a mesh over 8,388,608 triangles, and `add_instance`
  when the ray source is full (`DeviceLimit`).
- **Lights:** `LightShape::Point` is `Point { radius }` and `Spot` gains
  `radius` (use `LightShape::DEFAULT_RADIUS`; match `Point { .. }`). `Light`
  gains `fog_energy` and `shadow_opacity` (1); `DirectionalLight` those and
  `angular_diameter` (`SUN_ANGULAR_DIAMETER`). Sizes affect only rays.
  `add_light`/`set_light` refuse a negative or non-finite radius or fog
  energy, or an opacity outside 0..=1 (`InvalidLight`).
- **Directional shadows:** delete `DirectionalShadow::first_split`; SGL3D
  places the splits. A zero or non-finite `distance` no longer turns the
  shadow off (use `shadow: None`); distances stop at 8192 m.
- **glTF loading:** `asset::LoadOptions<'a>` gains a lifetime and the `Sync`
  callbacks `images` (return `ImageSource::Supplied(image)`, such as a BC7
  chain, to skip decoding a `GltfImage`) and `nodes`.
  `asset::load_slice_filtered(&bytes, pred)` becomes
  `load_slice_with_options(&bytes, LoadOptions { nodes: Some(&pred),
  ..LoadOptions::default() })`. `load_slice` now decodes `data:` URI images.
- **Materials:** `AlphaMode::Blend` is `AlphaMode::Blend {
  receives_screen_space_reflections }` (`false` is 0.1.0's; match
  `Blend { .. }`). `asset::Material` and `SurfaceMaterial` gain
  `normal_layers` (`None`) and `emits_into_gi` (`true`).
- **Frame time:** `FrameInput::elapsed_seconds` is `f64` (`as_secs_f64()`).
- **Internals now SGL3D's:** delete `FrameInput::crystal` and
  `CrystalParameters`; `Fog::detail_spread` and `temporal_reprojection`;
  `BloomParameters::low_frequency_boost`, `low_frequency_boost_curvature` and
  `high_pass_frequency`; `AutoExposure::min_log_luminance`,
  `max_log_luminance`, `filter_low`, `filter_high` and
  `exponential_transition_distance`. SGL3D keeps the old defaults. Auto
  exposure meters log2 luminance −8..8: to meter outside it, set
  `Exposure::stops = s` and shift the compensation curve's x by +s and
  `correction_min`/`correction_max` by −s.
- **Glow:** `effects::Glow::kind: f32` is `GlowKind`, which takes the removed
  `uv` and `other`: kind 0 is `Uniform`, 1 `Tapered { uv, profile:
  GlowProfile::default() }`, 2 `Line { other, offset }` (`offset` was
  `uv[0]`). `Glow` is no longer `Pod`.
- **Geometry statistics:** `Renderer::geometry_stats(&device)` takes
  `&mut self` and returns `Option<GeometryStats>` a few frames late (`None`
  until one arrives); opaque camera draws count per 128-triangle section.
  `geometry_stats_for_model(&device, &scene, model)` needs `diagnostics` and
  returns `Result<Option<_>, _>`. `lod` is a module (`MeshLod`,
  `MAX_MESH_LODS`).
- **Ambient occlusion:** a zero or non-finite
  `FrameInput::ambient_occlusion_radius` no longer turns AO off: set
  `Settings::ambient_occlusion` to `Off`. Radii clamp to 0.01–10000 m.

### sgl-3d: behaviour changes

These compile unchanged; check them on the game's routes.

- **Atmosphere off by default:** `FrameInput::new` sets `atmosphere: false`:
  set it `true` on frames that should show fog and mist.
- **Fog:** `Fog::ambient` defaults to 0 (was 1), so fog away from lights no
  longer glows (set `1.` for the old look); `Settings::fog_filter` blurs the
  froxels (`false` is 0.1.0's fog); directional shadows in fog fade over
  10–30 cm behind an occluder.
- **Shadows:** cascades split at 0.1, 0.2 and 0.5 of `distance` (Godot's),
  so near shadows are a little softer; receivers offset along the geometry
  normal, so shadow edges on normal-mapped surfaces and water stop crawling.
- **Coated materials:** `clearcoat` dims baked diffuse light (lightmaps,
  atlas charts, ambient cubes) as it dims live light.
- **TAA** keeps history under fast motion, as Godot's: fast surfaces are
  antialiased and softer. Timing group `DiligentFX closest motion` is now
  in `TAA`. TAA and reflections take no history from surfaces behind the
  last camera, and Crystal reflections stop smearing in fast motion.
- **Dither:** the output is always dithered by up to half an 8-bit step:
  exact image comparisons re-capture or allow one code value.
- **Draw order:** opaque and masked draws are built on the GPU, so coplanar
  surfaces in different draws have no defined winner: give an overlay a
  depth offset or make it a `Decal`.
- **World-space reflections** reach 1000 m (was 100 m); timing group
  `world reflection classify` joins `world reflection rays`.
- **Vertex packing:** vertex colours clamp to 0..1 and UVs are 16-bit across
  each mesh's UV rectangle: split a mesh tiled so often that 1/131,070 of its
  UV extent shows. Preparing a model costs more: check remeshing budgets.
- **Decals:** the scene's first decal, or removing its last, recompiles the
  lit pipelines: add decals at load if the hitch shows.
- **FSR2** falls back to TAA with `Renderer::fsr2_error` when wgpu rejects a
  pass, where it panicked, and runs at scene sizes under 64 pixels.

### sgl-3d: new features

- [Dynamic diffuse GI](crates/sgl-3d/README.md#dynamic-diffuse-gi):
  `Scene::set_dynamic_gi_volume` and `Settings::dynamic_gi`; a fixture a
  scene light stands for sets `emits_into_gi: false`.
- [Irradiance volume](crates/sgl-3d/README.md#irradiance-volume) the game
  computes and writes by region: `Scene::set_irradiance_volume`,
  `PreparedIrradianceRegion`, `write_irradiance_cells`.
- [Hardware ray tracing](crates/sgl-3d/README.md#hardware-ray-tracing)
  (opt-in; Metal on macOS 15 Apple silicon, Vulkan, DX12 tier 1.1 with DXC;
  no browser): request `graphics_device::ray_tracing_features(&adapter)`
  with `experimental_features: unsafe { wgpu::ExperimentalFeatures::enabled() }`
  and set `Settings::hardware_ray_tracing`; `Renderer::ray_tracing_in_effect`,
  `ray_tracing_error` and `ray_tracing_stats` report it. No feature
  requires it. Ray-traced shadows (`Settings::ray_traced_shadows`,
  `ray_traced_shadow_quality`) soften by light size while it is in effect;
  without it the shadow maps shadow everything.
- Occlusion culling (opt-in): `Settings::occlusion_culling`; timing groups
  `cull late`, `depth pyramid`, `geometry late`.
- [Scrolling normal layers](crates/sgl-3d/README.md#scrolling-normal-layers)
  animate water without uploads and mark FSR2's composition mask (timing
  group `FSR2 composition`).
- [Blended receivers](crates/sgl-3d/README.md#blended-receivers) take
  screen-space reflections (timing group `receivers`); world-space
  reflections `All` reflect static geometry too, best with hardware rays.
- `Scene::move_origin` keeps a large world near the origin
  ([lifecycle](crates/sgl-3d/README.md#retained-scene-and-frame-lifecycle)).
- [Settings](crates/sgl-3d/docs/settings.md) for shadow quality, SMAA
  quality, anisotropic filtering and FSR2 sharpening; the look fields above.
- [Diagnostics](crates/sgl-3d/README.md#validation-and-diagnostics) counters,
  resource sizes, per-view draws and times, and the `streaming` example.

### Licence notices

- `sgl-3d` and `sgl-post-fx` ship `THIRD_PARTY_NOTICES.txt` for their ported
  code, now including AMD FidelityFX Denoiser, AMD's single-pass downsampler,
  bcdec and Spartan Engine (all MIT). SGL's notices name Stephen Pryde.
- Follow the [distribution workflow](docs/licensing.md): regenerate the
  game's notices from its lockfile, targets and features (for the new ports
  and wgpu 30's dependencies), ship them in native and browser packages, and
  carry the rules into the game's `AGENTS.md`.

### From a Git revision between 0.1.0 and 0.2.0

Apply the entries above that are new since the pin (compare the
[changelog before the release](https://github.com/stevepryde/sgl/blob/095e23613c160aa8e001061f01b8e22040d4bf6a/CHANGELOG.md)),
then:

- `SceneResources::mesh_buffers`/`mesh_buffer_count` are
  `geometry_live`/`geometry_buffers`; `BuildStep` loses `RayWrite` and
  `MeshBuffers` and gains `Pack`, `Place` and `Write`.
- Delete uses of the instance-visibility oracle
  (`Diagnostics::instance_visibility`, `InstanceVisibility`,
  `Renderer::take_instance_visibility`, `InstanceVisibilityReport`) and of
  `DirectionalShadow::pancake_size`.
- `SceneError::TooManyLightmapCharts` is `TooManyLightmapCharts { mesh }`;
  `DynamicGiReport` gains `frame` and `skipped`.
- `Settings::ray_traced_shadow_quality` defaults to `Preset` (Low on the Low
  preset): set `High` for the earlier look.
- Rectangle lights lying on their fixtures are no longer shadowed by them in
  ray-traced shadows and dynamic GI, so what they light is brighter.

## 0.1.0 — Initial public baseline

- **Scope:** `sgl-core`, `sgl-net`, `sgl-input`, `sgl-2d`, `sgl-3d`, and
  `sgl-post-fx`, all at `0.1.0`. This is the public repository's starting
  snapshot, also published on crates.io.
- **Migration from the private repository:** update Git dependency URLs from
  `stevepryde/stevegame` to `stevepryde/sgl` and select a revision from the new
  repository. Its history starts fresh, so old commit pins do not exist there.
  The import changed repository metadata and package versions, without
  changing game APIs or baked formats. Existing game code needs no API rewrite
  solely for this import. Build the game against the selected dependency
  revision before committing its updated lockfile.
