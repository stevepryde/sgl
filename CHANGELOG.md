# Changes and migrations

Migration guidance for agents maintaining games with SGL. Read all entries
after the game's current version through its target version, in version order.
`Unreleased` describes changes on Git that are not yet in a released version.
See the [update workflow](docs/README.md#updating-a-game) for dependency requirements
and validation in the consuming game.

One bullet per change: the crate and symbol, the old and new behaviour in a
clause, and the migration in a line or a short code sample. Record changes
that still compile but alter what a game sees (defaults, settings, units,
platforms, asset and bake formats), and say when no game-code change is
needed. Rationale, measurements and full API details belong in the package
docs and specs the entry links.

## Unreleased

- `sgl-core` `prediction::InputQueue` adds `first_queued_tick` and `last_queued_tick`, the earliest and latest ticks with a queued input, and `skip_to(tick)`, which moves the next tick forward, discarding the inputs it passes without handing them out or making them the repeated input, where a server could only `take` them; `push` and `take` are unchanged. No game-code changes are needed.
- `sgl-core` `prediction::SnapshotBuffer` adds `insert_recent(tick, snapshot) -> Inserted` (keeps the newest `capacity` ticks in tick order: a full buffer drops its oldest for a newer tick, a late snapshot takes its place, one at a held tick replaces it), `newest() -> Option<(u64, &S)>`, and `sample_with(&self, now_tick, alpha, interpolate, extrapolate)`, which samples without changing the buffer and, past the newest snapshot, calls `extrapolate(newest, ticks)` on that snapshot alone, returning `Sampled::Extrapolated` within `max_extrapolation_ticks` and `Sampled::Held` beyond it ([guide](crates/sgl-core/README.md)). `push` and `sample` behave as before. No game-code changes are needed.
- `sgl-3d`: MikkTSpace tangent generation (`asset::TangentFrames::Generate`) is now the default-on `mikktspace` feature, and `bevy_mikktspace` an optional dependency; no game-code change is needed with default features. A game that exports its tangents or builds its meshes in code can set `default-features = false` (keeping any other features it names, such as `diagnostics`): a primitive that needs a frame and carries no `TANGENT` then keeps the frame of each triangle's coordinates and is listed as `asset::Ignored::TangentFrames`, and an anisotropic one fails the load. Regenerate the game's distribution notices after dropping it.

## 0.11.0 — 2026-11-25

Move every SGL crate to `0.11.0` together and commit the updated `Cargo.lock`.
Game code must change only where a game used `sgl_core::collision`, which moves
to the new `sgl-physics` crate (the first entry gives the migration).
Everything else is an addition. A game that adds `sgl-physics` regenerates its
distribution notices from its lockfile: they gain Box2D's and Box3D's MIT
notices.

- `sgl-core` `collision` moves to the new `sgl-physics` crate, for 2D and 3D ([guide](crates/sgl-physics/README.md)): a body is a `Shape::Box { half }` at a position instead of an `Aabb`; `ColliderSet::insert` takes a `Collider` carrying the game's `id` and `layers` (inserting an id again replaces it; `query` and `get` are gone); `ColliderFlags` becomes `ColliderKind::{Solid, OneWay, Sensor}`; `CollisionConfig` is `MoverConfig`, built from a `MoverSettings` of named fields; `move_and_collide`, which slides, is renamed `slide`, and it and `snap_to_ground` take a layer mask and return a `Result` (only a world with a visit cap answers `Unanswered`); `sweep_aabb` becomes `cast`. `slide` now keeps all motion across `up` after a floor contact and all motion along a wall after one, never slides back into a face met in the same move, and resolves contacts at one instant to the lowest id, then position, not insertion order; `snap_to_ground` no longer reaches a floor through a steeper face; a non-finite position, body or result moves nothing. Add `sgl-physics` at the same version, then `set.insert(Collider::new(id, Shape::Box { half }, center).with_kind(kind))`, `MoverConfig::new(MoverSettings { up, skin, snap_distance, normal_threshold, block_epsilon })` and `slide(&Shape::Box { half: body.half }, body.center, delta, &set, ALL_LAYERS, &config)?`. `sgl_core::math` also re-exports `IVec3` and `Quat`.
- `sgl-physics` (new crate) adds queries over any world: `cast`, `ray`, `overlaps`, `distance` and `move_and_collide` (stops at the first contact, without sliding), with worlds `ColliderSet`, a slice, `CellWorld` (a tile or voxel grid the game answers cell by cell) and pairs of worlds. No game-code changes are needed.
- `sgl-physics` adds the mover ([guide](crates/sgl-physics/README.md#the-mover)): `move_and_slide` steps a `MoverState { position, velocity, floor }` with `MoverOptions` (step-up, ledge guard, one-way drop-through, push-out), and `find_ledge` finds a ledge to climb. No game-code changes are needed.
- `sgl-physics` adds ramps: `Collider::cut` (`with_cut(Cut::new(normal, offset))`; in 3D the normal needs a zero component), walked at flat-ground speed and met by every query as the shape it is. No game-code changes are needed.
- `sgl-physics` adds round bodies and convex colliders: `Shape::Ball { radius }`, `Shape::Capsule { half_axis, radius }` and `Shape::Convex` (`Convex::polygon(points, cos, sin)` in 2D, `Convex::hull(points, quat)` in 3D) are bodies and colliders for every query and the mover, through a convex kernel ported from Box2D and Box3D. No game-code changes are needed.
- `sgl-physics` adds moving colliders ([guide](crates/sgl-physics/README.md#moving-colliders)): `Collider::velocity` and `Collider::push` (`Push::Rigid` or `Push::Soft(limit)`), so `move_and_slide` carries a body standing on a moving collider, pushes out one a collider moved into or reports it `crushed`, and nudges a body off soft colliders; `ColliderSet` gains `set_position`, `set_velocity` and `remove` by id; `sensors_at` and `sensor_events` report the triggers a body entered and left. No game-code changes are needed.
- `sgl-physics` adds `planar` for vehicles bumping in a plane: `time_of_impact` for bodies that move and turn, `manifold`, `bump`, and `advance`, which steps a slice of `planar::Body` through its impacts in time order and returns them. No game-code changes are needed.
- `sgl-physics` adds 3D surfaces for sight and ground ([guide](crates/sgl-physics/README.md#surfaces)): `TriangleSet::new(positions, indices, ids)` and `HeightField::new(heights, columns, rows, cell, origin, ids)` are worlds every query accepts, and `ground_under(point, max_height, depth, &world, mask)` returns the highest `Ground { height, normal, id }` straight down. Nothing walks on them. No game-code changes are needed.
- `sgl-core` adds `prediction` (new): `Predictor` (input history, reconcile and replay through the game's step function), `InputQueue` (a server's per-client inputs by tick), `Correction` (a fading visual offset), `SnapshotBuffer` (snapshot interpolation with bounded extrapolation) and `Sample` (previous and current state for drawing between steps). No game-code changes are needed; a game may replace its own prediction bookkeeping with it ([guide](crates/sgl-core/README.md)).
- `sgl-2d` adds `canvas::Renderer::render_screen`, which draws a list's screen channel over the picture a game rendered into `scene_view` (an SGL3D view, its own passes), and `Renderer::blit_scene`, which letterboxes that canvas into a swapchain frame, where a HUD over SGL3D needed a game-owned `SpritePass` ([guide](crates/sgl-2d/README.md#ui-and-huds)). No game-code changes are needed.
- `sgl-2d` adds `canvas::Context::try_new_with` / `try_new_with_async` and `canvas::Gpu::headless_with` / `headless_with_async`, which take `impl FnOnce(&wgpu::Adapter) -> wgpu::DeviceDescriptor<'static>` and request the device it returns, where the canvas could only request a default-limits device, so one device can serve the canvas and SGL3D; a refused request is `RendererInitError::Device` / `GpuError::NoDevice`. No game-code changes are needed.

## 0.10.0 — 2026-11-24

Move every SGL crate to `0.10.0` together and commit the updated `Cargo.lock`.
Game code, data and settings must change; the first four entries give the
largest migrations. Normal maps are read green up (glTF's convention) at full
strength: check every map with the `normal_convention` example, invert a
DirectX map's green at export, and retune `normal_scale` and
`coat_normal_scale`. `asset::Material`'s maps become `Option<MaterialTexture>`,
each with its own sampler, UV set and transform. `ViewFrame::output` becomes a
`ViewOutput`. A browser build creates its wgpu instance through the new
`sgl-webgpu` crate. `Renderer::compile` (new) readies a frame's shaders and
pipelines behind a loading screen. Regenerate the game's distribution notices,
and rebake probe captures, lightmaps and reference images that show
normal-mapped surfaces if they should match.

- `sgl-3d` `asset::Material::normal_texture` and `coat_normal_texture` on every material without authored tangents (all but anisotropic ones) were read green down (toward increasing V: DirectX's convention, glTF's opposite), flattened many times over in raster at real resolutions, and flattened or turned on a stretched or sheared unwrap; they are now read green up (glTF's convention) at full strength, in raster and at ray hits, on their meshes' `Vertex::tangent` wherever every mesh drawing the material has one and on glTF's unit frame otherwise, and the glTF loader gives MikkTSpace tangents to a primitive without `TANGENT` whose material has a normal map, anisotropy or a clearcoat normal map (`asset::LoadOptions::tangents`: `asset::TangentFrames::Generate` by default, or `AuthoredOnly`; vertices split at mirrored seams; an anisotropic one loads instead of failing). Every normal-mapped surface changes: relief is far stronger, and a DirectX map, which the old reading suited, now lights inside out. Check that each normal map is green up (the `normal_convention` example tells; [guide](crates/sgl-3d/docs/README.md#steering)) and invert a DirectX one's green at export, including procedural maps the game writes; retune `normal_scale` and `coat_normal_scale` values chosen against the flattened relief; rebake probe captures, lightmaps and reference images that show normal-mapped surfaces if they should match. Code that builds `LoadOptions` field by field adds `tangents`; code-built meshes zero `Vertex::tangent` unless it holds the normal map's frame (a placeholder now shades their normal maps wrong), and their levels of detail carry frames where the base mesh's material has them (`SceneError::MissingAnisotropyTangents` otherwise). A primitive in which more than 1 024 triangles share one vertex is not framed and is listed as the new `asset::Ignored::TangentFrames` (exhaustive matches add an arm). Regenerate the game's distribution notices: `bevy_mikktspace` adds Zlib, MIT and Apache-2.0 terms.
- `sgl-3d` `asset::Material`'s eighteen map fields (`base_texture`, `mr_texture`, …) change from an image index to `Option<asset::MaterialTexture>`: an image with its own `asset::Sampler` (filters and wrapping), `uv_set` (`asset::UvSet::Uv0` or `Uv1`) and `transform` (`asset::UvTransform`, `KHR_texture_transform`'s offset, rotation and scale); `Material::wrap` is removed, and `asset::Vertex` gains `uv1`, the second UV set (96 bytes, which the decoded load budget charges a vertex). The glTF loader keeps each texture's sampler, `texCoord` and `KHR_texture_transform` (so quantised UVs sample at their authored range) instead of refusing nearest, non-trilinear or mixed sampling and maps off `TEXCOORD_0` or leaving the extension out, and fills `uv1` from `TEXCOORD_1` only for meshes whose material samples it; `TEXCOORD_1` still loads as `lightmap_uv`, which alone selects the lightmap chart. `Material` also gains `specular_texture` and `specular_color_texture` (`KHR_materials_specular`'s strength in alpha, colour in sRGB RGB), and an `occlusion_texture` other than the metallic-roughness map now shades: both were left out (`Ignored::SpecularMap` is removed; `Ignored::OcclusionMap` now names only an occlusion map on `TEXCOORD_2` or above). They load as any map: the UV set they name is required, their images are decoded, and a specular map on `TEXCOORD_2` or above fails the load; the `Basic` tier keeps their factors. Write `Some(MaterialTexture::new(index))` for `Some(index)` (other fields with `..MaterialTexture::new(image)`), set each map's `sampler.wrap` where `wrap` was set, add `uv1: [0.; 2]` to `Vertex` literals and `specular_texture: None, specular_color_texture: None` to `Material` literals without `..Default::default()`, and drop `Ignored::SpecularMap` arms ([guide](crates/sgl-3d/README.md#occlusion-and-specular-maps)).
- `sgl-3d` `ViewFrame::output` is now a `ViewOutput`: write `output: ViewOutput::Texture(&view)` where a frame passed `output: &view`. `ViewOutput::RenderTexture` draws a view into a scene render texture (`Scene::add_render_texture`) that a material shows as its emission map (`SurfaceMaterial::emission_render_texture`, `None` by default; add it to any `SurfaceMaterial` built field by field): an in-world monitor ([guide](crates/sgl-3d/README.md#render-textures)). Exhaustive matches on `SceneError` add `UnknownRenderTexture` and `RenderTextureInUse`.
- `sgl-webgpu` (new) is SGL's WebGPU backend for wgpu, ported from wgpu's own; SGL's browser builds now run on it, and `sgl-2d`, `sgl-3d` and `sgl-post-fx` no longer enable wgpu's `webgpu` feature. Beyond wgpu's backend, its adapters and devices report `Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` where the browser has `texture-formats-tier2`, `Adapter::get_texture_format_features` reports the storage, render and multisample capabilities the browser's two format tiers add, and `create_render_pipeline_async` and `create_compute_pipeline_async` create a pipeline with the browser's asynchronous call and resolve to it, or to its validation or internal error. In the browser, add `sgl-webgpu` as a `wasm32` dependency and replace `wgpu::Instance::default()` with `sgl_webgpu::instance(&wgpu::InstanceDescriptor::new_without_display_handle())`; everything after is wgpu's API but for the differences its README lists (browser handles come from `sgl_webgpu::device_as_webgpu` and the like, not `as_webgpu`), and `sgl-2d`'s `Context` and `Gpu::headless_async` already do so. Native needs no change. The port carries wgpu's and wasm-bindgen's MIT notices in its `THIRD_PARTY_NOTICES.txt`.
- `sgl-3d` adds `Renderer::compile`, which readies a frame's shaders and pipelines before it, behind a loading screen: every one as a `CompileJob` for the game's threads (`CompileJob::label` names its stage), none created by the call, by the frame it readied or by the first refresh of each of the scene's live probes; FSR's upscaler still creates its own in the `View::new` or `View::resize` that builds its context and in the first frame after. In the browser, `CompileJob::run` starts WebGPU's asynchronous pipeline creation and returns, so the page does not stall while the browser compiles, and `Compilation::errors` is filled there too; the browser's device must come from `sgl_webgpu::instance`. `Renderer::new`, `View::new` and `View::resize` no longer create shaders or pipelines (FSR apart): the first frame that uses a stage does. No game-code changes are needed; to use it, call it each loading frame with `render`'s arguments, run `compilation.jobs` (all at once, or a handful a frame for a smoother loading screen), and keep presenting and calling `compile` until `compilation.ready()`, with no wait for the queue between batches ([guide](crates/sgl-3d/docs/README.md#compiling-ahead)).
- `sgl-2d` `SpriteInstance::normal`: a flipped (`flip_x`, `flip_y` or a negative `scale`) or rotated sprite's normal map lit it as if it were upright and unflipped, so a sprite flipped to face the other way was lit from the wrong side; the map's X and Y now turn with the sprite on the screen, under either camera convention, so such sprites look different. No game-code changes are needed.
- `sgl-3d` `Settings::soft_shadows`: a surface nearly edge on to a light (a hanging curtain under a high sun) followed its plane without bound and took up to half the light through an occluder above it, which normal maps showed as bright speckles; the plane is now followed no steeper than 10 m toward the light per metre across, so such surfaces keep the occluder's shadow; a surface the light grazes within about 6° of edge on shades some of itself again, so open ground under a sun lower than 6° is darker, slightly with the default 0.53° disc and by up to about three quarters with a 6–10° one. No game-code changes are needed.
- `sgl-3d` TAA and FSR: a scene holding a material whose shading may change (a game shader, moving normal layers, a render-texture screen), a blended material or a sprite batch made every TAA frame bind the temporal masks and run the masked resolve, and every TAA and FSR frame with such a material run the temporal composition pass; now only frames in which something marks them in that view do, and the two mask targets, once made, are kept while TAA or FSR runs. No game-code changes are needed.
- `sgl-3d` and `sgl-post-fx`, built without debug assertions, embed their WGSL without comments, indentation or trailing whitespace, every line kept so naga reports the same lines. No game-code changes are needed; build a browser game with the `web` profile in [Distributing a game](crates/sgl-3d/docs/README.md#distributing-a-game).
- `sgl-post-fx` `PostFXContext::new` uploads its blue-noise tables from stored bytes instead of parsing DiligentFX's C++ text. No game-code changes are needed.
- `sgl-3d`'s licence notices reproduce the 2008 INRIA notice of the Bruneton sky functions, which only the embedded shader carried. Regenerate the game's distribution notices.
- `sgl-3d` `Settings::screen_space_indirect`: each slice's samples now reach the full `screen_space_indirect_radius` (they stopped a step short), which slightly changes the bounce at both levels; turning it off releases its pipelines and bind groups as well as its targets, and `Renderer::compile` after turning it back on readies them again. No game-code changes are needed; a game that compiles ahead calls `compile` after turning it on, as after any settings change.
- `sgl-net` now exposes `udp` and `websocket` as independent, default-on Cargo features. Games using `default-features = false` must enable the transports they use. No game-code changes are needed.
- `sgl-2d` builds `fetch_runtime_bundle` with the narrower futures utility dependency and `TextRenderer` with fontdue’s standard-library maps. No game-code changes are needed.
- `sgl-3d` dynamic GI (`Settings::dynamic_gi`): two renderers given the same scene, settings and frames lit the volume's probes, and so drew their frames, slightly differently, varying from run to run; the probes now hold the same light on every run, so frames with dynamic GI on are reproducible on the same device. No game-code changes are needed.
- `sgl-3d` TAA and FSR: adding, removing, showing, hiding or swapping the model of an instance, moving a static one, replacing a model's geometry or a `Scene::set_material` edit that changes what is drawn re-armed TAA across the whole view for twelve frames (FSR saw nothing); the frame after now marks only where the instance stood and stands, so the rest of the view keeps its history and FSR rejects the instance's old pixels too. Past 1024 marked meshes (or 2²¹ of their triangles) pending before a frame, or for an instance drawn with a shader that displaces its vertices and reads no time, TAA still re-arms the view. No game-code changes are needed.
- `sgl-3d` adds, in the browser, `asset::load_slice_in_browser(&bytes, options).await` and `asset::decode_image_in_browser(&bytes).await`, which have the browser decode PNG and JPEG images off the page's thread into the new `Image::Browser` (copied into its texture, and the ray source, on the GPU); before, the only browser loader, `load_slice`, decoded them in WASM on the page's thread, which it still does. Replace `asset::load_slice_with_options(&bytes, options)?` with `asset::load_slice_in_browser(&bytes, options).await?`; decode decal and cookie images yourself (`Image::Rgba8`), since `add_decal_image` refuses a browser image with `SceneError::MissingDecodedImage`. Native is unchanged; on wasm32 an exhaustive `match` on `asset::Image` adds `Image::Browser`. `diagnostics::read_map_async` (new) reads a material map's texels back from its texture and the ray source.
- `sgl-3d` `View`, `Scene` and `Renderer` keep their state on the heap, so holding, returning or moving one no longer fills the browser's debug stack, and SMAA's lookup tables are decoded and uploaded once per renderer, the first frame a view may run SMAA, instead of in every view's creation; `ViewError::LookupTextures` is removed, as a view no longer decodes them. A game that boxed these values to spare the stack may stop; remove any `ViewError::LookupTextures` match arm. Nothing else changes.
- `sgl-3d` adds outlines of selected instances: `Scene::add_selection(&[InstanceId])` (`set_selection`, `remove_selection`) and `FrameInput::outline` (`Outline`: the selection, a width in output pixels and sRGB colours in sight and hidden; `None` by default) draw a line around them over the view's finished picture, before the HUD ([guide](crates/sgl-3d/README.md#outlines)). No game-code changes are needed; code that builds a `FrameInput` field by field adds `outline: None`, and exhaustive matches on `SceneError` add `UnknownSelection`.
- `sgl-3d` `Scene::add_materials` and `add_sprite_image` upload a decoded image's (`asset::Image::Rgba8`) first level and return, and the GPU builds its mip chain before the next frame or probe capture draws; before, the scene built it on the CPU, blocking the caller. `Rgba8MipChain::new` builds the same chains on the CPU. Minified textures look slightly sharper: levels are the area-weighted box mean of the level above in place of a wider tent filter. No game-code changes are needed; a game that prepared `Rgba8MipChain`s only to keep the page responsive can pass `Image::Rgba8` instead.
- `sgl-3d` adds world-space sprites: `Scene::add_sprite_image`, `add_sprite_batch` (`sprite::SpriteBatch`: facing, masked or blended, sizing), `set_sprites` (`sprite::Sprite`: pivot, size, rotation, atlas `SpriteRegion`, tint) and their `set_`/`remove_` operations draw each batch as one instanced draw facing every view; mist now faces the view's own axes, unskewed by TAA's jitter, and is ordered along its view axis. No game-code changes are needed; an exhaustive `SceneError` match adds `UnknownSpriteImage`, `UnknownSpriteBatch`, `SpriteImageInUse` and `InvalidSprite` ([guide](crates/sgl-3d/README.md#world-space-sprites)).
- `sgl-3d` `asset::Ignored` gains `MorphTargetAttribute { mesh, primitive, target, name }`: a morph target's `TEXCOORD_n`, `COLOR_n` or other displacement besides `POSITION`, `NORMAL` and `TANGENT` was dropped silently and is now listed in `Asset::ignored` (still not applied). Add an arm for it to any exhaustive `match` on `Ignored`; nothing else changes.
- `sgl-3d` `deformation::Rig::joint_matrices(&locals)` returned a new `Vec<Mat4>` every call and panicked on local matrices fewer than the rig's nodes; it now composes into a reused `deformation::JointMatrices` and returns `Result<(), deformation::RigError>`, refusing local matrices that are not one per node and joints or their ancestors' parents naming a missing node. Keep a `let mut joints = JointMatrices::new();` beside the pose, then `rig.joint_matrices(&locals, &mut joints)?;` and pass `joints.matrices()` where the returned vector went.
- `sgl-3d` `asset::load` and `load_slice` load `EXT_mesh_gpu_instancing` nodes into the new `Asset::instanced` (`InstancedMesh`: one mesh, once, with each node's instance `transforms`), which `Scene::add_asset` prepares as a model each in the new `AssetIds::instanced`; before, a file requiring the extension was refused and one using it drew each instanced node once. Loading installs nothing: add an instance of `ids.instanced[i]` at `asset_pose * transform` for each placement wanted ([guide](crates/sgl-3d/README.md#instanced-gltf-nodes)). Such a file no longer lists `Ignored::Extension("EXT_mesh_gpu_instancing")`; exhaustive matches on `asset::Ignored` add `Instancing` and `InstanceAttribute`. Code that builds `Asset { .. }` adds `instanced: Vec::new()`, and code that builds `AssetIds` adds `instanced`.
- `sgl-3d` `asset::load` and `load_slice` load glTF triangle strips and fans as triangle lists, `KHR_mesh_quantization` files (integer positions, normals, tangents, UVs and morph displacements) and `EXT_meshopt_compression` and `KHR_meshopt_compression` files (gltfpack `-c`/`-cc`, decoding compressed buffer views at import without reading fallback buffers); before, they refused all three. An unnormalized integer `TEXCOORD_0`/`TEXCOORD_1` now loads as its value, not normalized. The Meshopt port carries meshoptimizer's MIT notice in `THIRD_PARTY_NOTICES.txt`. No game-code changes are needed.
- `sgl-3d` `Antialiasing::Fsr2` and `Fsr3` run in the browser, and on any adapter without the native FSR features, where it grants `float32-filterable` and six storage textures a stage; before, they resolved to TAA there. `graphics_device::fsr_features` requests that feature, with `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` where the adapter has it, when the adapter lacks the native set. Update `sp-fidelity` and `sp-fidelity-wgpu` with SGL. No game-code changes are needed.
- `sgl-3d` `animation::Pose` (new) samples imported clips as glTF interpolates them (`Pose::sample` with `Wrap::Clamp` or `Wrap::Loop`, `Clip::duration`), crossfades poses (`Pose::blend`) and layers additive ones (`Pose::add`); `deformation::Node`'s rest transform moves from `translation`/`rotation`/`scale` into `rest: Transform`, and `Node::rest()` becomes `node.rest.matrix()`. Replace a hand-written sampler with `pose.sample(&rig, clip, time, Wrap::Loop); pose.local_matrices(&mut locals);` then `rig.joint_matrices(&locals, &mut joints)?` and `&pose.morph_weights`.
- `sgl-3d` `View` creates its local-light shadow atlases when a frame or probe capture first places a local shadow, not in `View::new`, so a view without one holds none. No game-code changes are needed.
- `sgl-3d` `asset::load` and `load_slice` load a glTF's `KHR_lights_punctual` lights into the new `Asset::lights` (`AssetLights`, placed with `PunctualLight::place`); before, the extension was ignored and a file requiring it was refused. A file whose lights the extension forbids (a bad type, negative intensity, zero range, invalid cone, a zero-scale spot or sun node) now fails to load. Loading installs nothing. Code that builds `Asset { .. }` adds `lights: Default::default()`.
- `sgl-3d` planar reflections (new): `Scene::add_planar_reflector(PlanarReflector::new(point, normal))` and `SurfaceMaterial::planar_reflector` make an opaque or masked surface mirror the scene, at `Settings::planar_reflections` (`Off`, `Quarter`, `Half` by default, `Full`); `View::planar_reflections` reports each picture. Code that builds a `SurfaceMaterial` or `Settings` field by field adds `planar_reflector: None` or `planar_reflections`; otherwise no game-code changes are needed.
- `sgl-3d` `timing::PassTime` gains `passes`, the timed passes in its group. No game-code changes are needed unless a game builds a `PassTime`.
- `sgl-post-fx`: a technique's pipeline comes from a `render_technique::PipelineCompiler` (in place by default; `set_pipeline_compiler` on the context, TAA and SSR), which is handed a `DescribePipeline` that passes the pipeline's descriptor to a sink, and an effect whose pipelines are not all ready returns `PostFxExecutionStatus::Pending`. `create_shader(file_name, entry_point, macros)` takes no device, `Shader`'s module is `Shader::module(&device)` in place of a public field, and `PostFXRenderTechnique::initialize_pso` takes the compiler and a `&'static str` name first. Direct callers update their arguments; games using SGL3D change nothing.
- `sgl-3d` adds HDR effects: a game's WGSL function over each view's resolved HDR frame, with its linear colour, depth and distance along the view ray, after TAA or FSR and before depth of field, motion blur, bloom and the tone map (`Scene::add_hdr_effect`, `hdr_effect_parameters_layout`, `remove_hdr_effect`, `Scene::hdr_effect` for `FrameInput::hdr_effect`, `None` by default; [guide](crates/sgl-3d/README.md#hdr-effects)); `DiagnosticTarget::HdrEffect` reads its frame. No game-code changes are needed; code that builds a `FrameInput` field by field adds `hdr_effect: None`, and exhaustive matches on `SceneError` add `UnknownHdrEffect` and `HdrEffectParameters`, on `DiagnosticTarget` `HdrEffect`. A material shader's `scene_depth` now returns `SCENE_DEPTH_FAR` at the sky under an orthographic or finite-far camera too, not the far plane's depth, and its module may call `projected_view_depth`, a name it can no longer declare.
- `sgl-3d` adds depth of field: `Settings::depth_of_field` (`DepthOfField::Off` by default, `Low`, `Medium`, `High`) focuses the antialiased frame through `FrameInput::depth_of_field` (`DepthOfFieldParameters`: focus distance, f-number, sensor height), after TAA or FSR and before motion blur; `DiagnosticTarget::DepthOfField` reads the focused frame. No game-code changes are needed; a game that matches every `DiagnosticTarget` adds the new variant.
- `sgl-3d` live specular probes (new): `Scene::add_live_specular_probe` places a probe the renderer captures on the GPU (`LiveSpecularProbe`, whose `hidden_groups` keep the reflecting object out of it), `set_live_specular_probe` moves it, and `Renderer::refresh_specular_probe(.., probe, ProbeRefreshPace::FacePerCall)`, called between frames when the game chooses, refreshes it with no readback or upload; the probe shows its last whole cube until the next is published ([guide](crates/sgl-3d/README.md#live-specular-probes)). Beside live probes, baked probes must be `Rgba16Float` at the same face size. Exhaustive matches on `SceneError` add `UnknownLiveSpecularProbe`. No other game-code changes are needed.

## 0.9.3 — 2026-11-20

Update SGL crates together to `0.9.3` and commit the lockfile. Existing game
code, settings and baked assets carry over.

- `sgl-3d` `Renderer` and `View` now skip shader and pipeline creation for unused GI, shadow, deformation, culling and optional image passes at startup. No game-code changes are needed.

## 0.9.2 — 2026-11-20

Move every SGL crate to `0.9.2` together and commit the updated `Cargo.lock`.
Games updating from 0.9.1 can keep their code, settings and baked assets.

- `sgl-3d` `Renderer` now creates each geometry pipeline on its first draw in a frame or probe capture, replacing eager creation during setup and scene changes. No game-code changes are needed.
- `sgl-3d` `View::new` and `View::new_surface` defer local exposure and presentation pipelines until the first frame that uses each one. No game-code changes are needed.

## 0.9.1 — 2026-11-20

Move every SGL crate to `0.9.1` together and commit the updated `Cargo.lock`.
No game code, data or settings changes are needed. Existing baked assets and
BC7 exports need no regeneration. Refresh the game's distribution notices to
carry the updated shadow-code attribution.

- `sgl-3d` ray-traced shadows no longer flash dark squares around thin geometry as coverage changes. No game-code changes are needed.

- `sgl-3d` `Settings::contact_shadows`: a floor seen near edge-on and the frame's border no longer shadow themselves in bands; a caster only a few pixels wide casts a thinner contact shadow. No game-code changes are needed.
- `sgl-3d` soft shadows (`Settings::soft_shadows`): a surface tilted to a large sun or a large point or spot light no longer shadows itself, and a caster's penumbra on a slope is no longer narrowed. No game-code changes are needed.

## 0.9.0 — 2026-11-20

Move every SGL crate to `0.9.0` together and commit the updated `Cargo.lock`.
Game code must change: `FrameInput::environment` is a `FrameEnvironment`,
`Settings::fsr_sharpening` and `fsr_sharpness` become `sharpening` and
`sharpness`, and `Scene::set_decal` loses its device argument; the first
three entries give each migration. `Settings`, `FrameInput`, `Light` and
`Fog` literals that name every field add the new fields below. The image
changes without code changes where soft shadows and sharpening now apply at
High, where a material has an occlusion map, and where dynamic GI occludes
the sky's reflection. Regenerate the game's distribution notices. Existing
baked assets and BC7 exports need no regeneration.

- `sgl-3d` `FrameInput::environment` is now `FrameEnvironment` (`None`, `Map(id)`, `Sky(Sky)`), was `Option<EnvironmentId>`: write `FrameEnvironment::Map(id)` for `Some(id)` and `FrameEnvironment::None` for `None`. `Sky` is a new procedural sky lit by the frame's directional lights, whose `illuminance` and `color` are then their light above the air. It gives diffuse and specular environment light, scaled by `diffuse_environment.intensity` and `reflection_environment.intensity`, and lights the dynamic GI volume, the fog's ambient share and probe captures; lower `hemisphere_light` where it stood in for sky light.
- `sgl-3d` `Settings::fsr_sharpening`/`fsr_sharpness` become `sharpening`
  (`settings::Sharpening::Preset`, `Off`, `On`) and `sharpness`, and now
  sharpen every antialiasing mode once, in post; TAA frames are sharpened on
  High by default, and FSR1 (`Upscaling::On`) follows `sharpness` instead of
  a fixed strength. Migrate `fsr_sharpening: true/false` to
  `Sharpening::On/Off` and rename `fsr_sharpness`; set `Sharpening::Off` to
  keep the previous TAA, SMAA and unantialiased image, and `sharpness: 0.9`
  for the previous FSR1 strength. Saved settings with the old keys still load,
  the keys dropped (unknown keys are ignored), as `Sharpening::Preset`:
  convert them on load as above. Timing group `sharpening` is new; `spatial upscale` times EASU alone,
  and an unsharpened EASU frame's final pass is timed under `tone map`.
- `sgl-3d` `Scene::set_decal` no longer takes the device, as `set_light`:
  `scene.set_decal(&queue, id, decal)`.
- `sgl-3d` `Settings::contact_shadows` (new, `Off` by default; `Short`, `Medium`, `Long`): screen-space contact shadows for the directional light with cascades, at the cost of the opaque stage's two-pass form. New timing group `contact shadows`. Distribution notices gain Bend Studio's Apache-2.0 notice: regenerate the game's notices. No game-code changes are needed.
- `sgl-3d` shading: a material's occlusion map now also shadows direct light (micro-shadowing), where it occluded ambient light only; materials without one are unchanged. No game-code changes are needed; lower `occlusion_strength` to weaken it, and look at occlusion-mapped materials under grazing light. Probe captures and other bakes taken before keep the earlier light and need no regeneration; recapture deliberately if the difference matters.
- `sgl-3d` `Fog::start` (new, 0 by default): the frame's fog medium begins that many metres from the camera, leaving nearer surfaces unfogged. No game-code changes are needed.
- `sgl-3d` `Sky::aerial_perspective` (new, default 1): under a procedural sky, distant opaque and blended surfaces, glow and mist take the air's haze through the fog volume. Set 0 for none. Frames without a sky are unchanged; no code changes are needed.
- `sgl-3d` adds `FrameInput::local_exposure` (`LocalExposure`, both
  contrasts 1 by default, which changes nothing); below 1 it draws large
  bright or dark regions toward middle grey before the tone map and keeps
  local detail. Timing group `local exposure` is new. No game-code changes
  are needed.
- `sgl-3d` `View::geometry_stats_by_model` polls once and borrows one
  completed frame's per-model counts for multiple lookups. Existing
  `geometry_stats_for_model` calls need no changes.
- `sgl-3d` `View` now creates volumetric fog and occlusion-culling pipelines
  on first use and keeps them for later frames. No game-code changes are needed.
- `sgl-3d` `Settings::screen_space_indirect` (new, `Off`) and
  `FrameInput::screen_space_indirect_radius` (new, 5.0 m): opt-in diffuse
  bounce between nearby on-screen surfaces. Add both fields to exhaustive
  literals or use `..`. Game shaders may now call `pack2x16float`,
  `unpack2x16float` and `quantizeToF16`, and `Scene::add_shader` refuses a
  module that declares `ScreenIndirect`, `ScreenIndirectView`,
  `ScreenIndirectTaps`, `ScreenIndirectKept`, `camera_screen_indirect`,
  `raster_screen_indirect` or a `screen_indirect_*` or `SCREEN_INDIRECT_*`
  name: rename any such declaration; no other game-code change is needed.
- `sgl-3d` `Light` gains `cookie: Option<LightCookie>` (default `None`): a
  spot projects a decal image (`LightCookie { image, up }`) that multiplies
  its light by the image's colour times alpha on surfaces, ray hits and the
  fog; an image a light projects cannot be removed, and `add_light` and
  `set_light` can now return `UnknownDecalImage` and `DeviceLimit`. A `Light` literal that
  names every field adds `cookie: None`; one built with
  `..Default::default()` needs no change.
- `sgl-3d` dynamic GI: where the volume lights a surface, the sky's share of
  its environment specular is now occluded by the sky the probes see, so
  shaded and enclosed surfaces stop reflecting the sky at full strength. No
  game-code changes are needed.
- `sgl-3d` `Settings::soft_shadows` (new, `Preset`: on at High, off at Low): camera shadows from the maps widen with distance from the caster by `DirectionalLight::angular_diameter` and a point or spot light's `radius`, under TAA or FSR at `ShadowQuality::High`. No game-code changes are needed; set `SoftShadows::Off` for the fixed filter.

## 0.8.1 — 2026-11-19

Move every SGL crate to `0.8.1` together and commit the updated `Cargo.lock`.
No game-code changes are needed. Existing baked assets and BC7 exports need
no regeneration.

- `sgl-post-fx` `ScreenSpaceReflection` (SGL3D Crystal) now weights reflection history by the motion of the reflector and the reflected source. Smooth changing light can retain history during camera motion. No game-code changes are needed.

## 0.8.0 — 2026-11-18

Move every SGL crate to `0.8.0` together and commit the updated `Cargo.lock`.
Game code must change: a renderer is now a `Renderer` and one `View` per
camera, `Scene::update_mist` loses its queue argument and
`Renderer::ray_traced_shadows_in_effect` takes the primary view's settings;
the entries below give each migration. Direct `sgl-post-fx` callers follow
its entries too. Existing baked assets and BC7 exports need no regeneration.

- `sgl-3d` TAA: a moving instance posed from its last submitted pose no
  longer re-arms full rejection across the whole view (its motion vectors
  re-arm only the pixels it covers and leaves), and a camera at rest away
  from the render origin is no longer taken as moving on f32 rounding of its
  matrices. Thin and sub-pixel detail now settles at rest while anything
  else in the scene moves, on screen or off. Appearances, departures,
  showing or hiding, model or geometry replacement, a moved
  `Mobility::Static` instance, drawing-changing material edits and a changed
  `FrameInput::visibility_mask` still re-arm the view. No game-code changes
  are needed; pose movers as `Mobility::Moving` every frame, as before.
- `sgl-post-fx` `TemporalAntiAliasingAttribs`: the resolve no longer
  compares cameras on the GPU; new `camera_motion` (with `padding0` to
  `padding2`, now 32 bytes) carries it, set from the new
  `TemporalAntiAliasing::camera_moved(&current, &previous)` over the
  cameras given to the context. `rigid_object_motion` is now for changes no
  motion vector describes. Direct callers set
  `camera_motion: u32::from(TemporalAntiAliasing::camera_moved(&curr, &prev))`
  and stop raising `rigid_object_motion` for moves whose motion vectors they
  write.
- `sgl-3d` motion vectors: an unchanged camera and pose now write exactly
  zero motion at any distance from the render origin (the previous position
  was composed in another order, leaving about 2e-4 px 100 m out). No
  game-code changes are needed.
- `sgl-post-fx` TAA, every flag: a still pixel (motion under 0.01 px on
  both axes) reads its own history instead of reprojecting by that residue,
  so outlines at rest no longer drift past the scene's radiance range far
  from the render origin. No code changes are needed.
- `sgl-3d` temporal masks (FSR's transparency-and-composition mask, TAA's
  responsive masks): an opaque or masked material with a game shader
  (`SurfaceMaterial::shader`) was marked on every frame; it is now marked
  only while its module reads `ctx.time` or `ctx.phase`, or in the frame
  after `Scene::set_shader_parameters` or `Scene::set_instance_shader_data`
  changes what it reads (an edited masked material over its whole
  footprint), so a still shaded surface now reaches TAA's resting stability
  and FSR's locks. Overlapping blended surfaces and glow could raise a
  pixel's reactivity above 0.9 (two 0.9 draws gave 0.99); reactivity now
  combines within AMD's recommended 0.9. No game-code changes are needed;
  write shader parameters and instance data only when they change
  (rewriting the same values marks nothing).
- `sgl-3d` glow (`Scene::update_effects`): its reactive-mask value was its
  radiance times alpha in the scene's light units; it is now that at the
  frame's exposure (`FrameInput::exposure`), what the glow adds to the
  displayed frame. No game-code changes are needed.
- `sgl-3d`: the sky's motion vectors follow a turn of
  `Backdrop::Environment`'s `yaw` between finished frames as well as the
  camera's rotation, so TAA, FSR and motion blur track a rotating backdrop;
  before, the sky wrote the camera's rotation only. A change of brightness,
  colour or environment still writes no motion. No game-code changes are
  needed.
- `sgl-3d` reflections: Crystal (the default screen-space method) keeps its
  accumulated history on a surface that moves with the camera, such as a
  vehicle a chase camera follows; its reflections there drifted by a pixel a
  frame or flickered while it moved. Velvet at
  `Settings::screen_space_reflections` Half keeps its history at the
  silhouettes of moving or camera-followed objects, which were noisy while
  they moved. No game-code changes are needed.
- `sgl-3d` ray-traced shadows (`Settings::ray_traced_shadows`): the denoiser
  and the temporal blend now keep a surface's history when the camera
  follows it or travels fast toward still scenery (they compared depths
  through the camera alone and against an absolute metre), and no longer let
  surfaces under a metre apart far away share history. No game-code changes
  are needed.
- `sgl-3d` `FrameInput::fog` (`Fog::length`): changing the fog length no
  longer restarts the volumetric fog's history and jitter. The fog
  reprojects through the last frame's length, so a length animated or
  recomputed every frame keeps accumulating. No game-code changes are
  needed; a game that held its length constant to keep fog history may now
  animate it.
- `sgl-3d` `Renderer`, `View`, `ViewFrame`, `ViewError`: a renderer now
  draws several views of one scene in one frame. `Renderer::new(&device)`
  takes only the device; each camera is a `View::new(&renderer, …)` or
  `View::new_surface(&renderer, …)` taking the old `Renderer::new` and
  `Renderer::new_surface` arguments after the renderer. `resize` and the
  per-camera queries (`render_size`, `scene_size`, `output_encodes_srgb`,
  `antialiasing_in_effect`, `fsr_error`, `geometry_stats`,
  `local_shadow_stats`, `occlusion_culling_in_effect`, and with
  `diagnostics` `diagnostic_target`, `diagnostic_draws`,
  `diagnostic_view_times`, `geometry_stats_for_model`,
  `take_frame_probe_reports`) move to `View`; `render` takes
  `&mut [ViewFrame { view, input, settings, output, timing }]`;
  `finish_frame(&mut scene)` is unchanged; `RendererError` is `ViewError`;
  `capture_specular_probe` and `capture_specular_probe_async` take the
  `&mut View` whose shadow maps they borrow, after `scene`. Game code must
  change; a single view:

  ```rust
  let mut renderer = Renderer::new(&device);
  let mut view = View::new(&renderer, &device, &queue, format, size, scale, &settings)?;
  // each frame, after editing the scene:
  view.resize(&device, size, scale, &settings);
  renderer.render(&device, &queue, &mut encoder, &mut scene,
      &mut [ViewFrame { view: &mut view, input: &input, settings: &settings, output: &target, timing: None }]);
  queue.submit([encoder.finish()]);
  renderer.finish_frame(&mut scene);
  ```
- `sgl-3d` several views: the first listed `ViewFrame` is the frame's
  primary view; its `hardware_ray_tracing`, `dynamic_gi`,
  `anisotropic_filtering`, diagnostics layers and visibility mask (for rays)
  apply to the frame, and dynamic GI follows its camera. A view left out of
  a finished frame of its scene restarts its history, and still reads back
  its statistics for the frames it was in. List every view of a frame in
  one `render`: two renderers drawing one scene in a frame make the first's
  views restart every frame. Each view holds its own targets, FSR context
  and local-light shadow atlas. One renderer shows one scene at a time. No
  game-code change beyond the migration above for a single view.
- `sgl-3d` `Renderer::ray_traced_shadows_in_effect`: takes the frame's
  primary view's settings after the view's (`(&settings, &primary,
  &scene)`), whose `hardware_ray_tracing` every view of the frame takes; it
  read the view's own, which was wrong for a secondary view. For one view
  pass its settings twice: `ray_traced_shadows_in_effect(&settings,
  &settings, &scene)`.
- `sgl-3d` `Scene::update_mist`: drops its unused `queue` argument. Game
  code changes: `scene.update_mist(&device, &positions)`.
- `sgl-3d` `Renderer::finish_frame`: commits what the frame drew (each
  instance's pose, visibility and shader data, deformations, shader
  parameter blocks, the rigid-change signal and static-edit bounds as they
  were when `render` ran); scene edits made after the submission and before
  `finish_frame` no longer become the finished frame's state (which zeroed
  the next frame's motion) and belong to the next frame, judged against
  what that frame drew (a model swapped back in that window raises the
  rigid-change signal). A frame rendered
  and not finished (its encoder dropped, or `finish_frame` omitted), whether
  or not its commands were submitted, and then rendered again as the same
  frame no longer restarts TAA, screen-space (Crystal, Velvet) and
  world-space reflections, ambient occlusion or ray-traced shadows; FSR
  still restarts, and filtered fog skips one frame of reprojection. A cut
  in an unfinished frame holds until a frame is finished (the next frame
  resets too, whatever its `camera_cut` says), and a stage switched off in
  the unfinished frame (antialiasing, a reflection method, another
  projection) restarts on the next, as after a finished one; ambient
  occlusion and ray-traced shadows continue, as FSR does when it did not
  run in the unfinished frame. Ray-traced
  shadows hold one more reduced-resolution texture while they run. No
  game-code change.
- `sgl-3d` `Scene::set_shader_parameters`: a material that gains or changes
  its shader no longer writes one frame of motion from all-zero parameters;
  its first frame measures from its own block. No game-code change.
- `sgl-3d` `diagnostics::Counters`: gains `deformations`, the deformations
  the deform pass dispatched. A game that builds `Counters` with a struct
  literal adds the field.
- `sgl-post-fx`: `TemporalAntiAliasing`'s motion-adaptive resolve reads each
  of `RenderAttributes::responsive_masks` as its largest value over the
  pixel's 3x3 neighbourhood, as FSR dilates its masks, so the jittered edges
  of still marked content settle at the mask's strength instead of
  alternating with full strength; marking now reaches one pixel beyond what
  the mask covers. No game-code changes are needed.
- `sgl-post-fx` `screen_space_reflection::RenderAttributes::motion_vectors_srv`:
  SSR's temporal pass now reads depth motion from the motion vectors' Z
  (previous minus current camera Z) and W (its validity, above 0 valid) and
  compares history with the depth that gives, so it requires a four-channel
  view (such as `Rgba16Float`) and panics on fewer, as motion-adaptive TAA
  does. Direct `sgl-post-fx` callers pass `[ndc_x, ndc_y, depth_motion,
  valid]`; a still scene passes `[x, y, 0., 1.]`. SGL3D games need no
  change.
- `sgl-post-fx` `TemporalAntiAliasing`, `ScreenSpaceReflection`: executing
  the same frame index again repeats that frame, its reset included, instead
  of restarting history. No change for callers that advance the index.

## 0.7.1 — 2026-11-18

Move every SGL crate to `0.7.1` together and commit the updated `Cargo.lock`.
No game-code changes are needed. Existing baked assets and BC7 exports need
no regeneration.

- `sgl-3d` reflections: world-space rays and Velvet keep their accumulated
  history on a surface that moves with the camera, such as a vehicle a chase
  camera follows. Its reflections were noisy while it moved and settled only
  at rest; they are now as steady in motion. No game-code changes are
  needed.

## 0.7.0 — 2026-11-17

Move every SGL crate to `0.7.0` together and commit the updated `Cargo.lock`.
A game that uses `sgl-3d` through its settings needs code changes only where
it awaits a probe capture in the browser, matches `DiagnosticTarget`
exhaustively, or looks up GPU timing groups by name; a direct caller of
`sgl-post-fx`'s TAA updates its attributes. `sgl-core`, `sgl-net` and
`sgl-input` have no API changes. Existing baked assets and BC7 exports need
no regeneration. Most of this release corrects what SGL3D rendered: expect
the image to change where the entries below say so.

- `sgl-3d::Renderer::capture_specular_probe_async`,
  `sgl-3d::diagnostics::read_async` and
  `sgl-2d::canvas::Renderer::read_scene_async` (with `CaptureError`, now on
  every target) await their readback, so they run in the browser too;
  `capture_specular_probe`, `diagnostics::read` and `read_scene` block on
  them natively, as before. In a browser build, where it always failed,
  `capture_specular_probe` is gone: await `capture_specular_probe_async`.
- `sgl-3d::diagnostics::DiagnosticTarget::Antialiased` (`diagnostics`
  feature) returns the last frame's linear HDR scene as antialiasing leaves
  it (TAA's or FSR's output, else the composite). Exhaustive
  `DiagnosticTarget` matches add `Antialiased`.
- `sgl-post-fx` `temporal_anti_aliasing`: `FeatureFlags::MOTION_ADAPTIVE_HISTORY`
  adds motion-adaptive history rectification, which keeps static sub-pixel
  geometry steady at rest while camera motion, object motion and depth
  disocclusion restore full rejection. Direct callers replace the unused
  `TemporalAntiAliasingAttribs::padding0` with `rigid_object_motion` (default
  zero), setting it when rigid geometry moves, appears or departs (the struct
  remains 16 bytes), and add `responsive_masks: &[], neighborhood_motion: None`
  to `RenderAttributes` to keep the resolve as it was; those two inputs
  (`NeighborhoodMotion`) let the flag reject history at rest where content
  changes without a motion vector of its own, and
  `TemporalAntiAliasing::prepare_responsive_variant` creates such a resolve
  ahead of the frame that needs it. Under the flag
  `RenderAttributes::motion_vectors_srv` must have four channels (such as
  `Rgba16Float`): Z the surface's previous minus current camera-space Z and W
  its validity (above 0 valid); the closest-motion history's Z and W carry
  holds. Under every flag an edge in motion no longer gets a fringe brighter
  or darker than anything in the scene: history is clipped within the
  neighbourhood's min/max range as well as its variance box.
- `sgl-3d` TAA uses that motion-adaptive history. Static sub-pixel geometry
  settles at rest, near, far and against the sky. Full rejection returns on
  camera motion, rigid changes, a changed `FrameInput::visibility_mask`, a
  `Scene::set_material` edit that changes what is drawn, local motion and
  disocclusion, and, with the camera at rest, wherever content changes
  without a motion vector of its own: blended surfaces, additive effects,
  mist, heat shimmer and moving normal layers (by the reactive and
  transparency-and-composition masks, now drawn in TAA frames that can mark
  them), and pixels within 32 to 63 pixels of anything moving a pixel a frame
  or more, so a deforming or skinned surface leaves no ghost where it was.
  The pass timing group `FSR composition` is renamed `temporal composition`,
  and TAA adds `TAA motion reach`: update any timing lookups by those names.
  No settings and no other game-code changes are needed.
- `sgl-3d` FSR2 at the frame's edges and corners: no darkened corner pixel
  (14% at the bottom-left on some jitter phases), no negative values in the
  column the camera turns towards at Ultra Performance, and no dip below the
  scene beside a brighter surface at the edge. It takes `sp-fidelity` and
  `sp-fidelity-wgpu` 0.3.2, which correct three faults of AMD's FSR2 there.
  FSR3 is unchanged. No game-code changes are needed.
- `sgl-3d` heat distortion (`Settings::heat_distortion`) now marks the
  transparency and composition mask where it covers, so FSR, and TAA by the
  entry above, no longer hold the shimmer as static detail. No game-code
  changes are needed.
- `sgl-3d` world-space reflections (`Settings::world_space_reflections`): a
  surface beside a nearer one, or meeting another at a crease, keeps its own
  reflection up to the edge (it took up to 72% of the other's over a few
  pixels); a reflection beside an untraced surface or a blended receiver no
  longer fades toward its fallback along a line one or two pixels wide; and
  the opaque surface a moving blended receiver uncovers no longer fades in
  over about a second. Image-changing along those edges only; no settings.
  No game-code changes are needed.
- `sgl-3d` ray-traced shadows (`Settings::ray_traced_shadows`): thin lit
  features against the sky or an unlit surface, and normal-mapped surfaces
  under a low light, no longer read as shadowed where nothing occludes them;
  a feature one render pixel wide at an odd column or row takes its own
  shadow, where it took its neighbours'; a one-pixel slit showing a far
  surface takes that surface's shadow from the texels about it. No settings.
  No game-code changes are needed.
- `sgl-3d` hardware ray tracing (`Settings::hardware_ray_tracing`) no longer
  traces a frame as if a model were absent when its BLAS was compacted just
  before it (seen on Metal under GPU load, one frame of rays through it
  meeting nothing): the TLAS keeps the built BLAS until the GPU has finished
  the compacted copy. No game-code changes are needed.
- `sgl-3d` over-range radiance that a backend stores as +infinity no longer
  presents black or NaN: reflection source completion reads the lit colour
  and the ambient light, and motion blur, bloom, tone mapping and the
  transmission copy read the scene colour, as 65504. It showed under opaque
  fog, where occlusion hides ambient light, behind a transmissive surface,
  and where neither TAA nor FSR ran. No game-code changes are needed.
- `sgl-post-fx` SSR now treats as background only depths at or beyond the
  camera's far plane, whatever the near plane (it lost surfaces past 909 m
  with a 1 mm near plane, past 9.1 km with 0.1 m), and every pass after the
  full-resolution mask takes eligibility from it rather than re-testing the
  8-bit roughness. No game-code changes are needed.

## 0.6.0 — 2026-11-12

Move every SGL crate to `0.6.0` together and commit the updated `Cargo.lock`.
Apply the SGL3D migrations below. The other five library crates have no API
changes; no game-code changes are needed for them.
Existing baked assets and BC7 exports need no regeneration.

- `sgl-3d` removes the Git-only FSR3 frame generation, `Settings::frame_generation`,
  `Presenter` and `PresentationFrame`. Use `Renderer::new_surface`, render
  scene and UI into the acquired surface, submit once, call
  `Renderer::finish_frame`, then present through wgpu. Use
  `Renderer::output_encodes_srgb()` to match direct UI composition to the output.
  FSR2/FSR3.1 upscaling and AO remain available. Remove saved frame-generation
  choices in consumers.

- `sgl-3d::Settings::ambient_occlusion` now matches raster jitter, advances
  XeGTAO's sample phase with effective TAA/FSR, and temporally filters visibility
  before lighting, including moving receivers. All antialiasing modes accumulate
  AO; Off/SMAA keep a fixed sample phase. Camera cuts and AO changes restart
  history. No game-code changes are needed.
- `sgl-3d::diagnostics::DiagnosticTarget::Motion` changes from RG16Float to
  RGBA16Float: RG retains UV motion, B is previous-minus-current perspective
  view depth in metres, and A marks valid depth motion. Update diagnostic
  readback strides from 4 to 8 bytes/texel; consumers needing UV use RG.

- `sgl-3d::Antialiasing` now offers mutually exclusive `Fsr2` and `Fsr3`
  temporal upscaling, with unchanged defaults and TAA fallback; FSR3 does not
  enable frame generation. Rename `Fsr2Quality` to `FsrQuality`, settings
  `fsr2_quality`/`fsr2_sharpening`/`fsr2_sharpness` to `fsr_*` (including saved
  data), `graphics_device::fsr2_features` to `fsr_features`, `Renderer::fsr2_error`
  to `fsr_error`, and diagnostics `disable.fsr2` to `disable.fsr`.
  Exhaustive `Antialiasing` matches must handle `Fsr3`.
- `sgl-3d` now uses the published `sp-fidelity` and `sp-fidelity-wgpu` crates
  instead of temporary Git pins, with frame generation disabled. Remove local
  patches for the old Git source if present; otherwise no game-code changes
  are needed. Refresh dependency notices; licence obligations are unchanged.

- `sgl-3d` showcase renders move from repository PNGs to a Cloudflare-hosted
  gallery with Slide/Toggle feature comparisons and a scene sidebar. Use the
  hosted README image URLs; publishing now uses
  `spdeploy --operation deploy --no-ui` and cached, bounded curl downloads.
  Install curl on the publishing machine; no game-code changes are needed.
- `sgl-3d::Fog::length` changes now restart fog history instead of reusing
  density at the old range's positions. No game-code changes are needed.
- `sgl-3d::MaterialShader` masked shadow casters now evaluate all material
  maps and mapped normals before custom coverage, matching camera passes;
  cached shadows follow material edits and scrolling normals too.
  No game-code changes are needed.
- `sgl-3d::ReflectionMethod::Velvet` and world reflections now reproject
  hit depth along the view ray, reject offscreen history candidates before
  choosing one, and compare depths in the previous camera's space. This
  prevents misplaced or discarded history; no game-code changes are needed.
- `sgl-3d::SurfaceMaterial::anisotropy_strength` now uses the decal-adjusted normal in the
  separate material pass, matching devices with larger attachment budgets.
  No game-code changes are needed.

- `sgl-3d-examples::{bistro, browser_bistro}` and `measure-browser` now accept
  `--split` to diagnose the existing two-pass opaque path and record
  `split_opaque` in reports. Defaults are unchanged; rebuild the examples to
  use the diagnostic, with no game-code or asset changes.

- `sgl-3d-examples::{bistro, browser_bistro}` accept explicit
  `transmissive_materials` fixture authoring for dielectric bulb shells; the
  opt-in `prepare-bistro-practical-lights.ts` recipe enables practical shadows
  instead of the baseline's unshadowed points. Existing fixtures are unchanged;
  generate the separate recipe to try it, with no game-code or asset changes.

- `sgl-3d-examples::{bistro, browser_bistro}` reports now include actual
  local-shadow atlas occupancy, overflow and draw work, with first-frame
  population separate from measured frames. Rebuild the examples to record
  these fields; no game-code changes or asset regeneration are needed.

- `sgl-3d-examples` now includes an optional Intel Sponza/Colorful Curtains
  lighting workload alongside Bistro, with neutral baselines and a source-HDR
  sunlit/GI recipe covering the full building rather than only the atrium.
  Run the [local-only bootstrap](examples/sgl-3d/SPONZA.md)
  and explicit sky/gallery-reflection exports; no game-code changes are needed.

- `sgl-3d` Bistro fixtures now use higher sunlight, façade-covering GI,
  authored point lights and ACES filmic instead of broad building shadow
  and emissive-only lamps. Rebuild the example and replay the fixtures;
  no game-code changes or asset regeneration are needed.

- `sgl-3d::{SceneOptions, PreparedModelOptions}`: scenes can now explicitly
  omit ray-only data with `RayData::Disabled`; existing constructors retain
  it. Use matching options for manually prepared models.
  `Renderer::{dynamic_gi_in_effect, ray_tracing_in_effect,
  ray_traced_shadows_in_effect}` now take `&Scene` after `&Settings`, and
  `world_space_reflections_in_effect` reports the same scene-aware fallback.
  ETC2/ASTC material images can omit their RGBA companion only when ray data
  is disabled; enabled rays and decal uploads reject a missing companion.
  No rendering settings are silently rewritten.

- `sgl-3d::SceneError` adds `MissingDecodedImage` and `RayDataMismatch`
  for missing ETC2/ASTC ray/decal companions and mismatched scene/model
  capabilities. Add both variants to exhaustive matches.

- `sgl-3d::asset::LoadOptions::budget`: import safety caps are now explicitly
  configurable with checked `LoadBudget::new(source_bytes, decoded_bytes)`;
  defaults remain 4 GiB per cap natively and 1 GiB on wasm32. Add `budget`
  to exhaustive `LoadOptions` literals, or use `..Default::default()`.
  Supplied images now count toward the decoded cap too; large callers must
  raise it explicitly or split their load. KTX2 constructors have matching
  `*_with_budget` variants; existing calls retain their defaults.

- `sgl-3d-examples::browser_bistro`: real prepared glTF scenes can now be
  measured and captured in WebGPU with shared native cameras, explicit
  fixture settings and optional lossless image deduplication during export.
  Run the [Bistro workflow](examples/sgl-3d/BISTRO.md); existing assets and
  library defaults are unchanged, and no game-code changes are needed.

- `sgl-3d-examples::browser_baseline`: `measure-browser --baseline` now
  measures a textured moving scene and bounded upload phases, with optional
  prepared mips, reduced-resolution spatial upscaling and frame capture.
  The existing streaming benchmark remains the default; no game-code changes
  are needed.

- `sgl-3d::asset::{CompressedFormat, CompressedImage, Image}`: material
  images now support ETC2 RGBA8 and ASTC 4×4 stored mip chains with an explicit
  decoded level-zero ray/decal companion, plus prepared `Rgba8MipChain`
  uploads. Request the matching compression features; existing BC7 struct
  literals must add `decoded_level_0: None`, and exhaustive matches must add
  `Image::Rgba8Mips`, `CompressedFormat::Etc2Rgba8` and
  `CompressedFormat::Astc4x4`. Existing BC7 exports need no rebake.

- `sgl-3d::Settings::upscaling`: reduced-resolution scenes can now opt into
  portable spatial upscaling after tone mapping; the default remains `Off`
  and native FSR2 is unchanged. Set `upscaling: Upscaling::On` with a smaller
  `scene_resolution`; settings literals without a default tail must add the field.

- `sgl-3d::Renderer`: unused FSR2 masks and post-processing targets are no
  longer always allocated, and XeGTAO retains unchanged bind groups between
  frames. No game-code changes are needed. Distribution notices now include
  the additional AMD FSR1 source attribution; regenerate the game's notices
  when adopting the port.

## 0.5.0 — 2026-10-24

New sgl-2d features; nothing changes for a game that does not opt in.

- `sgl-2d::canvas::Renderer::set_sprite_filter(linear)`: sprite pages were
  always sampled nearest; linear sampling is now selectable (nearest stays the
  default). Under linear sampling every fragment's sample is clamped half a
  texel inside its sprite's page rect, so atlas neighbours never bleed and
  1:1 sprites and text stay texel-exact. Opt in with
  `renderer.set_sprite_filter(true)`; otherwise no game-code changes are needed.
- `sgl-2d::canvas::SpriteInstance::shape`, `sgl-2d::canvas::Shape`,
  `sgl-2d::UiFrame::rounded_rect`: anti-aliased signed-distance rounded
  rectangles (fill, inner border of a given width and colour, corner radius)
  drawn through the sprite pipeline with the usual z order, clip and rotation,
  one target pixel wide at any UI scale. `SpriteInstance::new` sets
  `shape: None`; a `SpriteInstance` struct literal written without
  `..SpriteInstance::new(..)` must add `shape: None`. Draw one with
  `frame.rounded_rect(rect, radius, fill, border_width, border_color)`.
- `sgl-2d::UiFrame::clip()`: returns the clip currently applied, so a widget
  that clips its own text can narrow it and restore it instead of replacing an
  enclosing scroll area's clip. No game-code changes are needed.

## 0.4.2 — 2026-10-13

- `sgl-3d::Settings::tone_mapping`: fixed AgX is now selectable Khronos PBR
  Neutral (default), `AcesFilmic`, or `Agx`; grading stays neutral. Set
  `tone_mapping: ToneMapping::Agx` to retain the former transform and
  `ColorGrading::agx_look`; otherwise no game-code changes are needed.
  Distribution notices now include Khronos's Apache-2.0 attribution.
- `sgl-3d::Renderer::new`: browser float composition textures were incorrectly
  sRGB-encoded; float textures now stay linear on both platforms. No game-code
  changes are needed for composition. Direct surface callers use
  `Renderer::new_surface(device, queue, &surface_config, &surface_caps, device_scale, settings)`
  to select transfer from the actual format, colour space and capabilities.
- Repository: each crate's examples and integration tests moved to the
  unpublished `examples/<crate>` and `tests/<crate>` packages; run examples
  with `cargo run -p <crate>-examples --example <name>` (was `-p <crate>`).
  Published packages are unchanged; no game-code changes are needed.

## 0.4.1 — 2026-10-13

- Move every SGL crate to `0.4.1` together. Fixes only, but some change a
  signature, refuse input that loaded before, or change output; read every
  entry below. Compile breaks include `CollisionConfig` literals,
  `Grid2::filled`, custom `DatagramTransport`s, `EndpointConfig` literals,
  `sgl-2d` texture uploads, `scroll_area_end` and `Overlay` literals,
  `Scene::update_mist` and `update_effects`, newly native-only capture APIs
  on wasm32, and exhaustive matches on `GridError`, `AseDirection`,
  `AsepriteError`, `SceneError` and `Ignored`.

### sgl-core

- `CollisionConfig`: public fields let literals and mutation bypass
  validation; fields are now private, and `new` also panics on a negative or
  non-finite `skin`, `snap_distance` or `block_epsilon`, or an `up` whose
  length overflows. Migration: build with `CollisionConfig::new(up, skin,
  snap_distance, normal_threshold, block_epsilon)`, read through `up()`,
  `skin()`, `snap_distance()`, `normal_threshold()` and `block_epsilon()`,
  and reconstruct to change a value.
- `Grid2::filled`: panicked when the cells could not be allocated; now
  returns `Result<Self, GridError>` with new `GridError::TooLarge`.
  Migration: `Grid2::filled(w, h, 0u8)?`; exhaustive matches add the arm.
- `derive_stream_seed` components no longer cancel, so every derived seed
  changes, and `Rng::usize` on wasm32 now gives native's values. Content
  regenerated from a persisted base seed (worlds, replays) changes:
  regenerate it, or store the derived seeds before upgrading.
- `FrameAnimation::new` now panics on an infinite `fps`, and
  `FrameAnimation::tick`/`AnimationSequence::tick` ignore a nonpositive, NaN
  or infinite `dt`. Pass a finite `fps` and positive elapsed time.
- `StateHasher` promises distinct digests only within one schema; encoding
  and digests are unchanged. Games hashing several kinds of state in one
  stream add a leading tag or version.
- Fixes: `move_and_collide`, `sweep_aabb` and `snap_to_ground` no longer
  miss exact touches, grid-cell-boundary contacts or tiny motion, keep skin
  back-off on the swept path (diagonal slides stop slightly earlier), never
  push a body deeper into a solid it starts inside, and block one-way
  platforms by motion into the face under a tilted `up`; huge `ColliderSet`
  boxes no longer walk every cell; `AnimationSequence` ping-pong and
  zero-duration steps play once; `SequenceStep::RandomPause` is documented as
  `[min, max]`. No game-code changes.

### sgl-net

- `udp::DatagramTransport`: `send` now returns `io::Result<()>` and
  `receive` `io::Result<Option<(usize, SocketAddr)>>`, so refused sends keep
  their work and socket errors no longer end the poll. Standard adapters
  need no change; custom transports return their send result (temporary
  buffer exhaustion as `WouldBlock`), `Ok(None)` when empty and `Err` for a
  failed receive.
- `EndpointConfig::max_reliable_transmissions` is removed: UDP resends back
  off up to 1 s, and a peer closes `TimedOut` only after `timeout_ms` of
  silence, or 2 × (`timeout_ms` + 1 s) without acknowledgement. Migration:
  delete the field from literals; set `timeout_ms` for how long a stalled
  peer may last.
- `ClientIo::disconnect`/`ServerIo::disconnect`: an open connection now
  reports exactly one `Disconnected { reason: Local }` on every transport
  (was nothing, `Peer` or timing-dependent), a UDP client still in its
  handshake too (without `Connected`), and `ServerIoMux` forwards it.
  Games that clean up when they call `disconnect`: ignore that event, or
  move the cleanup to it.
- Configuration now refused: `NativeWebSocketClient::connect` with any
  scheme but `ws` (`InvalidInput`; it connected `wss://` in plaintext),
  WebSocket subprotocol names that are not one non-empty HTTP token, and
  `BrowserWebSocketConfig::latest_buffered_bytes` below
  `ENVELOPE_HEADER_LEN + MAX_LATEST_STATE_BYTES` (1186). Use `ws://` and
  terminate TLS at the edge, rename subprotocols on both peers, and raise a
  smaller watermark.
- `MemoryServerIo::send` after a disconnect returns `UnknownConnection`
  (was `Disconnected`), as every other server transport does. Match
  `UnknownConnection`.
- Changed behaviour, no game-code changes: native WebSocket reports a
  dropped stream as `Peer` and bad framing as `ProtocolViolation`; an
  over-cap inbound message on a full lane closes with `ProtocolViolation` at
  once (it waited on native, and closed `InboundOverflow` in the browser); browser `InboundOverflow` and
  `ProtocolViolation` closes reconnect under `ReconnectPolicy`, whose
  `max_attempts` count restarts only after a connection stays up
  `max_delay_ms`; native sockets set `TCP_NODELAY`; a 0 ms first round trip
  seeds UDP retransmission timing; `Delivery::LatestState` docs now promise
  it waits only for lane frames flushed with or before it.
  `udp::LANE_MESSAGES_PER_PEER_PER_POLL` is public.
- Fixes: UDP handshakes survive lost, reordered or late accepts, denies,
  confirms and cookies, including after `stop_admission`; closing and dropped
  endpoints notify peers and keep acknowledging through the grace;
  `ThreadedUdpServer` delivers messages before `Disconnected`, sends what was
  queued before `disconnect`, and no longer panics after a worker panic;
  `NativeWebSocketClient::connect` tries every resolved address, IPv6
  literals included, within one `handshake_timeout`; IPv6 literal origins are
  accepted; WebSocket worker fairness, keepalive, graceful close, browser
  flush pacing and `disconnect` during backoff; `memory_duplex` unreliable
  bounds; dual-stack challenge buckets. No game-code changes.

### sgl-2d

- Texture uploads: `Renderer::upload_texture`, `upload_normal_map`,
  `upload_light_cookie`, `SpritePass::upload`, `upload_normal` and
  `LightPass::upload_cookie` return `Result<(), TextureError>`, and
  `Renderer::replace_texture`/`SpritePass::replace`
  `Result<bool, TextureError>` (new `sgl_2d::canvas::TextureError`), where
  they panicked or ignored bad input; a normal map before its diffuse is
  `TextureError::DiffuseNotUploaded`, and a second one replaces the first.
  Handle or `.expect` each result and upload the diffuse first.
- `UiFrame::scroll_area_end` now takes the area's offset; a wheel tick
  scrolls only the innermost hovered area (from the next frame), areas nest
  within and restore the enclosing clip, and while the scrollbar shows,
  content is clipped `Ui::SCROLLBAR_WIDTH` (8 px) short. Migration:
  `frame.scroll_area_end(&mut offset);`, keep rows within
  `rect.max.x - Ui::SCROLLBAR_WIDTH`, and drop any `set_clip` that only
  restored the outer clip.
- `Overlay::units: WorldUnits` (new, default logical pixels): world-channel
  overlays under a world-unit camera shrank by `pixels_per_unit`. World
  gizmos set `units: camera.units()`; literals without
  `..Overlay::new(white)` add `units`.
- `Renderer::ui_size()` and `ui_pixel_scale()` (new): a target over 4096²
  scaled down laid the UI out too large and off the pointer; it now keeps
  the requested size. Lay out over `ui_size()` instead of
  `target_size() / ui_scale` and raster text at `ui_pixel_scale()`.
- Aseprite: `pingpong_reverse` tags parse as new
  `AseDirection::PingpongReverse` (was `Other`), and a frame past
  `meta.size` is new `AsepriteError::FrameOutsideSheet`; one-frame Hash
  exports and dotted titles now load. Add the variants to exhaustive
  matches; re-export sheets whose frames overrun.
- `Renderer::read_scene`, `capture_scene` and `CaptureError` are now
  native-only; in the browser they hung the tab. Browser games drop those
  calls.
- `fetch_runtime_bundle` (wasm) fetches assets relative to the manifest's
  directory (was `assets/` beside the page). Serve the manifest from the
  asset root, e.g. `fetch_runtime_bundle("assets/manifest.txt")`.
- `TextRenderer`: glyph pages are reused least recently used first, a
  glyph larger than a page gets its own (past the device limit, the upload
  fails with `TextureError::TooLarge`), and ring widths cap at
  `MAX_RING_WIDTH`. Call `end_frame` once per presented frame and log rather
  than unwrap page uploads.
- Fixes: UI layering of modals, popovers, panels and long draw lists;
  dropdown opening, closing and blocking; number-field drag, click and
  typing; line-edit clear layout; password clear zeroizes in place; splitter
  bounds; same-frame release and press; text laid out per exact size and
  nonpositive sizes; 2D light NaN and shadow caps; oversized swapchains,
  targets and sprite and shadow buffers; `DrawList::sort` with NaN `z`;
  `Overlay::circle` with a nonpositive radius. No game-code changes.

### sgl-3d

- `Scene::update_mist` and `update_effects` now return
  `Result<(), SceneError>`; oversized submissions to them and
  `update_fog_volumes` return `SceneError::DeviceLimit` and keep the
  previous content. Handle or `?` the result.
- Scene validation refuses invalid values that reached the GPU: material
  factors out of range or non-finite (new
  `SceneError::InvalidMaterialFactors`), non-finite normal, coat and bump
  scales, lightmap offsets and baked irradiance outside finite `0..=65504`,
  non-finite UVs, deformation weights whose sum overflows f32, anisotropic
  meshes whose tangents lie along their normals
  (`SceneError::MissingAnisotropyTangents`), `CompressedImage` sides above
  16384, and decal atlases past the device or 2 GiB (browser) and 8 GiB
  (native). Supply finite in-range values and non-parallel tangents;
  exhaustive `SceneError` matches add the variant.
  `Scene::set_baked_specular_probes` refuses a box whose extent overflows
  f32 with `ProbeError::InvalidProbe`; give it a finite extent.
- glTF import (`asset::load*`): malformed files panicked, aborted or loaded
  wrongly; malformed GLB headers, accessors, sparse accessors, images and
  KTX2 data, node cycles or multiple parents, singular joints, invalid
  material factors, repeated keyframe times, mismatched morph target counts,
  URIs outside the glTF's directory or through symlinks, and assets past the
  load budget now fail the load naming the element. Images and attributes it
  does not use (`TEXCOORD_2`+, `COLOR_1`+, `_NAME`) are skipped and listed as
  new `Ignored::Image` and `Ignored::Attribute`; `TEXCOORD_1` loads as
  `Vertex::lightmap_uv`; tiny uniform scales load; `EXT_materials_bump`
  defaults to 1. Buffer and image URIs must be embedded or relative regular
  files without `..` or symlinks, buffers must match `byteLength`, and
  PNG/JPEG sides must be at most 16384. Move referenced files beside the
  glTF or into its subdirectories, replace symlinks with files, split
  oversized images or assets, and re-export other files that now fail; add
  the arms to exhaustive `Ignored` matches; set `lightmap_uv` to `[0, 0]` on
  static models the irradiance bake does not chart.
- `Scene::add_shader` now refuses loops it cannot bound
  (`ShaderError::UnboundedLoop`) and declarations of WGSL built-in or
  SGL3D names (`ShaderError::NameTaken`). Rename such helpers (for example
  `my_smoothstep`); keep each loop counter's test and step in the loop's
  own test and update, start it at or below its limit, and set a nested
  `break if` loop's counter just before it. `MaterialVertex::tangent` docs now match behaviour:
  a vertex without tangents gets an arbitrary unit tangent, never zero.
- `diagnostics::read` is now native-only. Browser games drop the call.
- Changed look, no game-code changes: `ColorGrading` blends shadows (not
  highlights) with midtones, and negative `hue` wraps; `AutoExposure`
  ignores near-black pixels in its average and lands on target after long
  frames, and a non-finite or negative speed takes the default (use
  `f32::MAX`, not infinity, for instant); non-sRGB 8-bit outputs (and float
  outputs in the browser) are sRGB-encoded, no longer dark; SMAA runs after
  tone mapping; glow in fog looks dimmer; heat distortion under FSR2 is
  smaller; orthographic views shade with the camera's axis; lights reach
  surfaces facing them through the coat normal or any of their sphere;
  `Light::specular` governs coat and sheen dimming; blended `keeps_specular`
  materials fade diffuse transmission with coverage, and transmission dims
  beneath a sheen; fog froxels stay square.
- Fixes: ray-traced shadows lift ray origins off distant receivers;
  reflection (Velvet, Crystal, world-space) edge, history and step-cap
  fixes; dynamic GI history, ray budget and spacings past 170 m; fog, FSR2
  and exposure history resets; skinning, morph, deformation and anisotropy
  NaNs; local-light shadow slots and stale cached shadows; probe grids and
  captures; cull buffer, BLAS and pipeline growth past device limits. No
  game-code changes.

### sgl-post-fx

- WGSL `ProjectDirection` (`PostFX_Common`) takes the near plane's view Z as
  a fifth argument, so rays toward the camera no longer trace mirrored.
  WGSL calling it adds `g_Camera.fNearPlaneZ`.
- `ScreenSpaceReflectionAttribs::temporal_radiance_stability_factor`
  defaults to 0.95 (was 1.0, which left reflections dark after a reset).
  Set 1.0 explicitly to keep it; SGL3D already used 0.95.
- `ScreenSpaceReflection` reports a miss for a world normal whose squared
  length is more than 0.1 from 1, and caps `most_detailed_mip` at the top
  level. Games feeding SSR directly write unit world normals.
- Fixes: SSR half-resolution blocks, viewport edges, far-plane and NaN rays,
  off-screen history, mask-edge ghosting and step-cap streaks; TAA resize,
  first-frame, border and infinite-colour handling; `PostFXContext` frame
  description, camera buffer, half-precision depth toggle and frame index
  wrap. No game-code changes.

### sgl-input

- `Gamepad::name`, `is_pressed`, `value` and `Gamepads::gamepad` are now
  `#[must_use]`. Use or remove calls that discard them.
- Fixes: repeated presses, orphan releases and repeated `Connected` are
  dropped; macOS owner replacement keeps callbacks and background delivery,
  and input before an unplug is reported. No game-code changes.

### Packaging and examples

- Published packages no longer ship examples, tests, fixtures or unused
  vendored sources; `bytemuck`, `winit` and `pollster` are caret ranges, not
  exact pins. Distribution notices add NVIDIA Falcor's BSD-3-Clause notice
  and the vendored MIT OR Apache-2.0 Zstandard decoder; refresh the game's
  notices.
- `examples/direct-game` keeps one `Assets<Texture>` for
  `Renderer::white_texture`. Games that copied it pass their own texture
  cache.

## 0.4.0 — 2026-10-09

- Move every SGL crate to `0.4.0` together. Breaking: update `sgl-net`
  games, `FixedClock` callers, `SurfaceMaterial` and `Settings` literals
  and exhaustive `SceneError` matches as the entries below say.
- `sgl-net` `SendError::ReliableOverflow` → `SendError::WouldBlock`: a full
  reliable queue no longer disconnects the peer on any transport; the send is
  refused whole and may be retried. `Delivery::ReliableOrdered` →
  `Delivery::RELIABLE_ORDERED` (`Delivery::Reliable(Lane)`);
  `DisconnectReason::ReliableOverflow` → `InboundOverflow` (inbound only).
  `ClientIo::capacity(lane)` / `ServerIo::capacity(conn, lane)` are new
  required methods. `BrowserWebSocketConfig::reliable_buffered_bytes` now
  paces sends and must be at least `MAX_WEBSOCKET_FRAME_BYTES`;
  `ThreadedUdpServer::send` refuses connections it has not announced.
  Migration: rename the variants (exhaustive matches use
  `Delivery::Reliable(_)`); treat `WouldBlock` as "keep it and retry
  later", not as a lost connection; implement `capacity` on any
  `ServerIo`/`ClientIo` wrapper.
- `sgl-net` lanes: `RELIABLE_LANES` 1 → 4 independent lanes (each ordered on
  its own, unordered across lanes, own bounds, shared by weight). The new
  `ReliableConfig`/`LaneConfig` (`reliable` on `EndpointConfig`,
  `NativeWebSocketServerConfig`, `NativeWebSocketClientConfig` and
  `BrowserWebSocketConfig`; `memory_duplex_with`) replace
  `RELIABLE_OUTBOUND_*`, `RELIABLE_INBOUND_*`, `memory::MAX_RELIABLE_QUEUED`
  and `ThreadedUdpConfig::{reliable_queue_messages, reliable_queue_bytes,
  event_queue_messages}`, with the same values per lane (`DEFAULT_LANE_*`);
  `EndpointConfig::global_reliable_*_items` → `global_reliable_*_messages`,
  defaults 4,096 → 12,288 messages and 4 → 24 MiB.
  Migration: rename the constants and global fields, and move removed
  `ThreadedUdpConfig` bounds to `EndpointConfig::reliable.lanes[n]`; one-lane
  games change nothing else; others pick `Lane::new(n)` and set
  `config.reliable.lanes[n].weight`.
- `sgl-net` `MAX_RELIABLE_MESSAGE_BYTES` (64 KiB) →
  `ReliableConfig::max_message_bytes` (default
  `DEFAULT_RELIABLE_MESSAGE_BYTES`, 64 KiB; at most
  `RELIABLE_MESSAGE_BYTES_LIMIT`, 16 MiB); both ends must set the same cap,
  and `global_reliable_{outbound,inbound}_bytes` must be at least it. A lane
  holding no reliable bytes admits one message of any size up to the cap.
  Migration: use the constant or the configured value; delete game-side
  segmentation.
- `sgl-net` `Delivery::Unreliable(Lane)` (new): messages of at most
  `MAX_UNRELIABLE_BYTES` (1168) sent once, unordered, delivered at most
  once; a full send queue refuses with `WouldBlock`, and a receiver that is
  not polled drops its oldest unpolled ones. Migration: exhaustive
  `Delivery` matches add an arm.
- `sgl-net` wire format: UDP version 3 and WebSocket envelope version 2;
  old and new builds cannot connect, so rebuild servers and clients
  together and change the WebSocket subprotocol.
  `udp::MAX_RELIABLE_FRAGMENT_BYTES` 1168 → 1150; WebSocket frames carry at
  most `WEBSOCKET_FRAGMENT_BYTES` (16 KiB), so `MAX_WEBSOCKET_FRAME_BYTES`
  is 16,406 and an oversized frame is `ProtocolViolation` (was
  `Transport`). `Envelope` borrows its payload and gains
  `fragment: Fragment`, `encode_envelope(magic, &envelope)`, and
  `EnvelopeError` gains `InvalidLane`, `InvalidFlags` and `InvalidTotal`.
  `SimulatedConfig` gains `lane_loss_per_10k`. Migration: add
  `lane_loss_per_10k: [0; RELIABLE_LANES]` or `..Default::default()` to
  `SimulatedConfig` literals; direct envelope users pass an `Envelope`.
- `sgl-net` native WebSocket and UDP receivers: a lane past its `inbound_*`
  bounds (on UDP, or `EndpointConfig::global_reliable_inbound_messages`)
  stops taking messages until `poll` makes room, so the sender gets
  `WouldBlock` (was disconnected with `InboundOverflow`). A UDP sender
  waits while keepalives arrive; a native WebSocket sender still times out
  after its `timeout_ms`. No game-code changes needed; browser receivers
  still close on overflow, so size their `inbound_*` bounds for one poll
  interval.
- `sgl-net` UDP: a flush packs messages, latest state and acknowledgements
  into shared datagrams (was one item per datagram), so
  `max_packets_per_peer_flush` bounds datagrams, not messages. No game-code
  changes needed.
- `sgl-net` WebSocket: flushed latest state leaves ahead of lane frames
  flushed after it (was behind every released reliable frame), and a newer
  `send` no longer withholds it. No game-code changes needed.
- `sgl-core` `FixedClock`: accumulates exact `Duration` time (was `f32`
  seconds): `begin_frame` takes the frame's `Duration`, the `fixed_dt` field
  is now the `fixed_dt()` method beside `fixed_step()`, and `dropped_dt`
  reports unsimulated time. New opt-in `with_catch_up(hz, CatchUp { .. })`
  runs several steps per frame; `with_hz` and `new` keep one step per
  frame. Migration: pass the frame's elapsed `Duration` (e.g.
  `Duration::from_secs_f32(dt)` for an existing `f32` delta) and call
  `fixed_dt()`.
- `sgl-2d` `SpritePass::upload`: a handle already uploaded now has its pixels
  replaced (was ignored), and `SpritePass::replace` is public, so glyph pages
  from `TextRenderer::end_frame` update the existing pass. New
  `SpritePass::draw_stats` reports per-channel draws. No game-code changes
  required; games that upload unchanged textures every frame should upload
  only on change.
- `sgl-3d` programmable surfaces (new): `Scene::add_shader` takes a game's
  WGSL `material_vertex`, `material_surface` and `ShaderParams`, used by
  `SurfaceMaterial::shader` (`None` by default), with per-vertex data from
  `PreparedModel::with_shader_data`; blended materials may read
  `scene_volume_path`
  ([Programmable surfaces](crates/sgl-3d/README.md#programmable-surfaces)).
  Migration: `SurfaceMaterial` literals without `..` add `shader: None`;
  exhaustive `SceneError` matches add `UnknownShader`, `ShaderInUse`,
  `ShaderParameters`, `Shader`, `InvalidDisplacementBound` and
  `ShaderDataLength`.
- `sgl-3d` `Settings::volume_paths` (new, on by default; saved settings
  without it load on) and `Renderer::volume_paths_in_effect`. Migration: a
  `Settings` literal without `..` adds `volume_paths: true`.
- Docs: the [consumer guide](docs/README.md#physics) recommends custom
  arcade physics for most games and Rapier only where simulated physics is
  the game. No game-code changes needed.

## 0.3.0 — 2026-10-08

- Move every SGL crate to `0.3.0` together. Breaking: update `Asset`,
  material and `AlphaMode::Blend` literals and exhaustive `SceneError`
  matches as the entries below say.
- `sgl-3d` volumetric fog: a point or spot light's inverse square in the
  fog now adds the froxel's squared diagonal, so near lights no longer pulse
  or leave puffs, and their near fog is dimmer. Raise the light's
  `fog_energy` if its halo looks too faint; surfaces are unchanged.
- `sgl-3d` `Renderer::capture_specular_probe`: mip 0 held the one sample at
  each texel's centre, so a sub-texel emitter was stored at whole-texel
  energy or not at all; now each face renders at 2048 texels a side (or
  `face_size`, if larger) and is box-averaged to `face_size`. Captures take
  longer and use more memory, and fail in the browser before rendering. No
  game-code changes; re-export baked specular probes to pick it up; old
  exports still load.
- `sgl-3d` glTF loading: an unsupported extension listed only in
  `extensionsUsed` (an anisotropy texture's extensions included) no longer
  fails the load; it is left out and listed in the new `Asset::ignored`
  (`asset::Ignored`). Only unsupported `extensionsRequired` fail. Migration:
  `Asset` literals add `ignored: Vec::new()`; read `asset.ignored` after a
  load to see what was left out.
- `sgl-3d` occlusion maps, a load error before: one packed in the
  metallic-roughness image's red channel on `TEXCOORD_0` (ORM) occludes
  ambient diffuse, lightmap, atlas and ambient-cube diffuse and environment
  specular, taking the lesser of it and `Settings::ambient_occlusion`'s
  visibility where both apply; one in its own image or on another UV set
  loads unsampled (`Ignored::OcclusionMap`). New
  `asset::Material::occlusion_texture` (`None`), `occlusion_strength` (1)
  and `SurfaceMaterial::occlusion_strength`.
- `sgl-3d` `KHR_materials_ior` and `KHR_materials_specular`, a load error
  before: their factors set a dielectric's F0 and F90 through new `ior`
  (1.5), `specular` (1) and `specular_color` (`[1.; 3]`) on
  `asset::Material` and `SurfaceMaterial`; the defaults keep F0 0.04 and
  F90 1. Specular textures load unsampled (`Ignored::SpecularMap`).
  Migration: exhaustive material literals add the fields or take
  `..Default::default()`.
- `sgl-3d` `Scene::add_materials` and `set_material` refuse an IOR below 1,
  `specular` outside 0..=1 or a specular colour that is negative or not
  finite (`SceneError::InvalidReflectance`), and `occlusion_strength` outside
  0..=1 (`SceneError::InvalidOcclusion`). Migration: exhaustive matches on
  `SceneError` add both variants.
- `sgl-3d` binding tiers: a device with 48 or more sampled textures per
  shader stage takes `BindingTier::Extended`, any other `Basic`, reported by
  the new `Renderer::binding_tier()` (`graphics_device::BindingTier`). On
  `Basic`, devices with 21 to 47 (iOS GPUs older than Apple4, some Vulkan
  drivers), a material's anisotropy map gives way to its anisotropy
  factors, where it shaded before. No game-code change is needed; Metal on
  macOS, DX12 and Chrome's WebGPU take `Extended`.
- `sgl-3d` device floor: WebGPU's default limits (16 sampled textures per
  stage), down from 21, so a browser's default WebGPU adapter runs SGL3D,
  on `Basic`. On `Basic` baked light from a lightmap or irradiance atlas is
  non-directional and `Settings::dynamic_gi` resolves to `Off`, reported by
  the new `Renderer::dynamic_gi_in_effect(&settings)`; devices with 21 to
  47 lose both, where they ran before. No game-code change is needed.
- `sgl-3d` direct light: rough metals keep their multiply scattered energy
  under every light type (they were darker than under an even sky, and
  coloured metals shifted hue). Migration: none; lower any light intensity
  raised to make up for dark metals.
- `sgl-3d` environment specular reads a finer DFG table, so rough
  reflections change slightly. No game-code change.
- `sgl-3d` ray hits shade down to perceptual roughness 0.045, as raster does
  (was 0.0525). No game-code change.
- `sgl-3d` Fresnel is Schlick's fifth power (was Epic's exp2 fit), so
  highlights change slightly toward grazing. No game-code change.
- `sgl-3d` direct light on dielectrics: diffuse keeps only what the
  specular's Fresnel leaves, slightly darker. No game-code change.
- `sgl-3d` `LightShape::Point`/`Spot` `radius` and
  `DirectionalLight::angular_diameter` now also size specular highlights
  (they sized only ray ends), so the defaults widen highlights on smooth
  surfaces. Migration: none; a radius or angular diameter of 0 keeps a
  point's highlight and hardens ray-traced shadows.
- `sgl-3d` lightmaps, irradiance atlases, ambient cubes and the hemisphere
  fill light a material as the environment's diffuse light does (they took
  1 − F0 and 1, and no multiple scattering on metals). No game-code change
  or re-bake.
- `sgl-3d` multiple-scattered specular from the environment, hemisphere fill
  and volumes is darkened by `Settings::ambient_occlusion` and occlusion
  maps as specular (was left undarkened), so rough metals darken in creases.
  No game-code change.
- `sgl-3d` a lightmap's, atlas chart's or ambient cube's multiple-scattered
  specular takes the occlusion map as specular (was linearly), so occluded
  rough metals are slightly brighter. No game-code change.
- `sgl-3d` `DiagnosticTarget::Composite`'s alpha is 1 on opaque surfaces
  (was their base colour's alpha); the presented frame's is 1, as before.
  No game-code change.
- `sgl-3d` `KHR_materials_clearcoat` textures: load error → loaded and
  shaded on the `Extended` tier, the clearcoat normal map tilting the coat.
  New `asset::Material::clearcoat_texture`, `coat_roughness_texture`,
  `coat_normal_texture` (`None`), `coat_normal_scale` (1) and
  `SurfaceMaterial::coat_normal_scale`. Migration: literals add them or
  `..Default::default()`.
- `sgl-3d` `KHR_materials_iridescence`: unsupported → a thin film. New
  `asset::Material::iridescence` (0), `iridescence_ior` (1.3),
  `iridescence_thickness` (`[100., 400.]`), `iridescence_texture`,
  `iridescence_thickness_texture` (`None`), the first three on
  `SurfaceMaterial`. Migration: as above.
- `sgl-3d` `Scene::add_materials`/`set_material`: new
  `SceneError::InvalidIridescence` for a film outside its bounds.
  Migration: exhaustive `SceneError` matches add it.
- `sgl-3d` `Basic` tier: clearcoat and iridescence maps fall back to their
  factors, the clearcoat normal to the geometry normal. No game-code change.
- `sgl-3d` glTF: a primitive without `TEXCOORD_0` whose material has an
  emissive, normal or bump map loaded → refused. Migration: export UV0.
- `sgl-3d` `AlphaMode::Blend` gains `keeps_specular`: `true` fades only
  diffuse and emitted light by alpha, keeping reflections and highlights
  at full strength (glass); `false` is the previous behaviour, and glTF
  `BLEND` loads it. Migration: `Blend { receives_screen_space_reflections:
  r }` becomes `Blend { receives_screen_space_reflections: r,
  keeps_specular: false }`; patterns match `Blend { .. }`.
- `sgl-3d` transmission: new `transmission`, `thickness`,
  `attenuation_distance`, `attenuation_color` and `dispersion` on
  `SurfaceMaterial` and `asset::Material` (with `transmission_texture` and
  `thickness_texture`), loaded from `KHR_materials_transmission`,
  `KHR_materials_volume` and `KHR_materials_dispersion`, refract what lies
  behind on `Extended` and blend it through on `Basic`. Migration:
  exhaustive material literals take `..Default::default()` or the fields.
- `sgl-3d` a transmissive material draws with the blended surfaces whatever
  its alpha mode: it casts no shadow and rays pass through it. No game-code
  change.
- `sgl-3d` glTF files using those extensions, loaded opaque before, now load
  transmissive. No game-code change.
- `sgl-3d` `Scene::add_materials`/`set_material`: new
  `SceneError::InvalidTransmission` for values outside the extensions'
  bounds. Migration: exhaustive `SceneError` matches add it.
- `sgl-3d` `KHR_materials_sheen`, left out before: a sheen layer for cloth,
  through new `sheen_color` (`[0.; 3]`) and `sheen_roughness` (0) on
  `asset::Material` and `SurfaceMaterial` and `sheen_color_texture` and
  `sheen_roughness_texture` on `asset::Material`. Migration: exhaustive
  material literals add the fields or take `..Default::default()`.
- `sgl-3d` `KHR_materials_diffuse_transmission`, left out before: light
  passed through leaves, paper and, with `thickness`, volumes such as wax,
  through new
  `diffuse_transmission` (0) and `diffuse_transmission_color` (`[1.; 3]`)
  on `asset::Material` and `SurfaceMaterial` and
  `diffuse_transmission_texture` and `diffuse_transmission_color_texture`
  on `asset::Material`. Migration: as for the sheen.
- `sgl-3d` sheen and diffuse transmission maps bind on the `Extended`
  binding tier only; on `Basic` their factors apply alone. New
  `SceneError::InvalidSheen` and `InvalidDiffuseTransmission` refuse values
  outside glTF's ranges. No game-code change.

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
