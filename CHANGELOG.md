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

### Two-phase occlusion culling of the camera's list (opt-in)

- **Scope:** `sgl-3d` (#24, roadmap 22). New `Settings::occlusion_culling`
  (`bool`, `false` by default and in every preset) and
  `Renderer::occlusion_culling_in_effect`. While it runs, the camera's
  opaque and masked surfaces are culled in two phases, as Bevy's GPU
  culling runs them. The early phase also tests each instance and then
  each section, at its last frame's pose, against the last submitted
  frame's depth pyramid, and sets aside what lies behind it. The G-buffer
  pass draws the rest. The late phase then builds the pyramid from that
  depth and tests what was set aside again at this frame's pose, and a
  second G-buffer pass draws what it passes. The pyramid is built once
  more from the complete depth for the next frame, and lighting shades
  both sets once. Something hidden last frame and visible now is drawn in
  the same frame. The first frame, a camera cut, a resize and a frame
  after one without occlusion culling cull by frustum alone. The pyramid
  is AMD's single-pass downsampler as Bevy ports it, with AMD's (SDK
  d7531ae) and Bevy's notices. The directional cascades still cull by
  frustum alone.
- **Behaviour:**
  - While it runs, the opaque stage takes its two-pass form (a G-buffer
    pass, then lighting at its depth) on every device, since the fused
    pass cannot be split. The new timing groups are `cull late`,
    `depth pyramid` (twice a frame) and `geometry late`.
  - It needs six storage textures a shader stage, which
    `graphics_device::limits` requests from the adapter. A device with
    fewer culls by frustum alone, as does the `culling` diagnostics layer.
    `occlusion_culling_in_effect` reports it.
  - `Renderer::geometry_stats` counts both phases' sections.
    `Renderer::diagnostic_draws` counts two draws a set for the camera
    while it runs.
  - The camera's lists while it culls occlusion (three entries a draw
    candidate and one a section its sets can draw) always fit within the
    limits the scene already refuses content past
    (`SceneError::DeviceLimit`): no new refusal.
- **Migration:** no game-code changes. Code that builds `Settings` naming
  every field adds `occlusion_culling: false`. A saved settings file loads
  unchanged (`Settings` is `#[serde(default)]`). To opt in, set
  `settings.occlusion_culling = true` where the game's views hide much of
  what they submit, and offer it to players beside the other performance
  settings. Afterwards, measure the GPU frame on the game's routes with it
  on and off, and watch for anything drawn a frame late when the camera
  turns or an occluder moves (nothing should be).

### Dynamic GI traces within a per-frame ray budget

