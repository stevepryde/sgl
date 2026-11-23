# SGL3D features

What SGL3D renders, the API to reach for, and where the detail is. The
settings a game chooses are listed in [settings](settings.md).

## Platforms

Native (Metal, Vulkan, DX12) and the browser (WASM + WebGPU) run the same
features, with these exceptions. FSR2 and FSR3 need texture atomics, which
WebGPU lacks, and subgroup operations and storage formats that wgpu's
browser backend does not request, so the browser runs TAA in their place
and `View::fsr_error` says why. Hardware ray tracing needs ray queries, which
WebGPU does not have: there, as on any device without them, rays trace the
software BVHs. WebGL2 is not a target: SGL3D needs compute.
[Browser](../README.md#browser-wasm--webgpu).

A device takes a binding tier by its sampled textures per shader stage,
which `graphics_device::limits` requests from the adapter:
`BindingTier::Extended` at 48 or more (Metal on macOS and Apple4 and
later, DX12, Chrome's upper tier), `Basic` below, down to WebGPU's default
16 (a browser's default adapter, iOS GPUs older than Apple4 at 23); a
Vulkan driver lands in either, by its `maxPerStageResources`.
`Renderer::binding_tier` reports it. On `Basic` baked light from a
lightmap or irradiance atlas is non-directional, dynamic GI is off
(`Renderer::dynamic_gi_in_effect`), so are volume paths
(`Renderer::volume_paths_in_effect`), a material's anisotropy, clearcoat,
iridescence, transmission, thickness, sheen and diffuse transmission maps
give way to their factors (a clearcoat normal map to the geometry normal),
in raster and rays alike, and transmissive surfaces blend the light behind
them through unrefracted. A probe captured on an
`Extended` device keeps what its maps and lights did there.
[Binding tiers](../README.md#binding-tiers).

## Content

- **glTF 2.0 meshes**: triangles, strips and fans, their attributes floats or
  `KHR_mesh_quantization` integers, with metallic/roughness, normal and
  bump maps (a bump usually shows stronger in world-space reflections and
  dynamic GI than to the camera: its strength is per pixel in raster, as
  three.js's, and per metre at ray hits, [D-45](../../../specs/decisions.md)), an
  occlusion map packed in the metallic-roughness image's red channel
  (ORM), which also shadows direct light (micro-shadowing,
  [D-66](../../../specs/decisions.md)), `KHR_materials_clearcoat` (its
  clearcoat, roughness and normal maps on the `Extended` binding tier),
  emission, unlit,
  `KHR_materials_anisotropy`, `KHR_materials_iridescence` (a thin film
  over the base, its maps on `Extended`), `KHR_materials_sheen` (cloth) and
  `KHR_materials_diffuse_transmission` (leaves and paper lit from behind),
  their maps on `Extended`, the `KHR_materials_ior` and
  `KHR_materials_specular` factors (a dielectric's F0: water's IOR 1.33
  gives 0.02), and `KHR_materials_transmission`, `KHR_materials_volume` and
  `KHR_materials_dispersion` (their maps on `Extended`). Every map lies on
  `TEXCOORD_0`; `TEXCOORD_1` loads as `Vertex::lightmap_uv`, the static
  irradiance atlas chart.
  `asset::load` (a file), `load_slice` (bytes; the browser's way),
  `load_with_options` and `load_slice_with_options` (`LoadOptions`: an
  independent checked source/decoded byte caps, an emissive strength cap,
  images the game supplies, which are never decoded,
  and a selection of mesh nodes for named rigid parts), or a
  procedural `asset::Asset` (`asset::Material::default()` is glTF's default
  material). File references are percent-decoded relative regular files
  beneath the glTF's directory, without parent segments or symlinks;
  base64 data URIs and GLB are supported. External buffers must match
  `byteLength`; encoded sources share a default 4 GiB native / 1 GiB browser
  cap. `LoadBudget::new` can explicitly change the source and decoded caps
  without weakening structural validation or changing GPU limits.
  [Loading details](../README.md#asset-and-environment-limits). A file that
  requires an extension SGL3D does not support fails to load; one it only uses is left out and listed in `Asset::ignored`,
  with occlusion maps in an image of their own and specular textures, which
  SGL3D does not sample yet, the images no sampled map uses, which are
  neither read nor decoded, and vertex attributes nothing reads
  (`TEXCOORD_2` on, `COLOR_1` on, `_NAME`). A tangent along its normal
  (within 0.02°) is absent except on an anisotropic material, which
  refuses it. Other
  unsupported features return errors rather
  than partial models. Sparse accessor indices must strictly increase and
  stay below the accessor's count; invalid ones fail the load.
  [Limits](../README.md#asset-and-environment-limits).
  Valid node trees have no depth limit within the asset load budget; roots
  and children load depth-first in declared order, preserved within each batch.
- **Compressed material textures**: a material image is decoded RGBA8, whose
  mips SGL3D filters when it is added, caller-prepared colour/data RGBA mip
  chains, or a BC7, ETC2 RGBA8 or ASTC 4×4 mip chain uploaded as stored
  (`asset::Image::Compressed`, read from KTX2 with
  `CompressedImage::from_ktx2`; request the corresponding adapter-supported
  `graphics_device::texture_compression_features`). Games compress
  in their export step and supply the chains while a glTF loads
  (`LoadOptions::images`). BC7 rays decode level zero from stored blocks;
  ETC2/ASTC ray sampling and decals need an explicit decoded companion.
  A no-ray scene can omit that companion for ordinary material textures.
  [Compressed material images](../README.md#compressed-material-images).
- **Skinned meshes and morph targets**: the loader imports skins (four
  influences per vertex), morph targets (up to 256 a mesh), the node
  hierarchy and animation clips as plain data (`deformation`, `Asset::rig`).
  Each skin influence needs finite nonnegative weights with a finite positive
  f32 sum. `animation::Pose` samples a clip at the game's time as glTF
  interpolates it (clamped or looped), crossfades poses and layers additive
  clips against a reference pose, reusing its buffers; the game
  owns clocks, transitions, state machines, masks and layer order, and gives
  each instance its joint matrices and morph weights every frame
  (`Pose::local_matrices`, `Rig::joint_matrices`,
  `Scene::set_instance_deformation`). SGL3D morphs
  and skins each changed instance once per frame in compute, and every pass
  draws the result: motion from the last submitted frame's pose,
  culling by skinned bounds, shadows in the cascades and the local-light
  atlas. A deforming instance moves and keeps its model; rays see it only
  with hardware ray tracing.
  Each `set_instance_deformation` call deforms it again and redraws its
  shadow faces, so skip it for unchanged poses. Rigid meshes stay baked at
  their nodes' rest transforms; clips move them only as named rigid parts.
  [Skinned meshes and morph targets](../README.md#skinned-meshes-and-morph-targets).
- **Scene**: starts empty; the game adds, replaces and removes materials,
  models, instances, lights, decals and environments between frames (`add_…`,
  `set_…`, `remove_…`), each named by the identity its addition returned.
  A model arrives as a `PreparedModel`, built from its meshes with
  `PreparedModel::new` on any thread, so geometry made at run time is
  prepared on the game's workers and the scene only places and copies it.
  Each vertex packs into 32 bytes: exact positions, normals and tangents
  within 0.01°, 16-bit UVs across each mesh's UV rectangle, vertex colours
  as 8-bit sRGB clamped to 0..1
  ([limits](../README.md#asset-and-environment-limits)).
  Buffers grow as content is added; content in use cannot be removed.
  [Lifecycle](../README.md#retained-scene-and-frame-lifecycle).
- **Views**: several images of one scene in a frame (a split screen, an
  inset, a rear view, editor viewports): a `View` each, listed in one
  `Renderer::render`, each with its own camera, settings, output, targets
  and temporal history. The first listed steers what the scene has one of
  (dynamic GI, ray tracing, material filtering). A view left out of a
  finished frame restarts its history.
  [Several views](README.md#several-views), `examples/views.rs`.
- **Instances**: model placements (`InstanceState`; `InstanceState::new(model)`
  is at the origin and shown in every view), static or moving. Static
  instances are what bakes and probe captures contain and write no motion;
  moving ones are posed each frame with `Scene::set_instance` (and
  `set_instance_deformation` when their model deforms) and write motion from
  the last submitted frame.
- **Render origin**: positions are `f32` in the scene's render frame, which
  `Scene::move_origin` moves by an exact delta, so a large or streamed world
  renders near the origin. Motion, shadow caches and histories carry across
  the move; nothing redraws and no history restarts.
  [Lifecycle](../README.md#retained-scene-and-frame-lifecycle).
- **Alpha modes** (`AlphaMode`, glTF `alphaMode`): masked materials are cut
  out below their cutoff in every view, shadow and ray; blended ones are lit,
  fogged and drawn back to front over the frame, writing no depth or motion
  unless marked to receive screen-space reflections, and casting no shadow.
  Alpha fades all of a blended surface's light, or with `keeps_specular`
  only its reflected and transmitted diffuse light and emission, its
  reflections staying full (glass).
  [Alpha-masked and blended
  materials](../README.md#alpha-masked-and-blended-materials).
- **Transmissive glass and water** (`SurfaceMaterial::transmission`,
  `thickness`, `attenuation_distance`, `attenuation_color`, `dispersion`;
  the IOR refracts): the light behind the surface passes through it,
  refracted, blurred by its roughness, dimmed beneath its sheen and coat,
  tinted by its colour and Beer-Lambert's attenuation across its thickness,
  each colour refracted
  apart by its dispersion; its reflections stay full. Drawn with the
  blended surfaces whatever its alpha mode, so it casts no shadow, takes no
  ambient occlusion, rays pass through it and probe captures leave it out;
  it shows the opaque frame behind it, not other blended or transmissive
  surfaces; a volume draws its front faces alone (water is not seen from
  below); decals tint it. Give glass its own material. On `Extended` the
  frame is copied each frame that shows one; on `Basic`, and in what
  screen-space reflections see of it, the light behind blends through
  unrefracted. [Transmissive glass and water](../README.md#transmissive-glass-and-water).
- **Blended receivers** (`AlphaMode::Blend { receives_screen_space_reflections:
  true, .. }`): water or glass, with the normals the game animates, that
  receives the frame's screen-space reflections where it is the nearest
  receiver, and that TAA, FSR and motion blur reproject by its own motion.
  One reflecting layer per pixel; a transmissive receiver refracts too. Move
  water's waves with a shader's vertex function and add their detail with
  scrolling normal layers; a mesh replaced every frame with
  `Scene::set_model` is prepared again, ray BVH included, every frame.
  [Blended receivers](../README.md#blended-receivers).
- **Scrolling normal layers** (`asset::Material::normal_layers`,
  `SurfaceMaterial::normal_layers`, `NormalLayer`): a material's repeating
  normal map drawn as two layers, each with its velocity, scale and
  strength, moving with `FrameInput::elapsed_seconds` (an `f64`) in every
  view and ray: water's waves with no per-frame upload, on static instances
  too. Precise however long a session runs. FSR is told where they move,
  opaque surfaces included, as AMD asks for animated textures.
  [Scrolling normal layers](../README.md#scrolling-normal-layers).
- **Programmable surfaces** (`Scene::add_shader`, `SurfaceMaterial::shader`
  through `Scene::set_material`): the game's WGSL vertex and surface
  functions for a material, run in every pass that rasterises it (G-buffer,
  lighting, receivers, blended draws, FSR's composition pass, captures,
  cascades and local-light faces), with motion and culling that follow
  them: waves on static water chunks, wind on vegetation, glass of varying
  thickness, with no per-frame upload. On `Extended`, a blended draw's
  surface function reads the opaque depth behind it (`scene_depth_behind`)
  and, for a material that bounds a volume, the length of its view ray
  inside that volume and what ends it (`scene_volume_path`: to the exit,
  an opaque object inside, or from the eye inside it), measured each frame
  from depth layers of its deformed geometry (`Settings::volume_paths`),
  for absorption by finite glass or liquid. SGL3D ships no water, wind or
  glass. [Programmable surfaces](../README.md#programmable-surfaces).
- **Decals**: boxes that project the game's images onto the lit surfaces
  inside them, changing base colour and, with their maps, normal, roughness
  and metallic before lighting, so reflections and SSR see them
  (`Scene::add_decal_image`, `add_decal`, `Decal`). Godot's clustered decals:
  clustered with the lights, sampled from one atlas SGL3D packs, which
  spot lights' cookies share; probe
  captures and world-space ray hits take them too. Unlit materials take
  none. Keep boxes shallow: everything inside one takes the decal.
  `Decal::new(image)` has Godot's decal defaults. The lit pipelines apply
  decals only while the scene holds one. After adding the first decal, draws
  compile any missing variants on first use. The atlas is limited to 2 GiB in the browser (one 8192-square
  image is too large) and 8 GiB natively.
  [Decals](../README.md#decals).
- **Material edits** at runtime: `Scene::set_material` (`SurfaceMaterial`,
  whose default is glTF's default material).
- **Visibility groups**: `FrameInput::visibility_mask` switches material
  groups on and off per frame, including in probe captures.
- **Culling and LOD**: automatic. The GPU builds the camera's opaque and
  masked draws and each directional cascade's every frame: frustum culling
  of each instance, then of its mesh's 128-triangle sections, one indirect
  draw per material, pose kind and positions slab whatever the instance
  count, on native and in the browser alike; a cascade draws an opaque
  material's sections whose triangles pair as quads a second, indexed
  way. A mesh holds at most 65,536 sections.
  `Scene::set_mesh_lods` registers up to 8 authored coarser chunks a mesh
  ([spatial mesh LOD](../README.md#spatial-mesh-lod)). While world-space
  reflections run, primary draws retain full detail to match their ray geometry;
  turning them off restores primary LOD selection. Coplanar surfaces of
  different draws have no defined winner at equal depth: give an overlay a
  depth offset or make it a decal. Two-phase occlusion culling of the
  camera's opaque and masked geometry is opt-in
  (`Settings::occlusion_culling`, [settings](settings.md)).
- **Instanced draws**: automatic. In the CPU-built lists (blended surfaces,
  local-light shadows, probe captures), instances that draw the same mesh
  with the same material, face culling and mobility share one draw, each
  keeping its own pose, motion and identity; deforming instances draw alone.
  [Visible work](../README.md#visible-work-and-pass-selection).

## Lighting and shadows

- **Directional lights**: up to two (`FrameInput::directional_lights`,
  `DirectionalLight`). The first that is on and has a `shadow`
  (`DirectionalShadow`: distance and cascade count;
  `DirectionalShadow::DEFAULT` for a `const`) casts cascaded shadows that
  SGL3D splits and fits from the camera: up to four 2048-texel cascades
  (1024 at the Low `Settings::shadow_quality`, which also halves the
  local-light atlas and takes one hard filter tap), stable
  while the camera moves, blended across their
  overlaps, filtered and biased as Bevy does, and cast by everything between
  the light and the view. A single-sided material casts from its front faces, a
  double-sided one from both. `fog_energy` scales its light in the
  volumetric fog and `shadow_opacity` how dark its shadow is. A hemisphere
  fill
  (`FrameInput::hemisphere_light`, `HemisphereLight`).
  [Frame lights and look](../README.md#frame-lights-and-look).
- **Point, spot and rectangle lights**: scene content (`Scene::add_light`,
  `Light`, `LightShape`), clustered on the CPU each frame so each pixel
  pays only for the lights that reach it, up to 8,192 lights and decals a
  cluster (a probe capture's cluster is the whole scene, a ray hit's the
  view, a dynamic GI probe ray's the volume). A `baked` light lights only
  receivers without baked lighting (moving instances, and static ones with
  no lightmap or assigned atlas chart), leaving the rest to the game's bake,
  and casts ray-traced shadow rays only at those receivers; `specular` scales its
  highlights (0 for a fixture already reflected as an emitter, which also
  lights the base whole beneath a coat or sheen), and
  `fog_energy` its light in the volumetric fog (at most 0.001 leaves it out
  of the fog, which then skips its attenuation and shadow lookup), and
  `shadow_opacity` how dark its shadow is on surfaces and in the fog
  (Godot's; 1 by default). `Light::default()` and
  `DirectionalLight::default()` are Godot's light defaults, with Wicked
  Engine's 2.5 cm light radius and the sun's angular diameter, so a game
  sets only what differs (`..Default::default()`). A point or spot light's
  `radius` and the directional light's `angular_diameter` size their
  highlights (Karis's representative point, within about 20% of the light's
  energy at any angle): a large bulb or the sun on water shows a broad
  highlight. They also soften the camera's shadows from the maps
  (`Settings::soft_shadows`): sharp where a caster meets its receiver, wider
  with the distance between them.
  `LightShape::Rect` is a one-sided panel or strip whose face is integrated
  by linearly transformed cosines: soft light and stretched highlights
  nearby, a spot of the same intensity far away. It costs more per pixel
  than a spot (one integral for diffuse, one more per specular lobe); the
  lit pipelines shade rectangles only while the scene holds one.
  A spot's `cookie` (`LightCookie`) projects an image (a window's panes, a
  gobo) from the decal atlas over the square twice its outer angle across,
  its top toward `up`, multiplying the light by the image's colour times
  its alpha (sampled as sRGB) on surfaces, reflections' and GI's ray hits
  and in the fog, whatever its shadow; a `baked` light's pattern belongs in
  the game's bake. The lit pipelines sample cookies only while the scene
  holds one.
  [Point, spot and rectangle lights](../README.md#point-spot-and-rectangle-lights).
- **Local-light shadows**: lights with `casts_shadow` share one shadow atlas,
  sized and chosen by screen coverage; lights beyond its room are lit without
  a shadow (`View::local_shadow_stats` counts them). Each view has its own
  atlas, placed for its camera and created when it first places a shadow. A closed or unrepresentably
  narrow spot cone remains valid scene content and releases its shadow slots
  until its projection is finite. Static casters are
  cached per face, so a frame redraws a light's shadow only where something
  moved or changed in its range, including where a casting shader's
  parameters change or, for a shader that reads the time, every frame the
  time advances. [Local-light shadows](../README.md#local-light-shadows).
- **Image-based lighting**: environment panoramas with prefiltered atlases
  (`environment::EnvironmentMap`, `Scene::add_environment`);
  `FrameInput::environment` picks the frame's (`FrameEnvironment::Map`), `diffuse_environment` and
  `reflection_environment` (`EnvironmentLight`, unturned at intensity 1 by
  default) turn and scale its diffuse and specular light,
  and `backdrop` (`Backdrop`) shows its panorama or a colour behind
  everything; turning its yaw moves the sky's motion vectors with it. Probe captures whose padded readback exceeds `max_buffer_size`
  return `ProbeError::InvalidDimensions` before rendering.
- **Procedural sky**: `FrameInput::environment = FrameEnvironment::Sky(Sky)`
  in place of a map: a physically based sky (Hillaire 2020) lit by the
  frame's directional lights, which are then their light above the air and
  reach every surface, shadow, the fog and the GI volume reddened by the air
  and off below the horizon; the backdrop shows the sky and the lights'
  discs. It lights the frame as an environment map does (diffuse and
  specular, the GI volume's misses, the fog's ambient share, probe captures),
  following a changing sky within 16 frames for reflections and 31 for
  diffuse light. Distant surfaces take the air's haze (aerial
  perspective, `Sky::aerial_perspective`, 1 by default, 0 none): negligible
  at tens of metres, building over kilometres to 32 km, on opaque and
  blended surfaces, glow and mist through the fog volume, whether or not
  `FrameInput::atmosphere` is on; not in probe captures or ray hits.
  `Sky` scales the air's densities, sets the ground's height and albedo and
  the discs. [Procedural sky](../README.md#procedural-sky).
- **Baked diffuse**, authored and baked by the game: a lightmap for chosen
  materials of static instances (`Scene::set_lightmap`) and a static
  irradiance atlas, BC6H/BC7-compressed for shipping
  (`set_compressed_static_irradiance_atlas`), both textures with optional
  directionality that the hardware filters (on the `Extended` binding tier;
  `Basic` reads none), and ambient cubes for moving
  instances (`set_instance_baked_irradiance`). A fixture in the bake can also
  be a baked scene light, which lights moving instances live; its light then
  stays out of their ambient cubes. Every indirect source (the environment,
  the hemisphere fill, these and both volumes) lights a material alike.
  Baked light adds to the frame's ambient (the environment's diffuse light
  and the hemisphere fill), which only the two volumes below replace: bake
  without the sky and the fill, or lower `FrameInput::hemisphere_light` and
  `diffuse_environment` where the bake holds them.
  While an atlas is installed, a static instance's vertex with a
  `lightmap_uv` (a glTF's `TEXCOORD_1`) samples it and takes no baked
  lights: set `lightmap_uv` to `[0, 0]` on models the bake does not chart,
  such as one exported with a second UV map for AO.
- **Irradiance volume**: diffuse light the game computes over a lattice of
  cells (`Scene::set_irradiance_volume`, `IrradianceVolume`; a port of
  Bevy's irradiance volume), such as a voxel world's propagated sky and
  block light at a metre, each cell an ambient cube of its own light and the
  sky's visibility from each face (`IrradianceCell`), written by region from a box prepared on any thread
  (`PreparedIrradianceRegion`, `Scene::write_irradiance_cells`) with no
  geometry or bake replaced, and scrolled with the camera by installing it
  at a new origin. Static and moving surfaces without a chart take it in
  place of the dynamic GI volume, ambient cubes and the frame's ambient,
  which its sky visibility lets through; the visibility darkens the sky's
  share of their environment specular too.
  [Irradiance volume](../README.md#irradiance-volume).
- **Dynamic diffuse GI** (on the `Extended` binding tier): a volume of
  probes the game places (`Scene::set_dynamic_gi_volume`,
  `DynamicGiVolume`), kept up by rays
  through the scene (Wicked Engine's DDGI), near probes every frame and far
  ones in turn, within a per-frame budget of rays: coloured bounce light
  from the frame's and the scene's lights, emitters and the sky, shadowed
  by rays where the lights cast shadows, on static surfaces without a bake
  and on moving instances in place of their ambient cubes and the frame's
  ambient, fading out over one spacing past the volume. The probes also
  record how much sky they see, which darkens the sky's share of those
  surfaces' environment specular in shade and enclosed spaces. A new placement
  starts its probes over a few frames, nearest the camera first; one moved
  by whole spacings scrolls, keeping the probes that stay. Probes inside
  geometry or behind walls light nothing and cost little; those with no
  surface near them light moving instances alone. A sparse turn keeps
  unobserved texels' light and depth history; fresh depth texels start open.
  A converged volume pauses
  until something it sees changes. A glowing fixture that a scene light
  stands for keeps its own light out of the probes
  (`asset::Material::emits_into_gi`, `SurfaceMaterial::emits_into_gi`
  false), so it lights surfaces once, through the light; it still blocks
  the probes' rays and shows in reflections.
  `Settings::dynamic_gi` sets its rays and budget.
  [Dynamic GI](../README.md#dynamic-diffuse-gi).
- **Baked specular probes**: parallax-corrected reflection cubes with blended
  influence boxes (`Scene::set_baked_specular_probes`), captured offline or
  while a level loads with `Renderer::capture_specular_probe_async`, which
  borrows a `View`'s shadow maps (natively also blocking,
  `capture_specular_probe`).
  [Probes](../README.md#baked-specular-probes).

## Reflections

Environment and probe specular always apply. On top of them:

- **Screen-space reflections** with two methods, Crystal and Velvet, which
  combine passes from DiligentFX, AMD FidelityFX, Godot, Wicked Engine and
  Bevy, over opaque surfaces and blended receivers. Crystal runs in SGL's
  `sgl-post-fx` effects library. Half-resolution Crystal traces an eligible
  surface in each quad, so rough or background neighbors do not suppress
  a mirror strip. At half resolution a Crystal mirror pixel
  in a complete 2×2 block reads its own ray; an unpaired final row or column
  clamps to the ray grid. Roughness widens the reconstruction. Crystal keeps
  valid surface history when the reflection's virtual point leaves the previous
  viewport, subject to its depth checks. A moving receiver or reflected
  object reduces the history weight where the earlier pixel no longer covers
  the reflection.
  [Reflections](../README.md#reflections) credits each source.
- **World-space reflections**: rays up to 1000 m from the reflecting
  surface where screen-space reflections miss, through a software BVH or,
  with hardware ray tracing, the scene's acceleration structures. They
  reach moving objects (`Moving`), or everything (`All`), which shows
  static surfaces as they stand rather than as their probes recorded them
  and is meant for hardware ray tracing.
- **Hardware ray tracing** (opt-in): on a device with ray queries and with
  `Settings::hardware_ray_tracing` on, world-space reflections' rays, the
  dynamic GI volume's and ray-traced shadows' trace the scene's
  acceleration structures, built
  on the frames that trace rays, masked models among them on Metal; masked
  models on Vulkan and DX12 and what the device cannot hold stay on the
  software BVHs. Skinned and morphed instances are seen by
  those rays only on this path, and not at all where the device cannot
  hold them. `Renderer::ray_tracing_in_effect` and
  `ray_tracing_error` report it.
  [Hardware ray tracing](../README.md#hardware-ray-tracing).
- **Ray-traced shadows** (opt-in, `Settings::ray_traced_shadows`, which
  needs hardware ray tracing): the camera's opaque surfaces take the
  shadows of the directional light and of up to fifteen casting local
  lights from rays instead of the maps, soft by the light's size (a point
  or spot light's `radius`, the directional light's `angular_diameter`),
  denoised by AMD's FidelityFX shadow denoiser for the first four (the
  directional light's alone at `Settings::ray_traced_shadow_quality` Low)
  and temporally blended for the rest, reaching beyond the directional
  shadow's distance; the fog, blended surfaces and reflections keep the
  maps, and without hardware ray tracing the maps shadow everything.
  `Renderer::ray_traced_shadows_in_effect` reports it.
  [Hardware ray tracing](../README.md#hardware-ray-tracing).
- **Contact shadows** (opt-in, `Settings::contact_shadows`: `Short`,
  `Medium` or `Long`): a screen-space march through the camera's depth
  toward the directional light with cascades, Bend Studio's screen-space
  shadows as Godot runs them, finds the small casters the cascades miss
  (chair legs, kerbs, a scooter's stand) and darkens what they stand on;
  the cascades' shadow and theirs combine at the light's shadow opacity.
  Opaque surfaces of a `perspective` camera only; blended surfaces, the
  fog and reflections keep the maps, local lights take none, and
  ray-traced shadows replace them where they run. A caster's outline
  against a surface well behind it casts none (Bend's `IgnoreEdgePixels`),
  so a floor seen near edge-on and the frame's border do not shadow
  themselves, and a caster only a few pixels wide casts a thinner shadow
  away from where it stands.
  [Settings](settings.md).

## Image quality and post-processing

Pipeline setup follows the enabled effects and scene content. First use of an
effect can still compile shaders. On native, the game can construct its view
and encode its first world frame on a rendering thread. A later loading
frame with the upcoming look and output path warms them for subsequent frames
on that view. [Frame workflow](../README.md#retained-scene-and-frame-lifecycle).

- **Ambient occlusion**: XeGTAO after opaque shading, which occludes ambient
  light (its diffuse share, and the specular multiple scattering a rough
  metal reflects most of) and environment and probe reflections, and the
  volumes' sky visibility occludes the sky's reflection beyond its reach;
  turning it on draws no geometry again. Spatial and AO-owned temporal filtering run
  before lighting composition in every antialiasing mode; depth motion keeps
  moving receivers eligible for history.
  [Ambient occlusion](../README.md#ambient-occlusion).
- **Screen-space indirect light**: opt-in diffuse bounce between nearby
  surfaces on screen, a lit wall onto the floor beside it, gathered at half
  resolution after ambient occlusion and lit one frame late as baked light;
  light whose source is off screen, hidden or facing away is absent.
  [Screen-space indirect light](../README.md#screen-space-indirect-light).
- **Antialiasing**: SGL's DiligentFX-derived TAA in `sgl-post-fx`, SMAA at
  SMAA 2.8's Low, Medium, High or Ultra preset, or AMD FSR2/FSR3.1 temporal upscaling (no frame generation).
  [Temporal anti-aliasing](../README.md#temporal-anti-aliasing).
- **Sharpening**: AMD's RCAS over the displayed image after tone mapping,
  once, under every antialiasing mode and spatial upscaling; the game turns
  it on or off and sets its strength. [Sharpening](../README.md#sharpening).
- **Anisotropic texture filtering** of materials, Off to 16×.
- **Scene resolution** scaling below the output size; the game's UI stays at
  full size.
- **Exposure** (`FrameInput::exposure`): fixed stops, or automatic exposure
  from a log-luminance histogram with a metering mask, a compensation curve,
  separate brightening and darkening speeds and limits. Near-black pixels
  position the outlier filter but do not lower the average. FSR and the tone map
  take the same exposure.
  [Exposure, bloom and colour grading](../README.md#exposure-bloom-and-colour-grading).
- **Local exposure** (`FrameInput::local_exposure`): large bright and dark
  regions drawn toward middle grey before the tone map, local detail kept,
  so sunlit and shaded areas stay readable; separate highlight and shadow
  contrasts, off by default.
  [Exposure, bloom and colour grading](../README.md#exposure-bloom-and-colour-grading).
- **Bloom** (`FrameInput::bloom`): energy-conserving, through a mip chain;
  bright light scatters, with no threshold.
- **Motion blur** (`Settings::motion_blur`, `FrameInput::motion_blur`): the
  camera's and moving instances' motion, over a shutter angle, after
  antialiasing. Wicked Engine's tile-max reconstruction filter: moving edges
  blur over what is behind them, and slower surfaces in front, such as what
  the camera follows, stay sharp. [Motion blur](../README.md#motion-blur).
- **Colour grading** (`FrameInput::color_grading`): white balance, hue, and
  saturation, contrast, gamma, gain and lift for shadows, midtones and
  highlights; adjacent sections crossfade at each end of the midtone range,
  so highlights do not affect the shadows-to-midtones transition. Then
  `Settings::tone_mapping`: Khronos PBR Neutral by default, ACES Filmic or
  AgX (with `AgxLook`: none, punchy or golden), and post-saturation. The output is always dithered
  against 8-bit banding.
- **Volumetric fog** (`FrameInput::fog`, `Fog`): Godot's froxel fog, while
  `FrameInput::atmosphere` is on (off by default, as Godot's fog). The
  frame's medium (density with height falloff, albedo, anisotropy, the
  share of ambient light it scatters, none by default as Godot's) and
  denser boxes of it (`Scene::update_fog_volumes`, `FogVolume`, by default
  Godot's; the first 1,024 that reach the frame), lit by the
  directional lights through their cascades, the clustered point, spot and
  rectangle lights through their shadows, each scaled by its `fog_energy`
  and a spot's by its cookie, and that share of the ambient light, so light
  shafts form where openings let a shadowed light through, or carry a
  cookie's pattern. Opaque surfaces, the sky, blended surfaces,
  glow and mist all fog from one volume (glow dimmed by it, adding none of
  its scattered light), the sky by `Fog::sky_affect`
  (all of it by default, as Godot's). Under a procedural sky the volume also
  carries its aerial perspective, the fog over the air; the sky takes the
  fog alone. `Fog::start` sets where the frame's medium begins (nearer
  is clear; fog volumes are unaffected), `Fog::length` its reach,
  `Settings::fog_quality` its resolution and `Settings::fog_filter` its
  blur; SGL3D spaces its slices and weights its history.
  [Volumetric fog](../README.md#volumetric-fog).
- **Mist**: positioned billboards (`Scene::update_mist`, their look and
  drift `FrameInput::mist`, which `Mist::default()` hides), while
  `FrameInput::atmosphere` is on.
- **Additive effects**: game-generated glow triangles with soft depth fades
  (`Scene::update_effects`, `effects::Glow`): uniform, tapered by a
  `GlowProfile`, or one-pixel lines (`GlowKind`).
  [Soft additive effects](../README.md#soft-additive-effects).
- **Heat shimmer**: screen distortion over game-supplied triangles
  (`Scene::update_heat_distortion`).
  [Heat shimmer](../README.md#bounded-heat-shimmer).

Fog volume, mist and glow updates return `Result<(), SceneError>`; handle or
propagate the result (`?`). A submission above the device's buffer limits
(`DeviceLimit`) keeps the previous content. Fog also obeys the storage binding
size, and record counts must fit `u32`.

## Tools

- **GPU timing** per pass: `timing::GpuTiming`.
  [Pass timing](../README.md#gpu-pass-timing).
- **Geometry counts**: `View::geometry_stats`, the camera's draws and
  triangles in the most recent completed frame, read back without blocking
  (`None` until one completes), and, with diagnostics,
  `View::geometry_stats_by_model` for several model lookups from one
  completed frame or `View::geometry_stats_for_model` for a single model.
- **CPU rays** against scene triangles: `geometry::triangles` and
  `obstructed_distance`.
- **Diagnostics** feature: `Settings::diagnostics` turns layers off (culling
  among them), runs the frame probe and captures the tone-mapped target;
  `View::diagnostic_target` (among its targets `Antialiased`, the linear
  HDR scene as antialiasing leaves it), `take_frame_probe_reports`,
  `diagnostic_draws` and `diagnostic_view_times`, and
  `Renderer::take_dynamic_gi_reports` (`Diagnostics::dynamic_gi`: the dynamic GI
  volume's probes, rays, BVH visits and budget stride, by frame) return what
  they observed; `diagnostics::read_async` reads a target back (natively
  also blocking, `diagnostics::read`). Configuration only:
  no environment variables or files. Shipping builds leave it off.

## Not provided

An animation player, mixer or state machine: `animation::Pose` samples and
blends clips at the times and weights the game gives it, and holds no clock.
DLSS/MetalFX are [planned](../../../specs/sgl3d.md#planned). The
directional cascades cull by frustum alone.

Compressed material images are BC7, ETC2 RGBA8 or ASTC 4×4 chains the game
prepares ([compressed images](../README.md#compressed-material-images));
transcoding Basis Universal (UASTC) KTX2 is not provided
([#857](https://github.com/stevepryde/sgl/issues/857)). Transmissive surfaces
refract on `Extended` and blend through unrefracted on `Basic`
([transmission](../README.md#transmissive-glass-and-water)). Specular probes
are captured between frames, offline or while a level loads:
`Renderer::capture_specular_probe_async` returns the GGX-prefiltered cube
(`SpecularProbeRadiance`), which the game stores or installs in a
`BakedSpecularProbe` with `Scene::set_baked_specular_probes`
([probes](../README.md#baked-specular-probes)); refreshing probes on the GPU
during play is not provided
([#867](https://github.com/stevepryde/sgl/issues/867)).

Also not provided: occlusion maps in an image of their own, specular
textures, tangent generation and order-independent transparency.
