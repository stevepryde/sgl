# SGL3D features

What SGL3D renders, the API to reach for, and where the detail is. The
settings a game chooses are listed in [settings](settings.md).

## Platforms

Native (Metal, Vulkan, DX12) and the browser (WASM + WebGPU) run the same
features, with two exceptions: FSR2 needs native-only device features, so the
browser runs TAA in its place and `Renderer::fsr2_error` says why, and
`Renderer::capture_specular_probe`, an authoring tool that blocks for its
readback, runs natively only. WebGL2 is not a target: SGL3D needs compute.
[Browser](../README.md#browser-wasm--webgpu).

## Content

- **glTF 2.0 meshes**: triangles with metallic/roughness, normal and
  bump maps, clearcoat, emission, unlit and `KHR_materials_anisotropy`.
  `asset::load` (a file), `load_slice` (bytes; the browser's way),
  `load_with_options` and `load_slice_with_options` (`LoadOptions`: an
  emissive strength cap, images the game supplies, which are never decoded,
  and a selection of mesh nodes for named rigid parts), or a
  procedural `asset::Asset` (`asset::Material::default()` is glTF's default
  material). Unsupported features return errors rather than
  partial models. [Limits](../README.md#asset-and-environment-limits).
- **Compressed material textures**: a material image is decoded RGBA8, whose
  mips SGL3D filters when it is added, or a BC7 mip chain uploaded as stored
  (`asset::Image::Compressed`, read from KTX2 with
  `CompressedImage::from_ktx2`; needs `TEXTURE_COMPRESSION_BC`). Games compress
  in their export step and supply the chains while a glTF loads
  (`LoadOptions::images`). Rays decode a compressed image's level 0 from its
  stored blocks.
  [Compressed material images](../README.md#compressed-material-images).
- **Skinned meshes and morph targets**: the loader imports skins (four
  influences per vertex), morph targets, the node hierarchy and animation
  clips as plain data (`deformation`, `Asset::rig`); the game samples and
  blends clips and gives each instance its joint matrices and morph weights
  every frame (`Scene::set_instance_deformation`, `Rig::joint_matrices`).
  SGL3D morphs and skins each changed instance once per frame in compute, and
  every pass draws the result: motion from the last submitted frame's pose,
  culling by skinned bounds, shadows in the cascades and the local-light
  atlas. A deforming instance moves and keeps its model; rays do not see it.
  Each `set_instance_deformation` call deforms it again and redraws its
  shadow faces, so skip it for unchanged poses. Rigid meshes stay baked at
  their nodes' rest transforms; clips move them only as named rigid parts.
  [Skinned meshes and morph targets](../README.md#skinned-meshes-and-morph-targets).
- **Scene**: starts empty; the game adds, replaces and removes materials,
  models, instances, lights, decals and environments between frames (`add_…`,
  `set_…`, `remove_…`), each named by the identity its addition returned. Buffers grow
  as content is added; content in use cannot be removed.
  [Lifecycle](../README.md#retained-scene-and-frame-lifecycle).
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
  [Alpha-masked and blended
  materials](../README.md#alpha-masked-and-blended-materials).
- **Blended receivers** (`AlphaMode::Blend { receives_screen_space_reflections:
  true }`): water or glass, with the normals the game animates, that
  receives the frame's screen-space reflections where it is the nearest
  receiver, and that TAA, FSR2 and motion blur reproject by its own motion.
  No refraction; one reflecting layer per pixel. Animate water with scrolling
  normal layers; a mesh replaced every frame with `Scene::set_model` rebuilds
  its ray BVH every frame. [Blended receivers](../README.md#blended-receivers).
- **Scrolling normal layers** (`asset::Material::normal_layers`,
  `SurfaceMaterial::normal_layers`, `NormalLayer`): a material's repeating
  normal map drawn as two layers, each with its velocity, scale and
  strength, moving with `FrameInput::elapsed_seconds` (an `f64`) in every
  view and ray: water's waves with no per-frame upload, on static instances
  too. Precise however long a session runs.
  [Scrolling normal layers](../README.md#scrolling-normal-layers).
- **Decals**: boxes that project the game's images onto the lit surfaces
  inside them, changing base colour and, with their maps, normal, roughness
  and metallic before lighting, so reflections and SSR see them
  (`Scene::add_decal_image`, `add_decal`, `Decal`). Godot's clustered decals:
  clustered with the lights, sampled from one atlas SGL3D packs; probe
  captures and world-space ray hits take them too. Unlit materials take
  none. Keep boxes shallow: everything inside one takes the decal.
  `Decal::new(image)` has Godot's decal defaults. The lit pipelines apply
  decals only while the scene holds one, so the first decal added compiles
  them anew.
  [Decals](../README.md#decals).
- **Material edits** at runtime: `Scene::set_material` (`SurfaceMaterial`,
  whose default is glTF's default material).
- **Visibility groups**: `FrameInput::visibility_mask` switches material
  groups on and off per frame, including in probe captures.
- **Culling and LOD**: frustum culling of mesh sections is automatic.
  `Scene::set_mesh_lods` registers authored coarser chunks
  ([spatial mesh LOD](../README.md#spatial-mesh-lod)).
- **Instanced draws**: automatic. Instances that draw the same mesh with the
  same material, face culling and mobility share one draw in every view, each
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
  `Light`, `LightShape`), any number, clustered on the CPU each frame so each
  pixel pays only for the lights that reach it. A `baked` light lights only
  receivers without baked lighting (moving instances, and static ones with
  no lightmap or assigned atlas chart), leaving the rest to the game's bake; `specular` scales its
  highlights (0 for a fixture already reflected as an emitter), and
  `fog_energy` its light in the volumetric fog (at most 0.001 leaves it out
  of the fog, which then skips its attenuation and shadow lookup), and
  `shadow_opacity` how dark its shadow is on surfaces and in the fog
  (Godot's; 1 by default). `Light::default()` and
  `DirectionalLight::default()` are Godot's light defaults, so a game sets
  only what differs (`..Default::default()`).
  `LightShape::Rect` is a one-sided panel or strip whose face is integrated
  by linearly transformed cosines: soft light and stretched highlights
  nearby, a spot of the same intensity far away. It costs more per pixel
  than a spot (one integral for diffuse, one more per specular lobe); the
  lit pipelines shade rectangles only while the scene holds one.
  [Point, spot and rectangle lights](../README.md#point-spot-and-rectangle-lights).
- **Local-light shadows**: lights with `casts_shadow` share one shadow atlas,
  sized and chosen by screen coverage; lights beyond its room are lit without
  a shadow (`Renderer::local_shadow_stats` counts them). Static casters are
  cached per face, so a frame redraws a light's shadow only where something
  moved in its range. [Local-light shadows](../README.md#local-light-shadows).
- **Image-based lighting**: environment panoramas with prefiltered atlases
  (`environment::EnvironmentMap`, `Scene::add_environment`);
  `FrameInput::environment` picks the frame's, `diffuse_environment` and
  `reflection_environment` (`EnvironmentLight`, unturned at intensity 1 by
  default) turn and scale its diffuse and specular light,
  and `backdrop` (`Backdrop`) shows its panorama or a colour behind
  everything.
- **Baked diffuse**, authored and baked by the game: a lightmap for chosen
  materials of static instances (`Scene::set_lightmap`) and a static
  irradiance atlas, BC6H/BC7-compressed for shipping
  (`set_compressed_static_irradiance_atlas`), both textures with optional
  directionality that the hardware filters, and ambient cubes for moving
  instances (`set_instance_baked_irradiance`). A fixture in the bake can also
  be a baked scene light, which lights moving instances live; its light then
  stays out of their ambient cubes.
- **Dynamic diffuse GI**: a volume of probes the game places
  (`Scene::set_dynamic_gi_volume`, `DynamicGiVolume`), kept up every frame
  by rays through the scene (Wicked Engine's DDGI): coloured bounce light
  from the frame's and the scene's lights, emitters and the sky, shadowed
  by rays where the lights cast shadows, on static surfaces without a bake
  and on moving instances in place of their ambient cubes and the frame's
  ambient, fading out over one spacing past the volume. `Settings::dynamic_gi` sets its rays.
  [Dynamic GI](../README.md#dynamic-diffuse-gi).
- **Baked specular probes**: parallax-corrected reflection cubes with blended
  influence boxes (`Scene::set_baked_specular_probes`), captured offline with
  `Renderer::capture_specular_probe`.
  [Probes](../README.md#baked-specular-probes).

## Reflections

Environment and probe specular always apply. On top of them:

- **Screen-space reflections** with two methods, Crystal and Velvet, which
  combine passes from DiligentFX, AMD FidelityFX, Godot, Wicked Engine and
  Bevy, over opaque surfaces and blended receivers. Crystal runs in SGL's
  `sgl-post-fx` effects library.
  [Reflections](../README.md#reflections) credits each source.
- **World-space reflections**: rays through a software BVH for moving objects
  up to 1000 m from the reflecting surface that screen-space reflections miss.

## Image quality and post-processing

- **Ambient occlusion**: XeGTAO after opaque shading, which occludes ambient
  diffuse and environment and probe reflections; turning it on draws no
  geometry again. [Ambient occlusion](../README.md#ambient-occlusion).
- **Antialiasing**: SGL's DiligentFX-derived TAA in `sgl-post-fx`, SMAA at
  SMAA 2.8's Low, Medium, High or Ultra preset, or AMD FSR2, which also
  upscales, with RCAS sharpening the game can turn off or set.
  [Temporal anti-aliasing](../README.md#temporal-anti-aliasing).
- **Anisotropic texture filtering** of materials, Off to 16×.
- **Scene resolution** scaling below the output size; the game's UI stays at
  full size.
- **Exposure** (`FrameInput::exposure`): fixed stops, or automatic exposure
  from a log-luminance histogram with a metering mask, a compensation curve,
  separate brightening and darkening speeds and limits. FSR2 and the tone map
  take the same exposure.
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
  highlights, then AgX tone mapping with Filament's look (`AgxLook`: none,
  punchy or golden) and a post-saturation. The output is always dithered
  against 8-bit banding.
- **Volumetric fog** (`FrameInput::fog`, `Fog`): Godot's froxel fog, while
  `FrameInput::atmosphere` is on (off by default, as Godot's fog). The
  frame's medium (density with height falloff, albedo, anisotropy, the
  share of ambient light it scatters, none by default as Godot's) and
  denser boxes of it (`Scene::update_fog_volumes`, `FogVolume`, by default
  Godot's), lit by the
  directional lights through their cascades, the clustered point, spot and
  rectangle lights through their shadows, each scaled by its `fog_energy`,
  and that share of the ambient light, so light shafts form where openings
  let a shadowed light through. Opaque surfaces, the sky, blended surfaces,
  glow and mist all fog from one volume, the sky by `Fog::sky_affect`
  (all of it by default, as Godot's). `Fog::length` sets its reach,
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

## Tools

- **GPU timing** per pass: `timing::GpuTiming`.
  [Pass timing](../README.md#gpu-pass-timing).
- **Geometry counts**: `Renderer::geometry_stats` and
  `geometry_stats_for_model`.
- **CPU rays** against scene triangles: `geometry::triangles` and
  `obstructed_distance`.
- **Diagnostics** feature: `Settings::diagnostics` turns layers off, runs the
  frame probe and captures the tone-mapped target; `Renderer::diagnostic_target`
  and `take_frame_probe_reports` return what they observed. Configuration only:
  no environment variables or files. Shipping builds leave it off.

## Not provided

Animation playback (sampling and blending clips is the game's). A dynamic
GI volume that scrolls with the player (installing a moved volume starts its
probes afresh), DLSS/MetalFX, hardware ray tracing, and GPU-driven/occlusion
culling are [planned](../../../specs/sgl3d.md#planned). Current world-space reflections
use software rays; current culling runs on the CPU. Compressed images
are BC7 only: transcoding Basis Universal (UASTC) for a device without BC is
not provided.

Also not provided: occlusion textures, tangent generation, runtime probe
capture, order-independent transparency and refraction.