- **Scope:** `sgl-3d` dynamic GI (#185). A volume now traces at most a
  per-frame budget of rays, fixed rays included: 32,768 at
  `DynamicGiQuality::High`, 16,384 at `Low`, after Wicked Engine's surfel
  GI (4323a33c `SURFEL_RAY_BUDGET`). Probes take turns at a period that
  grows with their distance from the camera, and far probes trace fewer
  rays (Wicked's distance boost, 8:1); a probe whose light is changing
  takes its turns more often. Where requests exceed the budget, every
  period lengthens so each probe keeps its turns. A volume whose content
  keeps moving now costs a bounded amount: on Hyperdrive's moving route
  (3,179 probes, High) rays fell from about 124k to 32k a frame and the
  dynamic GI rays pass from about 12 ms (after #187) to about 3.4 ms, about
  1.4 ms with the camera still. A volume converges over volume updates
  rather than frames. New diagnostics observation: `Diagnostics::dynamic_gi`
  with `Renderer::take_dynamic_gi_reports` (probes, rays, BVH visits,
  budget stride, what kept the volume awake).
- **Migration:** no game-code changes. Code that names every field of
  `Diagnostics` adds `dynamic_gi: false`. Afterwards, move a light inside a
  dynamic GI volume and watch how quickly the bounce follows, and fly
  through a large volume to check far probes light up.

### World-space reflections can reach static geometry

- **Scope:** `sgl-3d`: `Settings::world_space_reflections` changes from
  `bool` to `settings::WorldSpaceReflections` (`Off`, the default,
  `Moving`, `All`). `Moving` is the old `true`: rays fill screen-space
  reflections' misses with moving objects, and a static surface in front
  of one leaves the probe or sky reflection. `All` is new: each ray takes
  its nearest hit of either kind, leaving the reflecting surface's own
  triangle, so an off-screen static wall reflects as it stands rather than
  as its probe recorded it, as Wicked Engine's ray-traced reflections trace
  the whole scene. `All` is meant for hardware ray tracing
  (`Settings::hardware_ray_tracing`): on the portable BVHs every ray also
  walks the static geometry. On an Apple M5, over a glossy 1 km strip with
  posts, boxes and 60 moving boxes at 960×540 (full-resolution SSR), the
  world-space ray pass took 0.49 ms under `All` against 0.22 ms under
  `Moving` on the portable BVHs, and 0.11 ms against 0.09 ms with hardware
  ray tracing. No preset turns either on, and the default stays off.
- **Migration:** replace the bool in code that sets the field:

  ```rust
  // Before
  settings.world_space_reflections = true;
  settings.world_space_reflections = false;
  // After
  use sgl_3d::settings::WorldSpaceReflections;
  settings.world_space_reflections = WorldSpaceReflections::Moving;
  settings.world_space_reflections = WorldSpaceReflections::Off;
  ```

  A saved settings file with the old bool fails to load: serde rejects
  `"world_space_reflections": true` (expected a variant name, `"Moving"` in
  JSON, `Moving` in RON), and SGL3D keeps no compatibility shim. Convert
  the field where the game loads its saved settings (`true` becomes
  `Moving`, `false` `Off`), or remove it, which loads `Off` since
  `Settings` is `#[serde(default)]`. A game that offers the setting to
  players can offer `All` beside `Moving`, best where hardware ray tracing
  is in effect (`Renderer::ray_tracing_in_effect`). Afterwards, load a
  settings file saved before the change, and look at glossy floors with
  `All` on: off-screen static walls and props now reflect where the probes
  showed them before.

### The camera and the directional cascades draw from GPU-built lists

- **Scope:** `sgl-3d` (#24, roadmap 22). The camera's opaque and masked
  surfaces and each directional shadow cascade now draw from lists the GPU
  builds every frame, on native and in the browser alike, in the form of
  Bevy's meshlet raster: after the deform pass, a cull stage tests each
  instance's meshes (draw candidates the scene keeps up as it is edited)
  for the view's population, the camera's level of detail and the view's
  frustum, then each chosen mesh's 128-triangle sections, and appends
  those that pass to their set's draw, one `draw_indirect` per set (a
  material, whether the pose mirrors and whether the instance deforms),
  whatever the instance count. The CPU no longer walks the instances for
  those views; blended surfaces, local-light shadow faces and probe
  captures keep CPU-built, instanced lists, the blended walk now over only
  the instances whose models hold a blended mesh. Each GPU-built view keeps
  a cluster list of 20 bytes per section the scene's sets can draw
  (measured 55 KB a view on the `streaming` example's walk and 514 KB at
  its headroom scale; `SceneResources::cluster_list`). Changed
  symbols: `Renderer::geometry_stats`, `Renderer::geometry_stats_for_model`,
  `Renderer::diagnostic_draws`, `Renderer::diagnostic_view_times`,
  `GeometryStats`, `Scene::set_mesh_lods`, `PreparedModel::new`,
  `SceneError` (`TooManyLods` and `TooManySections`, new), `lod` (now a
  module of `MeshLod` and `MAX_MESH_LODS`, new), `diagnostics::SceneResources`
  (`draw_candidates`, `draw_sets`, `level_chains` and `cluster_list`, new).
  Removed with the CPU camera walk: `settings::Diagnostics::instance_visibility`,
  `settings::InstanceVisibility`, `Renderer::take_instance_visibility` and
  `diagnostics::InstanceVisibilityReport` (added earlier in this release
  cycle by #180's measurement, which they served), and the examples'
  `--visibility`.
- **Behaviour:**
  - Equal depth: a GPU-built list draws its sets in their order and each
    set's sections in the order the cull appended them, so two coplanar
    surfaces of different draws in the camera or a cascade no longer have
    a defined winner (the CPU builder drew each instance's meshes in their
    model's order). The blended list keeps its order.
  - `Renderer::geometry_stats` takes `&mut self` and the device and
    returns `Option<GeometryStats>`: the most recent completed frame's counts, read
    back without blocking, a few frames late, and `None` until a frame's
    readback arrives. The camera's opaque and masked draws count one per
    section (at most 128 triangles) appended, with their triangles; its
    blended draws count as before.
  - `Renderer::geometry_stats_for_model` needs the `diagnostics` feature
    and does not exist without it; it takes `&mut self` and the device,
    and returns `Result<Option<(usize, u64)>, _>` of the
    same frame: its instances' sections and triangles with the blended draws
    that hold one.
  - `Renderer::diagnostic_draws` counts a GPU-built view's indirect draws,
    one per set it draws; `diagnostic_view_times`' build time is preparing
    the view's cull and encoding it.
  - Levels of detail: the camera's opaque and masked surfaces choose on the
    GPU, by the same bound under margins for its `f32` rounding: never
    coarser than before, and possibly finer at the margin. Blended
    surfaces still choose on the CPU.
  - Limits: `Scene::set_mesh_lods` refuses more than `lod::MAX_MESH_LODS`
    (8) alternatives a mesh with `SceneError::TooManyLods`, and
    `PreparedModel::new` refuses a mesh past 65,536 sections of 128
    triangles (8,388,608 triangles) with `SceneError::TooManySections`;
    both were accepted before. Content past what the device binds for the
    lists is refused with `SceneError::DeviceLimit`.
  - The `culling` diagnostics layer makes the camera's GPU cull accept
    every candidate and section, as it made the CPU walk submit every
    range.
- **Migration:**
  - Read the statistics a few frames late and handle `None`:

    ```rust
    // Before
    let stats = renderer.geometry_stats();
    hud.triangles = stats.total().1;
    // After
    if let Some(stats) = renderer.geometry_stats(&device) {
        hud.triangles = stats.total().1;
    }
    ```

    A test that read the frame it just rendered submits it, calls
    `finish_frame`, waits with `device.poll(wgpu::PollType::wait_indefinitely())`,
    then calls `geometry_stats(&device)`. Compare the camera's opaque draw
    counts with sections, not instanced draws.
  - Hold the renderer mutably where the game reads its statistics. A game
    that calls `geometry_stats_for_model(&scene, model)` enables the
    `diagnostics` feature (or drops the call; shipping builds should) and
    calls `geometry_stats_for_model(&device, &scene, model)`, which returns
    an `Option` inside the `Result`.
  - Give a coplanar overlay drawn as a separate opaque or masked mesh (a
    decal-like strip, a painted line on a road) a depth offset in its
    geometry, or make it a `Decal`, where it relied on drawing after the
    surface beneath it.
  - Split a mesh past 8,388,608 triangles, and register at most 8
    alternatives a mesh.
  - Code using `InstanceVisibility` or `take_instance_visibility` deletes
    it; `Diagnostics` built field by field drops `instance_visibility`.
  - Exercise afterwards: the game's camera views and shadows over its
    routes (geometry, levels of detail and cascades as before), its HUD or
    tests that read geometry statistics, and coplanar overlays.

### Model BVHs built by the surface area heuristic

- **Scope:** `sgl-3d`: `PreparedModel::new`, and `Scene::add_asset`, which
  prepares on its calling thread, build each model's ray BVH by the binned
  surface area heuristic (Wald 2007; nodes of four or fewer triangles stay
  leaves, as in Embree's builder) instead of a median split, whose halves
  overlapped on long, thin triangles (#187). `add_model` and `set_model`
  take a prepared model and build nothing. A portable scene ray
  (world-space reflections, dynamic GI probe and visibility rays) visits
  about half the BVH nodes and tests about a third of the triangles over
  such content: a dynamic GI probe ray over Hyperdrive's track visited 204
  nodes and tested 34 triangles before, 101 and 10 after (CPU replay), and
  its route's `dynamic GI rays` pass took 11.8 ms rather than 27.9 (Apple
  M5). Instance BVHs keep the median split, which builds fastest on the
  render thread, so `add_instance` and static edits cost what they did.
  A ray's hit is unchanged, except that between two surfaces at exactly the
  same distance along it, the one it meets first in the new tree's order
  may differ. Preparing a model takes longer: a 134,000-triangle model's
  BVH 25 ms rather than 13, and the 128×128 grid the `water` example's
  `set-model` run prepares again every frame on the render thread 5.3 ms
  rather than 4.9 (`PreparedModel::new` whole). The BVH takes about 41
  bytes a triangle of the ray source on Hyperdrive's track rather than 36,
  and as before on a grid or a block world.
- **Migration:** no game-code changes. A game that replaces a model every
  frame, or prepares models under a per-frame or per-tick budget, should
  check that budget. Afterwards, exercise the game's streaming and model
  loading, and its world-space reflections and dynamic GI.

### Hardware-traced scene rays

- **Scope:** `sgl-3d`: `Settings::hardware_ray_tracing` (now traces rays),
  `Renderer::ray_tracing_in_effect` and `Renderer::ray_tracing_error`
  (new), world-space reflections, the dynamic GI volume. The second step
  of hardware ray tracing (roadmap 13, #23). With the setting on, on a
  device with ray queries, world-space reflections' rays and the dynamic
  GI volume's probe and visibility rays now trace the scene's acceleration
  structures instead of the portable BVHs. They take the same acceptance
  rules (sides, blended and hidden content, the reflecting surface's own
  triangle, cut-out texels), and the portable BVHs now cover only the
  instances the TLAS does not hold: models with a masked mesh, models whose
  BLAS is pending, and what the device cannot hold (a deforming instance
  the device cannot hold is seen by no ray). Unlike the portable
  path, these rays see skinned and morphed instances, at their deformed
  pose: deforming characters now appear in world-space reflections, cast
  the dynamic GI volume's visibility shadows and block its rays, and their
  animation counts as an edit that wakes a converged volume. Every native
  backend runs one form of the trace (opaque queries, with a re-trace past
  a hit the rules reject); a triangle at exactly a rejected one's distance,
  such as back-to-back single-sided faces, may be skipped.
  `Renderer::ray_tracing_in_effect(&settings)` says whether the hardware
  path traces the rays; `Renderer::ray_tracing_error()` says why a frame
  that asked for it traced the portable BVHs instead (no ray queries, or no
  memory for the TLAS).
- **Migration:** no game-code changes are required, and a game that does
  not turn `Settings::hardware_ray_tracing` on renders as before. A game
  that turned it on now gets hardware-traced world-space reflections and
  dynamic GI; exercise both with its skinned characters in view, and a
  dynamic GI volume about them, which now repaints as they animate. A game
  that showed `Settings::hardware_ray_tracing` to players can report
  `ray_tracing_in_effect` and `ray_tracing_error` beside it.

### A material can keep its own light out of dynamic GI

- **Scope:** `sgl-3d`: `asset::Material::emits_into_gi` and
  `SurfaceMaterial::emits_into_gi` (new, `true` by default). A glowing
  fixture that is also a scene light reached the dynamic GI volume twice:
  its probes' rays met the fixture and took its light, and the light lit
  the same surfaces. With the field `false`, a probe ray's hit on the
  material takes none of the light it gives off itself (its emission, and
  an unlit material's whole colour); the surface still blocks the ray and,
  when lit, reflects the light that reaches it. World-space reflections
  and probe captures still show it glowing.
- **Migration:** no game-code changes for code that builds these structs
  with `..Default::default()` or from `Scene::material`, and nothing renders
  differently until a game sets the field `false`. Code that names every
  field of `asset::Material` or `SurfaceMaterial` adds `emits_into_gi:
  true`. To stop a fixture's light counting twice, set it `false` on the
  materials of glowing geometry that a scene light stands for:

  ```rust
  // After, before Scene::add_asset
  material.emits_into_gi = false; // a lamp panel with its own rectangle light
  ```

  Afterwards, with dynamic GI on, look at surfaces near those fixtures,
  which take the fixtures' light once.

### A model may name any number of lightmap charts across its meshes

- **Scope:** `sgl-3d`. Since scene vertices were packed (below),
  `PreparedModel::new`, and `Scene::add_asset` through it, refused a model
  whose vertices together named more than 65,536 distinct
  `Vertex::lightmap_bounds` with `SceneError::TooManyLightmapCharts`, which
  0.1.0 accepted: a packed vertex's 16-bit chart index counted across its
  model. Each mesh now keeps its own chart table and the index counts within
  it, so only a mesh naming more than 65,536 is refused, and the error,
  now `TooManyLightmapCharts { mesh }`, names that mesh's index among the
  model's meshes (an asset's mesh index). A chart that several meshes name
  is stored once in each. Vertices stay 32 bytes; lighting is unchanged.
- **Migration:** no game-code changes, except that a `match` arm naming the
  variant becomes `SceneError::TooManyLightmapCharts { .. }`. Content the
  error refused loads again, so a model split to stay under the limit can
  be added whole. Afterwards, exercise the lightmapped and atlas-lit
  surfaces of the game's largest models.

### Scene acceleration structures for hardware ray tracing

- **Scope:** `sgl-3d`: `graphics_device::ray_tracing_features` (new),
  `graphics_device::limits`, `Settings::hardware_ray_tracing` (new, off by
  default and in every preset), `Renderer::ray_tracing_stats` and
  `RayTracingStats` (new), `diagnostics::Counters` and
  `diagnostics::SceneResources` (new fields). The first step of hardware
  ray tracing (roadmap 13, #23), which a game opts in to: on a device with
  wgpu's ray queries and with the setting on, the scene builds
  acceleration structures over its geometry on the frames that trace rays
  (world-space reflections, the dynamic GI volume) — a BLAS for each model
  that does not deform and has no masked mesh, built nearest the camera
  first under a budget of 400 000 vertices a frame (a replaced model at
  once) and then compacted; one for each deforming instance, rebuilt in
  each frame that deforms it; and a TLAS over the instances, rebuilt every
  such frame. Rays still trace the portable BVHs, as before: the hardware
  trace comes in a later change. Models with a masked mesh, models whose
  BLAS is pending and what the device cannot hold (its limits, or memory)
  stay on the portable BVHs, counted by `Renderer::ray_tracing_stats`.
  Turning the setting off frees the structures. `graphics_device::limits`
  now also requests the adapter's `max_blas_primitive_count`,
  `max_blas_geometry_count`, `max_tlas_instance_count` and
  `max_acceleration_structures_per_shader_stage`; an adapter may report
  them without the feature (Metal does), and they take effect only on a
  device with it. `diagnostics::Counters` gains `blas_builds`,
  `blas_build_vertices`, `deformed_blas_builds`, `tlas_builds`,
  `blas_compactions` and `blas_compacted_vertices`, and `SceneResources`
  gains `blases` and `blas_triangles` (wgpu 29 reports no acceleration
  structure's size).
- **Migration:** no game-code changes are required, and nothing renders
  differently: the setting is off unless the game turns it on. A game that
  opts in turns `Settings::hardware_ray_tracing` on and requests the
  feature with wgpu's `unsafe` experimental token, by which it accepts that
  wgpu's ray tracing is experimental:

  ```rust
  // Before
  let (device, queue) = adapter
      .request_device(&wgpu::DeviceDescriptor {
          required_features: sgl_3d::graphics_device::features(&adapter),
          required_limits: sgl_3d::graphics_device::limits(&adapter),
          ..Default::default()
      })
      .await?;
  // After
  let (device, queue) = adapter
      .request_device(&wgpu::DeviceDescriptor {
          required_features: sgl_3d::graphics_device::features(&adapter)
              | sgl_3d::graphics_device::ray_tracing_features(&adapter),
          required_limits: sgl_3d::graphics_device::limits(&adapter),
          // SAFETY: the game accepts wgpu's experimental ray queries.
          experimental_features: unsafe { wgpu::ExperimentalFeatures::enabled() },
          ..Default::default()
      })
      .await?;
  let settings = sgl_3d::settings::Settings {
      hardware_ray_tracing: true,
      ..saved_settings
  };
  ```

  Metal has the feature from macOS 15 on Apple silicon (in hardware from
  M3), Vulkan with ray queries, and DX12 at ray-tracing tier 1.1 where the
  game ships DXC (wgpu's `static-dxc`, or `dxcompiler.dll` beside the
  executable); no browser has it. Until the hardware trace lands, a game
  that opts in pays the structures' memory and build time on frames that
  trace rays without faster rays. Each deforming instance's BLAS is rebuilt
  in every traced frame that deforms it, about 16 µs of GPU time per
  768-triangle instance on an Apple M5 (a crowd of 64 skinned instances
  added about 1 ms a frame), so a game with large crowds budgets for it.
  Saved settings that predate the field load it off (`Settings` is
  `#[serde(default)]`). A game that opts in runs its world-space
  reflections or dynamic GI on a ray-tracing device afterwards and checks
  `Renderer::ray_tracing_stats`.

### Diagnostics measure each view's CPU time

- **Scope:** `sgl-3d` with the `diagnostics` feature (#24's measurement).
  `Renderer::diagnostic_view_times` returns `diagnostics::ViewTimes`: the
  CPU time the last frame's camera and each cascade's draw list took to
  build and to record. In the browser, build steps' times
  (`diagnostics::Counters::steps`) now come from `performance.now()`, where
  they were zero. A frame probed (`Diagnostics::frame_probe`) and abandoned
  before `finish_frame` no longer yields a report when the next submitted
  frame probes nothing. The `streaming` and `irradiance_volume` examples
  print these, with `--split` for the opaque stage's two-pass form, and
  `bun scripts/tasks.ts measure-browser` runs the streaming world in
  headless Chromium. (The instance-visibility oracle this measurement
  added is removed again by the GPU-built lists above.)
- **Migration:** none. Nothing changes without the feature.

### A failed FSR2 dispatch falls back to TAA instead of panicking

- **Scope:** `sgl-3d` with `Antialiasing::Fsr2`, through the `sp-fidelity`
  and `sp-fidelity-wgpu` 0.1.2 dependencies. When wgpu rejected a view or
  bind group of one of FSR2's passes, the backend had already recorded that
  pass into the frame's encoder, so the game's `finish` found the encoder
  invalid: wgpu's default error handler panicked, and with a game's own
  `Device::on_uncaptured_error` handler the whole frame was lost. The
  backend now fails such a pass before recording it and stops the passes
  after it (its SDK-P28), and SGL3D reads the failure the backend reports,
  since FSR2's dispatch ignores its passes' result. The frame completes
  without FSR2, its render-size image scaled to the scene size as for a
  camera FSR2 cannot take, and TAA runs from the next `Renderer::resize`;
  `Renderer::fsr2_error` says why (wgpu's reason where wgpu rejected a
  pass). Frames whose dispatch succeeds are unchanged.
- **Migration:** no game-code changes. Regenerate the game's distribution
  notices for the new `sp-fidelity` versions; their licences are unchanged.

### SSR and TAA take no history for a surface behind the last frame's camera

- **Scope:** `sgl-post-fx` screen-space reflections' and TAA's temporal
  accumulation, and so `sgl-3d`'s Crystal method and TAA (DFX-32). Both
  compare each surface's depth reprojected into the last frame
  (`ComputeReprojectedDepth`) with that frame's depth buffer. A surface that
  was behind the last frame's camera (clip w below 0), as after a quick move
  backwards with a turn, got a depth mirrored to as far in front of it, and
  where a surface the last frame saw lay near that depth, the passes kept
  history the surface never had; Crystal took it through its reflection-hit
  reprojection, whose virtual point can still lie in front of that camera.
  Such a surface now reprojects to the last frame's near plane, and both
  passes treat a reprojected depth at or nearer than that plane as
  disoccluded, as they do a surface the near plane would have clipped. This
  is a correctness fix with no setting; DiligentFX divides unguarded. In
  `sgl-3d` the change is mainly Crystal's: TAA already dropped that history
  by its motion, which is two screens long for such a surface, except where
  a pixel at a silhouette takes a neighbour's on-screen motion (TAA uses the
  nearest depth's motion in each 3×3 neighbourhood). Frames where no surface
  lies behind the previous camera are unchanged.
- **Migration:** no game-code changes through `sgl-3d`. A game calling
  `sgl-post-fx` directly must set each `CameraAttribs`' clip planes with
  `set_clip_planes(near, far)` (far before near for reversed-Z), as
  `sgl-3d` does: the temporal passes now read `f_near_plane_depth` and
  `f_far_plane_depth`, and left at `Default`'s 0, SSR and TAA keep no
  history at all. Afterwards, move the camera quickly backwards while
  turning over glossy floors with Crystal reflections: the next frame shows
  its own reflections, without history from elsewhere.

### Reflections take no history by a hit behind the last frame's camera

- **Scope:** `sgl-3d` world-space reflections and Velvet
  (`temporal_reprojection.wgsl`), and `sgl-post-fx` screen-space
  reflections, which `sgl-3d`'s Crystal method runs (DFX-31). Each
  reflection's temporal pass reprojects its history two ways: by the
  surface's motion, and by the reflection's virtual hit point through the
  last frame's camera. When that point was behind the last frame's camera
  (clip w below 0), as after a sharp turn or a quick move backwards, the
  division by its negative w mirrored it onto the screen, and where the
  history's depth matched there the pass blended in history from the wrong
  place, clamped to the colour box, at 95%; on the camera's plane (w = 0)
  the result was non-finite and already rejected. Such a point now
  reprojects off the screen and takes no history, as a surface behind the
  last frame's camera already took none by its motion (AMD's reflection
  denoiser and Wicked Engine's, Bevy's, Godot's and Filament's reflection
  reprojections divide unguarded; Wicked's own velocity keeps only a
  positive w). This is a correctness fix with no setting. Frames where no
  hit point lies behind the previous camera are unchanged.
- **Migration:** no game-code changes. Afterwards, turn the camera sharply
  between frames (towards a half turn), or move it quickly backwards, over
  reflective floors and water: the next frame shows its own reflections,
  without reflections blended in from elsewhere on the screen.

### Bloom accepts a scene far wider than it is high

- **Scope:** `sgl-3d`. Bloom scales the scene into a mip chain 512 texels
  high, as Bevy does. For a scene more than 32 times as wide as it is high
  on a device whose largest texture is 16,384 texels (16 times at 8,192),
  such as a window dragged to one pixel tall, that chain was wider than the
  device allows, and creating or resizing the renderer panicked in wgpu.
  Such a chain now takes the device's widest texture at the scene's aspect,
  so 64×1 renders with default settings. Every size that rendered before
  keeps its chain and its look.
- **Migration:** no game-code changes.

### A coat dims the baked diffuse light beneath it

- **Scope:** `sgl-3d` shading of coated materials (`clearcoat` above 0)
  that take baked diffuse light: a lightmap (`Scene::set_lightmap`), an
  irradiance atlas chart (`Scene::set_static_irradiance_atlas`), or a moving
  instance's ambient cube (`AmbientCube`). The coat's Fresnel toward the
  view dimmed every other light beneath the coat (direct and environment
  light, the hemisphere fill, the irradiance and dynamic GI volumes,
  emission) but not baked diffuse, so light baked into a lightmap lit a
  coated surface more than the same light live. Baked diffuse is now dimmed
  by `1 - clearcoat * F` too, as Three.js 0.185.1 and Godot dim baked light
  and Filament dims all image-based diffuse (Bevy does not). This is a
  correctness fix with no setting. At `clearcoat` 1, baked diffuse is 4%
  dimmer head-on and 20% dimmer at a view cosine of 0.3, more toward
  grazing. Camera views, probe captures and ray hits (world-space
  reflections, dynamic GI probe hits) change alike. Emission under a coat
  stays dimmed as before, as KHR_materials_clearcoat defines it. Uncoated
  materials are unchanged.
- **Migration:** no game-code changes and no re-bake. Afterwards, look at
  coated surfaces lit by lightmaps or atlas charts, and moving coated
  instances lit by their ambient cube, toward grazing angles: they are
  darker than before, as the same surfaces are under live lights.

### Scene vertices are packed into 32 bytes

- **Scope:** `sgl-3d`. The ray source, which the pulled raster passes,
  masked shadow casters, the deform stage and rays read every vertex from,
  kept each `asset::Vertex` as its 88 bytes; `PreparedModel::new` now packs
  each into 32, after Godot's attribute compression. Positions stay exact
  `f32`. The normal and tangent are one rotation, each within 0.01°; the
  tangent is made perpendicular to the normal and unit first, as shading
  already made it (the deform stage now morphs that unit tangent), and a
  mesh without authored tangents gets an arbitrary one, which nothing
  reads. UVs are 16-bit fractions of
  each mesh's UV rectangle, within the rectangle's extent over 131,070 per
  axis. Vertex colours are 8-bit sRGB with linear 8-bit alpha, clamped to
  0..1 as glTF's `COLOR_0` is (a colour above 1 or below 0 was used as
  given). Lightmap UVs are 16-bit (within 1/131,070); a negative one is
  unassigned, as before. Lightmap chart bounds are stored once per distinct
  chart in a table per mesh. On the streaming example's walk, the ray
  source holds 235 bytes a resident quad instead of 464 (25.8 MB instead of
  50.8 MB of content, a 41 MB buffer instead of 82 MB), and the scene thread
  uploads 232 KB of model words a frame instead of 449 KB. Preparing a model
  costs its workers more: they pack each vertex.
- **Refusals:** `PreparedModel::new` (and `add_asset` through it) refuses a
  vertex normal that is zero or not finite with
  `SceneError::NonFiniteGeometry`, as a non-finite position is refused, and
  a mesh whose vertices name more than 65,536 distinct `lightmap_bounds`
  with the new `SceneError::TooManyLightmapCharts { mesh }`, which names the
  mesh's index; a model's meshes together may name any number.
- **Migration:** no game-code changes for content within those limits; a
  `match` over every `SceneError` needs an arm for
  `TooManyLightmapCharts { .. }`.
  Give every vertex a nonzero normal. Scale colour through the material's
  base colour rather than vertex colours outside 0..1, and split a mesh whose
  UVs span so many repeats that 1/131,070 of their extent is visible (a mesh
  tiled 1,000 times holds its UVs to about 1/131 of a repeat). Afterwards,
  exercise the game's vertex-coloured and masked materials, lightmapped and
  atlas-lit surfaces, anisotropic materials, skinned and morphed models,
  shadows and reflections on its route.

### Models are prepared before the scene takes them

- **Scope:** `sgl-3d` adds `PreparedModel`. `Scene::add_model` and
  `Scene::set_model` take a `PreparedModel` instead of `Vec<ModelMesh>`.
  `PreparedModel::new(meshes)` does everything of building a model that
  depends only on its meshes, without a device or the scene: it validates
  them (the `IndexOutOfRange`, `NonFiniteGeometry` and `InvalidDeformation`
  errors now come from it), builds their culling hierarchies, shadow-caster
  clusters and ray-query BVH, and packs their ray-source words, deformation
  and shadow-caster geometry. The value is `Send`, so a game prepares on its
  own worker threads; SGL3D starts no thread. `add_model` and `set_model`
  then only check what needs the scene and the device (an unknown
  material, an anisotropic material without tangents, a deforming model
  with static instances, device limits), place the model's ranges and copy
  them to the queue: in the streaming example a chunk's `set_model` on the
  thread that edits the scene takes about a third of what it did. A failed
  operation places nothing and consumes the prepared model. `add_asset` is
  unchanged and prepares inside. A deforming model's rigid meshes now count
  toward the deform stage's dispatch limit too (`SceneError::DeviceLimit`
  past about 4.19 million vertices in one mesh on a typical device), since
  the stage deforms every mesh of a deforming model. With the
  `diagnostics` feature,
  `BuildStep` loses `RayWrite` and `MeshBuffers` and gains `Pack`, `Place`
  and `Write`; counters are each thread's own, so preparation's steps are
  counted on the thread that prepares.
- **Migration:** wrap the meshes given to `add_model` and `set_model` in
  `PreparedModel::new`, ideally on a worker thread for geometry the game
  makes at run time (a voxel chunk's mesh), and handle its validation
  errors there:

  ```rust
  // Before
  let model = scene.add_model(&device, &queue, meshes)?;
  scene.set_model(&device, &queue, model, remeshed)?;
  // After
  let prepared = PreparedModel::new(meshes)?; // any thread
  let model = scene.add_model(&device, &queue, prepared)?;
  scene.set_model(&device, &queue, model, PreparedModel::new(remeshed)?)?;
  ```

  Afterwards, exercise the game's streaming and editing on its route and
  compare its frame times while it streams.

### Shader loops end whatever the data

- **Scope:** `sgl-3d` and `sgl-post-fx` shaders. A loop whose count came from
  a buffer, uniform or texture could run as long as corrupt or stale data
  said, and a GPU kept busy that long hangs, freezing the machine's display
  with it. Every loop now has a named constant cap that no data can raise;
  data may end a loop earlier, never later, and a loop that reaches its cap
  fails safe. The ray source's BVH walks stop at a node whose escape does not
  lead forward, stay within the ray source, take at most four records from
  a leaf, and a ray visits at most 65,536 nodes across its instance and
  model walks, after which it reports a miss (the most measured in a
  pathological forest of 40,000 instances was 19,238). Other caps, each
  above what valid content needs: a cluster lists at most 8,192 lights and
  decals (Godot's most clustered elements), keeping live lights, then baked
  lights, then decals, in the scene's order, on the CPU and the GPU alike.
  For probe captures the cluster is the whole scene (every light that is on
  and every decal), for world-space ray hits the whole view and for dynamic
  GI probe rays the whole volume, so there it limits the scene. A probe grid
  cell names at most the
  collection's 256 probes; a frame's fog sums at most the first 1,024 fog
  volumes that reach it, over at most 512 depth slices (Godot's most);
  shadow cascades (4), exposure compensation points (8), dynamic GI rays per
  probe (256), XeGTAO slices and steps (9 and 3), the reflection probe
  collection (256), Velvet's trace steps (512, Godot's most) and world
  reflections' upsample radius (2). A mesh may have at most 256 morph
  targets, as Bevy allows: `add_model` and `add_asset` refuse more with
  `SceneError::InvalidDeformation`. In `sgl-post-fx`,
  `ScreenSpaceReflectionAttribs::max_traversal_intersections` above 256 now
  counts as 256 and `spatial_reconstruction_radius` above 8 as 8, the tops
  of DiligentFX's own ranges (PROVENANCE.md DFX-30), and the bilateral
  kernel keeps its radius of at most 2 whatever
  `bilateral_cleanup_spatial_sigma_factor` holds. Valid data renders as
  before.
- **Migration:** no game-code changes, except for a mesh with more than 256
  morph targets, which is now refused: split its targets across meshes or
  drop unused ones in the asset. A corrupt-data hang can no longer freeze
  the machine. Code
  that drives `sgl-post-fx` directly with `max_traversal_intersections`
  above 256 or `spatial_reconstruction_radius` above 8 now gets those maxima
  (SGL3D sets 64 and 4).

### Models share their shadow-caster buffers

- **Scope:** `sgl-3d` no longer creates three GPU buffers per mesh (shadow
  casters' positions, indices and caster-cluster indices). Every model's
  meshes are placed in shared slabs, as Bevy's mesh allocator packs them:
  one kind for positions and one for indices, each slab starting at 1 MiB
  and growing by half again up to 512 MiB (or the device's largest buffer),
  data of 256 MiB or more in a slab of its own, and an emptied slab
  released. Adding, replacing and removing models (`add_model`,
  `set_model`, `remove_model`, `add_asset`) creates no buffer once the slabs
  hold a stream's peak; a scene with any geometry now holds at least 2 MiB
  of slabs (a deforming model's meshes take indices only, since their
  casters read deformed positions), and up to half again what its geometry needs while a slab has
  room to fill. A mesh with no indices draws nothing (before, it issued an
  empty draw). With the `diagnostics` feature, `SceneResources` reports
  `geometry`, `geometry_live` and `geometry_buffers` instead of
  `mesh_buffers` and `mesh_buffer_count`, `Counters::buffers_created`
  counts every buffer the library creates, not only those created with
  contents, and `Counters::geometry_growths` counts slab growths.
- **Migration:** no game-code changes, except for diagnostics code that
  reads the renamed `SceneResources` fields. `geometry` is the slabs'
  capacity, which includes room they have not filled; the bytes the old
  `mesh_buffers` summed are now `geometry_live`, and `mesh_buffer_count` is
  `geometry_buffers`, the slabs:

  ```rust
  // Before
  let (bytes, buffers) = (resources.mesh_buffers, resources.mesh_buffer_count);
  // After
  let (bytes, buffers) = (resources.geometry_live, resources.geometry_buffers);
  ```

  Afterwards, exercise the game's shadows (directional and local, with
  masked materials) on its route; nothing should change.

### Local-light shadow records upload only when they change

- **Scope:** `sgl-3d`'s local-light shadow stage. Each frame wrote every
  light slot's 144 B shadow record, 18 KB at 128 slots and 147 KB at 1,024;
  it now writes only the records that changed, when a light is placed,
  re-placed, moved or left without a shadow. Over 600 frames of the
  `streaming` example's `walk`, `fly` and `torches-1024` runs that is 4 to
  37 KB instead of 11 to 88 MB. Shadows are unchanged, after abandoned
  frames as well.
- **Migration:** no game-code changes.

### A game-authored irradiance volume, relit by region

- **Scope:** `sgl-3d` adds `IrradianceVolume { origin, cell_size, cells }`,
  `IrradianceCell { irradiance: AmbientCube, sky_visibility: [f32; 6] }`
  (its `Default` is a cell never written: no light of its own, sky
  visibility 1), `PreparedIrradianceRegion::new(corner, cells, &values)`,
  `Scene::set_irradiance_volume(&device, &queue, Option<IrradianceVolume>)`,
  `Scene::irradiance_volume`, `Scene::write_irradiance_cells(&queue,
  &region)`, and `SceneError::InvalidIrradianceVolume`,
  `InvalidIrradianceRegion` and `IrradianceRegionOutside`. A scene holds at
  most one volume: a lattice of cells the game places, each an ambient cube
  of its own light (irradiance / PI) and of the sky's visibility from each
  face, a port of Bevy's irradiance volume. The game writes boxes of cells
  prepared on any thread (`PreparedIrradianceRegion` is `Send`); a write
  queues one texture write per face and is not a static edit. Installing the
  same cell size and counts at another origin scrolls the volume by whole
  cells, keeping the cells that stay, which move in place through a stripe
  16 cells thick (kept with a stripe of zeros for each axis the volume has
  scrolled along); `Scene::move_origin` translates it.
  Static surfaces without a lightmap or atlas chart and moving instances
  within it take `sky_visibility × ambient + irradiance` in place of the
  environment's diffuse light and the hemisphere fill, and of the dynamic GI
  volume and their ambient cube, which it covers; its share fades over the
  one cell past each face. Ambient occlusion occludes it as it did the
  ambient, `FrameInput::baked_lighting` turns it off, and
  `SurfaceMaterial::environment_scale` scales the ambient it lets through,
  not its own light. Its sky visibility also occludes the sky's share of
  environment specular (Lagarde's specular occlusion) on opaque, blended
  and captured surfaces and ray hits, not specular probes. Dynamic GI probe
  rays' hits take it whole. It costs 48 bytes a cell and three 3D taps a lit
  fragment; nothing is uploaded per frame. Lit group 0 binds one more
  texture, so the device floor (S3D-1) rises from 20 to 21 sampled textures
  per shader stage; no known adapter offers 20 (WebGPU in Chromium reports
  16 or 48, Metal, DX12 and Vulkan 31 or more). `graphics_device::limits`
  now also requests the adapter's `max_texture_dimension_3d`, which bounds
  the volume.
- **Migration:** none for a game without a volume: its frames are
  unchanged. An exhaustive `match` on `SceneError` adds the three arms.
  Devices requested with `graphics_device::limits` need no change; a game
  that requests its own limits requests at least 21 sampled textures per
  stage. To light a world from a field the game computes (a voxel world's
  sky and block light), install a volume over what the field covers and
  write it by region as the field changes, scrolling it with the camera
  ([irradiance volume](crates/sgl-3d/README.md#irradiance-volume);
  `examples/irradiance_volume.rs` does so for a block world's cave and
  prints what relights, scrolls and installs cost); a fixture written into
  the field is not also a baked `Light`. Afterwards,
  look at caves and overhangs beyond the shadow cascades, a torch placed
  and removed, moving objects entering and leaving lit and dark cells, the
  sky's reflection on wet or metal surfaces in caves, the volume's border
  and a scroll, and compare the `opaque geometry + lighting` (or `opaque
  lighting`) and `reflection source completion` timing groups on the game's
  route.

### Static edits redraw only the shadow faces they reach

- **Scope:** `sgl-3d` keeps each static edit's own bounds for the frame
  (adding, removing or changing a static instance, or replacing a model one
  shows), up to 1024 a frame, and the local-light shadow cache redraws a
  face's static layer only where one of them reaches it: within its light's
  range, then within the face. Before, a frame's edits merged into at most
  16 boxes, so a frame that streamed in tens of chunks redrew faces of
  lights between them that no chunk reached. Past 1024 edits in a frame,
  the bounds merge in pairs of spatial neighbours. Shadows look as before.
- **Migration:** no game-code changes. Afterwards, compare the local-light
  shadow faces and layers drawn per frame (`Renderer::local_shadow_stats`)
  and the `local shadows` and `local shadow layers` timing groups while
  streaming or editing static content near shadowed lights. With the
  `diagnostics` feature, `Counters::static_edit_boxes_merged` now counts
  the pairs merged when the pending list halves past 1024 (before, the
  boxes merged past 16).

### Shadows offset their receivers along the geometry normal

- **Scope:** `sgl-3d` shadow lookups of the directional cascades and of
  point, spot and rectangle lights offset the receiver along its geometry
  normal (the interpolated vertex normal toward the side shaded) instead of
  the normal its normal map, bump map, decals or scrolling normal layers
  make, as Bevy and Filament do. This is a correctness fix with no setting.
  On normal-mapped surfaces, shadow edges no longer shift texel by texel
  with the map, and on surfaces with scrolling normal layers, such as water,
  they no longer crawl from frame to frame. Surfaces without a normal or
  bump map, normal-mapped decals or normal layers are unchanged, as are
  probe captures and ray hits of such surfaces and the fog, which takes no
  offset.
- **Migration:** no game-code changes. Afterwards, look at shadow edges and
  contact shadows on normal-mapped and bump-mapped surfaces, and on water
  with normal layers, at grazing light: acne or peter-panning there may
  differ from before.
### Crystal's denoiser skips tiles where every ray missed

- **Scope:** `sgl-post-fx` screen-space reflections, and so `sgl-3d`'s
  Crystal method at `Full` and `Half`. Spatial reconstruction, temporal
  accumulation and bilateral cleanup now run only on 8×8 tiles with a
  confident hit in or beside them, as AMD's SSSR denoiser runs only over
  its tile list; elsewhere their result was already zero. On an Apple M5
  at 1920×1080 the three passes take about half the time (a lake 1.22 →
  0.60 ms, a scene of mixed ray lengths 1.62 → 0.66 ms). The tiles' reach
  follows `spatial_reconstruction_radius` and
  `bilateral_cleanup_spatial_sigma_factor`, so reflections are unchanged at
  any settings. A skipped tile's history now holds zero radiance and, as
  where a pixel has no history, variance 1: a reflection returning there is
  blurred over 3×3 pixels by bilateral cleanup for about 65 frames (about
  1 s at 60 fps) where its perceptual roughness is 1/16 or more, rather
  than from the variance its misses left. The `SSR spatial reconstruction`
  timing group includes the two small passes that find the tiles.
- **Migration:** no game-code changes. Afterwards, compare the `SSR`
  timing groups on the game's route.

### Materials scroll their normal maps; frame time is `f64`

- **Scope:** `sgl-3d` adds `NormalLayer` and the field `normal_layers:
  Option<[NormalLayer; 2]>` on `asset::Material` and `SurfaceMaterial`
  (`None` by default and from the glTF loader), and
  `SceneError::InvalidNormalLayers`. With layers, the material's normal map
  is drawn twice, each layer at its `scale`, moving across the surface at its
  `velocity` (material UV units per second) with `FrameInput::elapsed_seconds`,
  their slopes added at their `strength`: water's waves with no geometry
  uploaded per frame, seen alike by the G-buffer, the receiver pass, blended
  surfaces, probe captures and world-space ray hits. Speeds are rounded to
  whole repeats of the map per hour (at most 1/7200 of a repeat per second
  off; a layer slower than that stands still). A material with layers needs
  a normal map that repeats on both axes, finite velocities and strengths,
  positive finite scales and at most 2^24 repeats of the map per hour;
  `add_materials`, `add_asset` and `set_material` refuse others. FSR2 takes
  a blended surface's moving layers from the masks blended surfaces write;
  an opaque material's layers write none.
  `FrameInput::elapsed_seconds` is now `f64` (was `f32`): SGL3D reduces it
  modulo an hour on the CPU, so the layers keep their precision however long
  a session runs. The mist drifts as before. The `water` example's lake is
  now one static quad whose material scrolls a wave map; its `set-model` run
  keeps the per-frame `Scene::set_model` waves for comparison. Materials
  without layers render as before.
- **Migration:** assign `elapsed_seconds` an `f64`, ideally straight from
  the game's clock rather than through an `f32`:

  ```rust
  // Before
  input.elapsed_seconds = start.elapsed().as_secs_f32();
  // After
  input.elapsed_seconds = start.elapsed().as_secs_f64();
  ```

  `asset::Material` and `SurfaceMaterial` struct literals that list every
  field add `normal_layers: None`; those built with `..Default::default()`,
  or from `Scene::material`, need nothing. A game that matches `SceneError`
  exhaustively adds the `InvalidNormalLayers` arm. To move water's waves in
  its material, give it a repeating normal map and two layers, and drop the
  per-frame `Scene::set_model` (the receiver can then be a static instance):

  ```rust
  let water = Material {
      normal_texture: Some(waves),
      normal_layers: Some([
          NormalLayer { velocity: [0.06, 0.025], scale: 1., strength: 1. },
          NormalLayer { velocity: [-0.04, 0.07], scale: 2.7, strength: 0.6 },
      ]),
      ..water
  };
  ```

  Afterwards, look at the water in motion with TAA, FSR2 and screen-space
  reflections, and compare the `receivers`, `blended` and `SSR *` or
  `Godot SSR *` timing groups on the game's route.

### Dynamic diffuse GI from a volume of probes

- **Scope:** `sgl-3d` adds `DynamicGiVolume { origin, spacing, probes }`,
  `Scene::set_dynamic_gi_volume` and `Scene::dynamic_gi_volume`,
  `SceneError::InvalidDynamicGiVolume`, and `settings::DynamicGiQuality`
  (`Off`, `Low`, `High`) as `Settings::dynamic_gi`, `High` by default. A
  scene holds at most one volume, a lattice of probes the game places; a
  new stage, first after prepare, keeps the probes up every frame with rays
  through the scene's ray source, a port of Wicked Engine's DDGI: coloured
  bounce light from the frame's directional lights, the scene lights whose
  range reaches the volume (each hit's light, where it casts a shadow,
  shadowed by a ray at its shadow opacity, never a shadow map; a light
  without a shadow lights the probes unoccluded, as it lights surfaces),
  emitters, the sky and further bounces. Probe rays meet single-sided
  surfaces from either side: one met from behind brings no light and
  counts as occluding, so probes inside closed geometry or beyond walls
  keep what lies behind a surface from receivers on its other side.
  `Scene::move_origin` translates the volume and keeps its probes.
  Within the volume (fading out over one spacing past it) static surfaces
  without a lightmap or atlas chart and moving instances take its
  irradiance in place of the environment's diffuse light and the hemisphere
  fill, and moving instances in place of their ambient cube; ambient
  occlusion occludes it as it did those. Lightmapped and charted surfaces
  keep their bake. `SurfaceMaterial::environment_scale` does not scale it.
  A receiver weighs the probes about it with RTXGI's wrap-shading weight and
  tests their visibility from a point offset toward the viewer (Majercik et
  al. 2021's self-shadow bias, about a quarter of the least spacing), where
  Wicked lets a surface facing a nearby wall take the light beyond it; a
  change in the bounce about a small object now settles over tens of frames
  rather than a few. A probe more than a quarter of whose fixed rays meet
  single-sided surfaces from behind (inside geometry, beyond a wall) is
  inactive, as RTXGI classifies its probes: it lights nothing and traces the
  fewest rays, and every probe traces 4 fixed rays a frame beside its
  others. A probe with no surface within a spacing of it is dormant: it
  lights moving instances alone (static surfaces skip it, so none takes
  light from beyond a room's corner) and traces the fewest rays unless a
  moving instance's bounds come within that spacing; a static object so
  small that no probe's fixed rays find it takes its other indirect light,
  and a probe's class follows a change within 8 frames. The probes' own rays
  take the volume's light at what they hit, never the environment's
  fallback, so a closed room starts dark rather than holding the sky for
  seconds. Once its light has converged (RTXGI's probe variability stops
  falling), the volume pauses until something its light follows changes: a
  scene edit that changes what its rays see or light, the frame's
  directional lights, hemisphere fill or environment, the quality or the
  placement; a converged static scene's dynamic GI then costs next to
  nothing. Materials that scroll their normal maps keep it running. Installing the volume again with its origin moved by whole
  spacings scrolls it: the probes that stay keep their light, and those that
  enter start afresh; an origin off the lattice, or another spacing or
  count, is another placement. A restart (another placement or
  scene, or a frame without the volume), or a scroll's entering planes,
  starts at most 128 probes a frame at High (256 at Low), nearest the camera
  first, where Wicked starts every probe in one frame; the surfaces about a
  probe not yet started keep their other indirect light. The `dynamic_gi`
  example lights a room, scrolls a volume after its camera (`--scroll`) and
  prints the stage's cost. Timing groups `dynamic GI allocation`,
  `dynamic GI rays` and `dynamic GI blend` report its cost, and frames that
  run it rebuild the ray source's instance BVHs, as world-space reflections
  do. A volume costs about 11 KB of GPU memory a probe at High (8 KB at
  Low). Lit group 0 binds one more texture, so the device floor (S3D-1)
  rises from 19 to 20 sampled textures per shader stage; no known adapter
  offers 19 (WebGPU in Chromium reports 16 or 48, Metal, DX12 and Vulkan 31
  or more).
- **Migration:** none for a game without a volume: nothing runs, and its
  frames are unchanged. A `Settings` literal that lists every field adds
  `dynamic_gi: DynamicGiQuality::High` (saved settings without the field
  load with it); an exhaustive `match` on `SceneError` adds
  `InvalidDynamicGiVolume`. Devices requested with `graphics_device::limits`
  need no change. To light a level, install a volume that covers the
  surfaces it should light, its probes one to a few metres apart and off
  the surfaces themselves, give lights that should stay in their rooms a
  shadow, and offer `Settings::dynamic_gi` to players
  ([dynamic GI](crates/sgl-3d/README.md#dynamic-diffuse-gi)). Afterwards,
  look at rooms and their corners
  lit through openings and by lamps, moving objects passing through the
  volume and leaving it, and the first second after loading a level or
  moving the volume; compare the `dynamic GI *` timing groups on the
  game's route at High and Low.

### A streamed block world example, upload and resource counters, and a cheaper origin move with many lights

- **Scope:** `sgl-3d` adds the `streaming` example
  (`cargo run --release -p sgl-3d --example streaming`), a block world
  streamed in 16 m chunks about a moving camera at a block game's scale,
  edited, remeshed and moved with `Scene::move_origin`. It prints the CPU
  time of each scene operation and of a frame's scene calls (apart from the
  game's meshing), what the library uploaded and built, the scene's buffer
  sizes and each view's draws, and writes each run's last frame to
  `target/streaming-example/`; `--check` verifies that a moved origin shows
  no motion and redraws no shadow, and that remeshed chunks redraw their
  torches' static shadow layers once, across an abandoned frame. The
  `diagnostics` feature adds `diagnostics::counters()`, the thread's
  `Counters` (uploads by call site as `UploadSite`s, buffers created with
  contents, `StepTime`s of each `BuildStep` of building a model and the
  instance BVHs, ray-source growths, static-edit boxes recorded and merged),
  with `Counters::since` for what happened between two of them;
  `Scene::diagnostic_resources()`, its buffer sizes as `SceneResources`; and
  `Renderer::diagnostic_draws()`, the last frame's draws per view as
  `ViewDraws`. Without the feature nothing is counted. `Scene::move_origin`
  now rewrites the scene's light records in one write rather than one a
  light, which with about a thousand lights took the lights' share of a move
  from milliseconds to tens of microseconds; the whole move then took
  0.09-0.24 ms with up to 512 instances in the example.
- **Migration:** no game-code changes.

### The scene's render origin moves without a cut

- **Scope:** `sgl-3d` adds `Scene::move_origin(&device, &queue, to: Vec3)`
  and `SceneError::InvalidOrigin`. Positions stay `f32` in the scene's
  render frame; a game whose world is larger than `f32` renders precisely
  keeps its own coordinates and moves the render origin to `to` (in the
  current render frame) to stay near what it renders. Every position the
  scene holds becomes what it was less `to`: instances and the poses their
  motion is measured from, object records, the ray source, lights, decals,
  fog volumes, mist, glow and heat geometry, installed specular probes and
  their grid, and pending static-edit bounds. It is not a static edit:
  moving instances keep their motion, static shadow layers stay valid,
  every history continues (the renderer translates its camera history and
  the local-light shadow cache what it keeps), and the directional cascades
  snap their texel grid about the frame the scene was created in, so a move
  shifts no shadow texel. Internally the renderer's camera history now keeps
  the previous view, projection and jitter, and Velvet, world-space
  reflections and the DiligentFX context take their previous camera from it
  instead of keeping their own; their output is unchanged.
- **Migration:** no game-code changes for a game that never moves its
  origin, and its frames render as before. A game that matches
  `SceneError` exhaustively adds the `InvalidOrigin` arm. To use it: after
  `move_origin`, give the camera (`FrameInput::camera`) and every position
  the game edits afterwards (instance poses, lights, decals, transient
  geometry) in the new frame, less `to`; the scene has translated what it
  already holds. Exercise a moving camera across a move with TAA, SSR and
  shadows on: nothing should jump, smear or redraw.

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

  Mark water and glass that should reflect the scene with `true`, and
  animate their normals with material normal layers (the entry above) or in
  a mesh on a moving instance (see the package README's
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
