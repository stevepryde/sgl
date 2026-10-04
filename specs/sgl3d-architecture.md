# SGL3D architecture

SGL3D follows the internal design established on 2026-09-30
([D-18](decisions.md), roadmap 23). The structural migration is implemented;
every change is held to the [rules](#rules) below.
[SGL3D](sgl3d.md) owns the consumer contract, rendering rules and roadmap;
this spec owns the shape of the code.

## Shape

Games use two objects.

- **`Scene`** is retained content: geometry, materials, textures, instances,
  lights, decals, environments, baked lighting and probes, with stable identities
  ([Scene content](#scene-content)). It holds the CPU state and the GPU buffers
  that mirror it, and nothing that depends on a camera, an output size or a
  quality setting.
- **`Renderer`** is the frame: views, stages, pipelines, targets, histories,
  settings and timing. It reads the `Scene` and does not change its content.

A `Scene` has one presenting `Renderer`. `Renderer::finish_frame(scene)`
commits a submitted frame: the `Scene` advances its instances' last-submitted
poses and the `Renderer` its histories (S3D-4).

The crate is six layers. Each may use the layers before it and none after it.

1. **content**: CPU data and loading: meshes, materials, images, light and
   probe descriptions, glTF and KTX2. No wgpu.
2. **shading**: the shared WGSL library and the Rust layouts that mirror it.
3. **scene**: `Scene`, its GPU buffers, change tracking and the ray-query
   structure built from its geometry.
4. **view**: a camera with its matrices, jitter and history, and what it sees:
   culled draw lists and clusters. The main camera, every shadow
   cascade and local-light shadow face, and every probe-capture face are
   views built by the same code. Clusters list the lights and decals that
   reach each part of a view. The types stages
   share live here: those for drawing scene geometry, and what the renderer
   lends them (the frame context, the effective configuration, sizes and shared
   targets, group 0's bindings, and a port's context that two stages share).
5. **stages**: one module per rendering feature.
6. **renderer**: `Renderer`. It selects and orders the stages from the
   effective configuration, owns what they share, and signals history resets.

The SGL effects library (`sgl-post-fx`) and the external AMD ports
(`sp-fidelity` and `sp-fidelity-wgpu` from crates.io) are separate crates
that know nothing of SGL3D. Adapters live
in SGL3D, in the stage that uses the library, or in `view` when two stages
share its context. `sgl-post-fx` owns its GPU effects and evolves for SGL3D
under RD-2; DiligentFX is its provenance, not a conformance contract.

## Scene content

A `Scene` starts empty. The game adds, edits and removes its content between
frames; nothing is fixed when the scene is created, and no capacity is
declared. Buffers grow as content is added, and removed content's records and
buffer ranges are reused, so bounded content keeps bounded resources. Content
that would exceed a device limit is refused with the typed error.

| Kind | What it is | What `set_…` replaces |
| --- | --- | --- |
| Material | Surface values and the textures they sample. Materials added together share their textures; a texture lives as long as a material that uses it. An image is decoded RGBA8, whose mips the scene filters, or a block-compressed chain uploaded as stored; one texture serves a channel that samples it as sRGB colour and one that samples it as data. | Its values. |
| Model | Geometry: an ordered list of meshes, each with vertices, indices, one material and what deforms it: a skin (each vertex's joint influences), morph targets, both or neither. A model with a deforming mesh deforms. | Its whole geometry, with any vertex and index counts, none included. |
| Instance | A model placed in the world: an `InstanceState`, and whether it is static or moving, chosen when it is added; an instance of a deforming model also has its deformation, its joint matrices and morph weights. | Its state; its deformation (`set_instance_deformation`). |
| Light | A point, spot or rectangle light: position, shape (a spot's direction and cone, a rectangle's facing, width axis and size), colour, intensity, range, whether it is baked, its specular scale and whether it casts a shadow. | Its description. |
| Decal image | An image decals project, kept as texels (a compressed image's level 0 decoded). The scene packs the images its decals use, in the colour space each map samples, into one atlas: a decal that brings one in that the atlas lacks places a new layout, and the next frame's prepare, or a probe capture, packs and uploads it once. | Nothing. |
| Decal | A box that projects images onto the lit surfaces inside it: its position, orientation and size, a base colour image, optional normal and metallic-roughness images, a colour, how much of the base colour it replaces, and its fades. | Its description. |
| Environment | An environment map. The frame input names the one that lights the frame; with none, or an ended identity, no environment does. | Nothing. |

Transient geometry (glow, heat, mist) and fog volumes have no identity: each
kind is replaced whole by its own operation. Baked lighting and probes are installed whole and
stay the game's to bake again when its static content changes.

**Identities.** Adding content returns its identity: a typed value
(`MaterialId`, `ModelId`, `InstanceId`, `LightId`, `DecalImageId`, `DecalId`,
`EnvironmentId`) holding an
index and a generation. The game never chooses an index, a slot or a key.
Removing content ends its identity for good: its index is reused under a new
generation, so new content never shows earlier content's geometry, record or
motion. An operation given an ended identity, or one another scene issued,
fails and changes nothing. The identity types are plain data declared in
`content`; only the scene issues their values.

**Operations.** Each kind has `add_…`, which returns the identity, `set_…`
where the table lists something to replace, and `remove_…`. Content that other
content uses (a material by a model; a model by an instance or as a level of
detail; a decal image by a decal) cannot be removed, and a model used as a level of detail cannot be
replaced: the operation fails and changes nothing. Replacing a model's
geometry clears the levels of detail registered on it. A loaded `Asset` is
added whole and returns the identities of its materials and of its model;
procedural geometry is added as a model whose meshes name materials already in
the scene. An asset's own indices do not outlive that call: material edits,
levels of detail, lightmap charts and ambient cubes name content by identity.
Every `Scene` operation that can fail returns the scene's typed error, never a
string or a boxed error.

**Instances.** `InstanceState` is one plain struct with named fields: the
model, the pose, `visible` (the main camera draws it) and `capture_visible`
(the other views show it: the shadow views of every light, rays and, for a
static instance, probe captures). `add_instance` and `set_instance` take it
and reading an instance returns it; the scene's motion history is private. An
instance is static or moving, and nothing else distinguishes one piece of
placed geometry from another:

- A **static** instance is expected to stay as it was added. It is what bakes
  and probe captures contain and what shadow slots cache; it takes baked
  diffuse light from its lightmap charts and writes no motion. Changing it in
  any way is a static edit.
- A **moving** instance is expected to be posed every frame. Nothing caches
  it: a view that shows it draws it every frame. It takes baked diffuse light
  from the ambient cube the game sets for it, and writes motion from its pose
  in the last submitted frame. It has no motion in a frame where that pose
  does not apply: it is new, its state names a different model, or it was not
  `visible` in the last submitted frame. There is no caller key that cuts
  motion; an instance that must not carry it across a change is removed and
  added again.

An instance of a deforming model is moving and keeps its model: the
operations that would make it static or name another model, or a deforming
model a level of detail, fail. Its deformation starts at the bind pose
(identity joint matrices, zero weights) and is posed like its pose: the game
sets it each frame, and its motion is measured from its deformation in the
last submitted frame under the same rule.

**Edits and frames.** The game edits the scene before `Renderer::render`, and
not again until that frame is finished or abandoned. A frame draws the edited
scene in every view, shadow and ray alike: an edit updates bounds, culling
hierarchies, object records and the ray source together. It uploads what it
changed and, apart from moving content when a buffer grows or the decal atlas
packs anew, nothing else. Its
uploads go through the queue and never through the frame's encoder, so
abandoning a frame loses none.

A static edit (adding, removing or changing a static instance, or replacing
the geometry of a model one uses) also records the world bounds it touched.
The scene keeps those bounds pending until `finish_frame` and may merge them
conservatively. A cache of static content marks what they reach as stale in
any frame that shows them to it, whether or not it redraws then, and a cache
that was not kept up in a frame starts over. Static-edit bounds are the
scene's part; what else makes a cache stale (its light, the visibility mask,
a material's caster values, another `Scene`) is its owner's. An abandoned
frame commits nothing: the next frame measures motion from the last submitted
frame and sees the same pending bounds.

## Frame

One frame, as the game sees it: edit the `Scene`;
`Renderer::resize(output size, scale, settings)`, which does nothing unless
they changed; `Renderer::render(scene, input, settings, output)` into the
game's encoder; submit; `Renderer::finish_frame(scene)`. `FrameInput` holds the
camera, the authored look and per-frame state (time, visibility, lights).
Player choices are `settings::Settings`, one value the game stores (S3D-6).
Authored look, such as exposure, bloom, colour grading or fog, is frame input,
not a setting.

Stages run in one order, written in one place in `renderer`:

1. **Prepare**: upload scene changes; build the views; cull; cluster
   lights and decals; then deform, in one compute pass before any pass draws scene
   geometry, the instances whose deformation changed since the last
   submitted frame.
2. **Shadows**: the local-light atlas and the directional cascades.
3. **Volumetric fog**: the camera's froxel volume, lit by the frame's lights
   through the shadows and integrated into the frame's one fog, before any
   draw that fogs. It needs no depth, as Godot updates its volumetric fog
   before its opaque pass.
4. **Opaque**: the sky, the G-buffer and lit colour (direct, baked, emitted and
   ambient light, the ambient diffuse also recorded apart, unoccluded), then
   ambient occlusion over the G-buffer's depth and normals. Where the device
   has the colour attachments for it, the G-buffer and lit colour are one
   fused pass, the faster form on the consumer's route; otherwise a G-buffer
   pass and a lighting pass at its depth write the same targets.
5. **Reflections**: ambient occlusion of the opaque surfaces' ambient diffuse,
   environment and probe specular, the screen-space method, world-space rays,
   and their one composition. Completion fogs the opaque surfaces and the sky.
6. **Transparent**: blended surfaces, additive effects and mist, each fogged
   where it lies, then distortion. It is drawn onto the composed frame and,
   while a screen-space method traces, into the reflection input before
   tracing, so reflections show it.
7. **Exposure**: the frame's one exposure, fixed or metered from the complete
   HDR frame at the render size, before both of its readers.
8. **Antialiasing**: TAA or FSR2, from render size to scene size. FSR2 reads
   the exposure.
9. **Motion blur**: the antialiased frame blurred along the geometry's
   motion, at the scene size, before bloom, SMAA and tone mapping, as Bevy
   and Wicked Engine run it after TAA.
10. **Post**: bloom, SMAA, then tone mapping with the exposure and colour
   grading, to the output.

A new feature takes a place in this list by editing it here. A probe capture
is a `Renderer` operation that runs prepare, shadows and opaque over its own
views.

A stage is one module with one struct. It owns its private pipelines, bind
groups, targets and history, and offers the renderer the same few operations:
create, resize, prepare, encode and reset history. It states what it reads,
what it writes, which settings it honours and its timing group. Stages meet
only through the shared contracts below and the values the renderer passes
between them; no stage imports another. What two stages share (a port's
context, the geometry pipeline cache, shared targets, group 0) is owned by the
renderer and lent to them.

## Shared contracts

Each has one definition, which every producer and consumer uses.

| Contract | Definition |
| --- | --- |
| Conventions | Units, axes, depth and colour are S3D-3. |
| View and frame data | `View`, one per view (matrices, previous matrices, jitter, eye, viewport), and `Frame`, one per frame (time, the directional lights with the shadowed light's cascades, the hemisphere fill, the environment's diffuse lighting, reflection sky and backdrop, the fog volume's slicing and whether the frame has fog, mist, the visibility mask, and the scene's baked-lighting constants: the atlas scale and the lightmap's chart transform), declared with their flag bits in `shading::uniforms`. Named fields; flags are integers with named bits. The renderer packs `Frame` from `FrameInput`'s typed values; no GPU layout is public. |
| Bind groups | For pipelines that draw scene geometry through the shading library. Group 0 has three layouts: lit (view and frame data, lights, decals and the atlas their images are packed in, clusters, shadows, environment, probes, lookup tables and the fog volume), unlit (view and frame data, the frame's environment with its backdrop and the fog volume, for the sky, additive effects and mist) and shadow (view and frame data). The fog volume and its sampler are visible to fragment stages only. A view that renders into one of those binds a neutral stand-in for it; ray hits bind the lit layout with their light and decal lists, the local-light atlas's static layers and, as probe captures do, the installed probes. Group 1: the object records, one storage buffer bound whole that geometry passes read, and the geometry buffers ray queries read. Group 2: material. Group 3: the stage's own. Ports, full-screen passes and the deform stage lay out their own. Four groups is the limit. Lit group 0 and group 1 bind 8 storage buffers to a fragment stage, wgpu's default limit and S3D-1's floor: `graphics_device::limits` requests the adapter's `max_storage_buffers_per_shader_stage`, and the change that adds another states the floor it needs in S3D-1 or folds two buffers into one (the probe collection holds its world grid; the decals' indices share the clusters' lists). Lit group 0 and a material bind 17 sampled textures to a fragment stage, the floor S3D-1 states, and lit group 0 and the world-space trace's own targets 16 to a compute stage, where the fog volume does not count: `graphics_device::limits` requests the adapter's, and the change that adds another states the floor it needs in S3D-1 or folds two textures into one (one lookup-table texture holds the rectangle lights' fit and the DFG table). |
| Layout mirroring | A struct shared between Rust and WGSL is declared once in each, side by side in `shading`. A test compares the Rust layout with naga's layout of the composed WGSL. A vertex buffer's layout is derived once from the Rust type it holds, and a test compares it with naga's inputs of the vertex entry points that read it. |
| G-buffer | What the opaque stage records for later stages, including the ambient diffuse within lit colour before occlusion. One WGSL module defines its targets and encodings with `encode` and `decode`; a port converts at its adapter. |
| Surface shading | One evaluated `Surface` and one set of functions for direct, environment and baked light, and one for the decals that change a lit surface before it is lit. Raster shading, probe captures and ray hits call the same functions (S3D-5). |
| Lights and shadows | One light record, at its identity's index in the scene's light buffer, and one accessor for the lights and decals that reach a point: the cluster in a camera view, culled lists elsewhere, which are a grid of one cluster. One writer packs every view's clusters; each list holds its live lights, then its baked ones, then its decals. A point or spot light's shading reads its record's first four rows; a rectangle's also reads its last, and `surface_direct_light` integrates its face by linearly transformed cosines, from lit group 0's table, in one loop over its lobes. A light's shadow record, at the same index in a buffer the shadow stage writes each frame, places its faces in the local-light atlas; a light without one is unshadowed. One sampling function per shadow kind, and one set of filters and receiver bias for every 2D shadow map, in `shading::shadow_sampling`: Bevy's Castano '13 kernel, its Jimenez '14 spiral where temporal antialiasing resolves it and its one hardware 2×2 tap for the fog, whose reprojection resolves it, chosen by what receives the shadow (a capture's or ray hit's surface, the camera's surface or the fog), and its normal offset scaled by the map's texel size plus a depth offset toward the light. Each kernel tap is clamped to the map's rectangle in its texture, as Wicked Engine clamps to a light's atlas rectangle. The directional cascades and the local-light atlas use them. Every shadow kind culls casters as the camera does, as Bevy's shadow pipelines do and its bias assumes: a single-sided material casts from its front faces, a double-sided one from both. |
| Material records | A material's values reach the GPU as one record, `Material` in `shading/material.wgsl`, mirrored by `MaterialUniform`: named fields, and flags as integer `MATERIAL_*` bits (unlit, double-sided, the maps it was added with, its alpha mode). The scene packs it from the public typed `SurfaceMaterial` (named fields, `bool`s and the `AlphaMode` enum) and from the maps the material was added with, which no edit changes; group 2 binds it and the ray source holds the same record. No GPU layout is public. |
| Scene records | An instance has one object record, declared in `shading::uniforms`, at its identity's index in the scene's object buffer, a storage buffer. Its flags are named bits; static or moving is one of them, never a range of indices. A deforming instance's record names its deformed vertices this frame and its positions in the last submitted frame. That index is the source identity the G-buffer stores and a ray hit reports; it names an instance within one frame only. Each instance of a draw reaches its record through its draw instance (`shading::vertex::DrawInstance`, a vertex buffer stepped per instance): the record's index and the drawn mesh's record in the ray source, as Bevy's batched draws reach each instance's `MeshUniform` from its instance index. A fragment reads the record at the source identity it carries. Instancing renumbers no instance. |
| Ray source | Each material, texture and model owns ranges of the ray buffers (its record; its level-0 texels as RGBA8, a block-compressed image's decoded as raster samples them; its vertices, indices and BVH; a deforming model's influences and morph targets), and each deforming instance its joint matrices, morph weights and deformed vertices (`shading::deformation`), written when it is added or replaced and freed for reuse when it is removed. The instance list has one entry for each capture-visible instance that does not deform, with its index and flags, so a hit names the instance raster names. |
| Draw lists | One builder turns a scene and a view into instanced draws. A view's population is a filter over instances by their flags (static, `visible`, `capture_visible`) and over materials by the visibility mask and alpha mode, not a walk of named collections. Blended materials are the camera's blended population alone, sorted back to front. Each instance is culled and selects its level of detail on its own; its draws of one mesh then merge with other instances' into one instanced draw per index range when they share geometry (model and mesh, or a deforming instance's own, as that instance deforms it), material, pipeline variant and mobility and draw the same ranges, as Bevy batches its phases: opaque, masked, capture and caster populations wherever they are, in bins ordered by their model's first instance and mesh, so each instance's meshes keep their order; blended ones only where adjacent in their sorted order. A batch names its instances by index, its geometry by model and mesh, and its material by identity. A deforming instance is culled by its deformed bounds and draws no level of detail. Every list of a frame, or of a probe capture, appends its draw instances to one buffer, which the renderer uploads once every list is built and before any pass draws, as Bevy writes one batched instance buffer for every view. Every geometry pass (G-buffer, lighting, shadow, capture) draws from a draw list; none walks the scene. Draw statistics count the draws and triangles of static and moving instances; a draw holds one mobility. |
| Geometry pipelines | One cache keyed by pass and by what the material and instance require (face culling; the alpha mode: opaque, masked or blended; and for pulled passes whether the instance deforms), not a field per variant, and by whether the scene holds a rectangle light. A masked material's pipelines discard the texels it cuts out, so opaque ones keep early depth; masked, blended and deformed pipelines are prepared once the scene holds such content. The scene's rectangle lights specialise the lit passes too: the lit passes and the world-space reflection trace shade rectangles (`rect_lights_enabled`) only while it does, as Godot specialises its clustered pass on `cluster_has_area_light`, so a scene without them pays nothing for their shading. |
| Sizes | Render size up to antialiasing, scene size after it, output size at presentation. Defined once by the renderer. |
| History | A stage owns its history. The renderer issues one reset for `FrameInput::camera_cut`, a `Renderer::resize` that changed the targets, or a different `Scene`. Content edits and lighting changes restart no history: each history rejects what changed by reprojection and clamping, as its upstream does; the scene's change tracking rebuilds bindings and instance motion and reports static edits to caches of static content ([Scene content](#scene-content)), nothing more. |
| Settings | The renderer resolves requested settings into one effective configuration per frame. Stages read only that, and report why a choice could not run. |
| Timing | Every pass belongs to its stage's timing group. |
| Diagnostics | Behind the `diagnostics` feature. Switches are `Settings::diagnostics`, resolved into the effective configuration, never environment variables; observations return to the game, and the library writes no files. |

WGSL is composed from named modules by one function, `shading::compose`: each
module declares the modules it uses, and a program is their concatenation in
dependency order, each once. The layout test also parses and validates every
composed program. A shader file holds its entry points and its stage's own
code; it does not redeclare a struct, binding or function another module owns.

## Designs that span stages

- **Lights.** The `Scene` holds point, spot and rectangle lights as
  [scene content](#scene-content); directional lights and the hemisphere fill
  are typed frame input. Each scene light says whether it is baked, in which case it
  lights only receivers without baked lighting, as Godot's `BAKE_STATIC`
  lights skip only lightmapped meshes: moving instances, and static receivers
  with no baked map because baked lighting is off, or their material is not
  lightmapped and either no irradiance atlas is installed or their lightmap
  UV is unassigned ((0,0) or negative). A chart's black outside its cropped
  bounds is part of its bake. Baked diffuse makes that determination once,
  per receiver and never from where a fragment falls in its chart, and the
  light loop and any shadow that follows a baked light use it. Each scene
  light carries a specular scale so a fixture already visible as an emitter
  is not highlighted twice. Prepare clusters the lights for the main view on the
  CPU each frame (Bevy's clustered forward grid); a rectangle reaches the
  clusters in front of its face within its range, the spot test at a right
  angle. Captures and ray hits use a
  culled list: a capture every light that is on, and ray hits the lights that
  reach the camera's view, as Wicked Engine's do.
- **Decals.** The `Scene` holds decals and the images they project as
  [scene content](#scene-content): each decal's record at its identity's
  index in the scene's decal buffer, and its images packed into one atlas of
  linear texels with mips, as Godot packs its decal atlas. Decals are
  clustered with the lights, each as the sphere about its box, as Bevy
  clusters its decals: the camera's clusters, every decal in a probe
  capture's list, and the decals that reach the camera's view in the ray
  hits' list. Godot's clustered decals change a lit surface's base colour,
  normal, roughness and metallic in the shading library, before it is lit, so
  every geometry pass (the G-buffer, lighting and fused passes, probe
  captures and blended surfaces) and ray hits see them the same way, and
  reflections read them from the G-buffer. Raster samples the atlas along
  the position's screen derivatives; a ray hit samples its first level, as
  its material textures. Unlit materials take no decals.
- **Shadows.** Local lights share one 2D depth atlas. The casting lights
  that reach the camera's view are placed by screen coverage, as Godot's
  quadrant atlas places them; lights without room are unshadowed and
  counted. A point light and a spot wider than a cube face have six cube
  faces, of which a spot draws those its cone reaches; a narrower spot one.
  A rectangle has a point shadow from its centre, as Godot shadows area
  lights, drawn in the cube faces that see the half-space in front of it. Each face caches its static instances in a
  static layer, in a second atlas of the same layout. A frame copies a face's
  layer and draws its moving instances over it only when they entered, left,
  moved or deformed; it redraws the layer when the scene's static-edit bounds, the
  visibility mask or a material's caster values make it stale, and draws
  every caster of a light that moved. What a frame draws becomes reusable at
  `finish_frame`. The camera's surfaces sample the frame's atlas; probe
  captures and ray hits, which show static content, sample the static
  layers. The directional
  light that casts a shadow has cascades that SGL3D fits from the camera
  (Bevy's constant-diameter, texel-snapped fit), each a view with its own
  draw list in one layer of a depth array. A cascade's casters are culled
  without its near plane and drawn with unclipped depth (emulated where the
  device lacks `DEPTH_CLIP_CONTROL`), so a caster between the light and the
  cascade still casts. The camera's surfaces take the cascade at their view
  depth and blend into the next across the overlap; a probe capture fits
  its own cascades about its centre, and its surfaces and ray hits take the
  first cascade that holds them. Shadow views render through the common
  draw-list path.
- **Opaque and masked surfaces.** Direct, baked and ambient light are computed
  in the forward pass, never from the G-buffer; environment specular and
  reflections are computed from it. Ambient occlusion is applied after the
  forward pass, by source completion, to the ambient diffuse (environment
  diffuse and the hemisphere fill) that the pass records apart, as completion
  already occludes environment specular. Bevy, Godot, Filament and Wicked
  occlude only indirect light by default (Bevy 9d12036
  `pbr_fragment.wesl:538-548`, Godot b130438
  `scene_forward_clustered.glsl:2100-2267`, Filament ef1a133
  `surface_light_indirect.fs:780-812`, Wicked 4323a33
  `lightingHF.hlsli:49-50`), an additive term, so occluding it after shading
  equals occluding it in shading; Bevy's deferred path does it in a screen
  pass over its G-buffer (`deferred_lighting.wesl:65-77`). Their forward paths
  occlude while shading, after a depth prepass; SGL3D draws none, since on the
  consumer's route a prepass costs more than it saves (#393). This placement
  rules out albedo-tinted diffuse multi-bounce (Bevy's `ssao_multibounce`,
  Godot's `MULTI_BOUNCE_OCCLUSION_ENABLED`, Filament's `multiBounceAO`) unless
  albedo is added to the G-buffer; SGL3D never applied it. A masked
  material is opaque with a discard, applied in every geometry pass, in
  shadows and at ray hits: Bevy's `alpha_discard` and its shadow casters'
  (`MAY_DISCARD`), after a fragment's last derivative. The portable ray
  traversal's one acceptance predicate rejects cut-out texels, so nearest,
  any-hit and visibility queries agree.
- **Blended surfaces.** Sorted, in the transparent stage, writing no depth,
  motion or G-buffer. They use the same lights, shadows and environment through
  the shading library: the camera's lit group 0 (its clusters, the frame's
  shadows and the installed probes), with the probe and sky specular captures
  and ray hits add, and fog themselves as the stage's other draws do. The
  camera's blended population is sorted back to front by the view depth of
  each drawn mesh's bounds centre, as Bevy's `Transparent3d` phase sorts, and
  drawn before additive effects and mist, by a pass that tests the opaque
  depth without writing it and, while FSR2 runs, writes its reactive mask
  (alpha, at most 0.9, as AMD documents) and its transparency and composition
  mask (alpha, as AMD's sample writes for translucency). They cast no shadow
  and rays pass through them, as Godot leaves alpha-pass materials out of its
  shadow passes; probe captures, which run the opaque stage, do not show
  them.
- **Reflections.** Environment and probe specular always apply: source
  completion adds them to the main view's opaque surfaces from probes culled
  per screen tile, and captures, ray hits and blended surfaces add them in
  shading from the probes a world grid, built when the probes are installed,
  lists for their cell.
  While ambient occlusion runs, completion first takes the share of each
  surface's ambient diffuse that its ambient visibility hides out of the
  opaque colour, and occludes the specular it adds by the same visibility.
  Captures and ray hits have no ambient occlusion and keep their ambient
  diffuse whole. A
  screen-space method returns premultiplied radiance and confidence
  ([D-17](decisions.md)); world-space rays fill its misses; one composition
  combines them, so another method plugs in beside the existing ones.
- **Deformation.** Skinned and morphed positions reach every geometry pass the
  same way, with the previous frame's positions for motion. Prepare's deform
  stage morphs and skins each deforming instance whose deformation changed
  since the last submitted frame once, in one compute pass, into its own
  vertices in the ray source (Bevy's skinning and morph math, run once per
  frame as Wicked Engine's `skinningCS` runs it, chosen over skinning in
  every pass's vertex shader by measurement, #347): its positions, in the
  slot that does not hold the last submitted frame's, and its normals and
  tangents. Pulled passes read them through the object record, with the
  other slot's positions for motion; casters, which read positions from a
  vertex buffer for every instance, read its slot. Culling uses each mesh's
  skinned bounds (Bevy's `SkinnedMeshBounds`, grown by the weighted morph
  displacements). A deforming instance is moving, so no static layer or
  probe capture holds it, and scene rays do not see it, as Bevy's ray-traced
  scene leaves out meshes with joints.
- **Fog.** One participating medium per frame (`FrameInput::fog`), with the
  scene's fog volumes added where they lie (Godot's box `FogVolume`s), fills
  a froxel volume over the camera's frustum, as Godot's volumetric fog does
  (after Hillaire 2015). The fog stage lights each froxel through the shared
  lighting: the directional lights with the shared cascade sampling, the
  camera's clustered lights (baked ones too) through their records and the
  shared local-shadow sampling, each froxel a shadow receiver with no side,
  and the ambient fill; blends it with its
  reprojection into the stage's last volume; and integrates each column
  along its view ray. The integrated volume is the frame's one fog: group 0
  lends it, with its slicing in `Frame`, to the draws that fog themselves
  (blended surfaces, glow and mist) through `shading::fog`, and source
  completion samples it for the opaque surfaces, the sky and the incident
  radiance, as Godot's forward pass samples its volume for every material;
  composition scales reflections by its transmittance. No pass fogs the
  composed frame, and probe captures and ray hits have none.
- **Temporal.** Geometry writes unjittered motion. Jitter is applied to the
  projection by the antialiasing in effect, and every history resets together.
  Motion blur reads the same motion and depth after antialiasing, so it
  blurs the camera's and moving instances' motion alike.
- **Exposure.** Lighting stays linear HDR to the tone map. One exposure value,
  a 1×1 texture the exposure stage writes and the renderer passes on, feeds
  FSR2 and the tone map. Automatic exposure keeps its adapted correction as
  history, which takes its target when history restarts and when automatic
  exposure follows a fixed one.

## Rules

1. **AR-1 — One direction.** Dependencies follow the layer order, and a stage
   never reaches into another stage. Cargo checks crate boundaries
   ([testing](testing.md) 6); inside `sgl-3d`, Rust visibility cannot express
   the layer order, so AR-10's review holds it. A layer becomes its own crate
   when a consumer needs it alone or review fails to hold its boundary.
2. **AR-2 — One owner.** Every layout, encoding, formula, constant and binding
   has one definition: one per language where Rust mirrors WGSL, tied by the
   layout test. A second copy is a defect, including a struct redeclared in
   another shader. A port's own upstream helpers are not copies.
3. **AR-3 — One pipeline.** A feature takes a place in the stage order; it does
   not build a path around it. A second implementation of a stage exists only
   as a setting with a real trade-off (S3D-6) or for a benefit measured on the
   consumer's route, and it writes the same targets under the same contract.
4. **AR-4 — Thin renderer.** `renderer` holds no pipelines, shaders or
   per-feature logic. It selects and orders stages and lends them what they
   share.
5. **AR-5 — Typed boundaries.** Named fields and enums. No positional option
   packs, no floats as flags, no environment variables steering library
   behaviour.
6. **AR-6 — No game in the library.** S3D-2 covers names, constants and special
   cases too. A mechanism only one game could want lives in that game.
7. **AR-7 — Small public surface.** Games use `Scene`, `Renderer` and the plain
   data they take and return. Everything else is private, and an entry point
   becomes public when a consumer needs it. The values S3D-6 makes settable,
   and the defaults or constructors that build them, are part of that plain
   data whether or not a game uses them.
8. **AR-8 — Cohesion.** A module does one thing. Before adding to a Rust file
   that passes about 600 lines, or to a type that owns resources for two
   concerns, split it by concern; spreading one type's `impl` across files
   does not count. Ports and tests are exempt. WGSL is written one statement
   per line with descriptive names; a port keeps upstream's names and order so
   it can be diffed.
9. **AR-9 — Design before code.** A change that adds or alters a stage, a
   shared contract, a layer boundary or a public type says in its issue or
   change description where it plugs in: its place in the stage order, what it
   reads and writes, the contracts it touches and what it deletes. If it does
   not fit this spec, amend the spec first, in the same change.
10. **AR-10 — Review structure.** A change covered by AR-9 gets an independent
    agent review against this spec, which the implementer obtains and resolves
    before merging. It never waits on the owner. Other changes do not need one.
11. **AR-11 — Leave it cleaner.** RD-3 applies to structure. A change leaves no
    compatibility shim, parallel path or dead option behind. Work that finds a
    breach of these rules in the code it touches fixes it or files the issue.

## Open questions

- How a blended surface that must also receive screen-space reflections is
  ordered (#20).
- How a game that moves its render origin for precision keeps motion and
  history exact without a static edit for every instance: an origin operation
  on the scene and renderer, or poses the renderer makes camera-relative.
  Decided by #17, before #19 builds on it.
- How ray traversal stays bounded in a scene of many instances; today it
  visits every one. Decided by #18, before #19 builds on it.
- Where dynamic GI updates, ray-traced shadows and two-phase occlusion culling
  sit in the stage order. Decided by the roadmap steps that add them.
