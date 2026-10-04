# SGL3D architecture

SGL3D follows the internal design established on 2026-09-30
([D-18](decisions.md), roadmap 23). The structural migration is implemented;
every change is held to the [rules](#rules) below.
[SGL3D](sgl3d.md) owns the consumer contract, rendering rules and roadmap;
this spec owns the shape of the code.

## Shape

Games use two objects.

- **`Scene`** is retained content: geometry, materials, textures, instances,
  lights, decals, environments, baked lighting, probes and the dynamic GI
  volume's placement, with stable identities
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
stay the game's to bake again when its static content changes. The dynamic
GI volume's placement (`DynamicGiVolume`: its origin, the first probe's
position; its probe spacing on each axis, in metres; and its probe counts,
at least two on each axis) is installed whole too
(`Scene::set_dynamic_gi_volume`; `None` removes it), and refused with the
typed error when its probes would exceed the device's texture limits. A
scene holds one; a game that installs none pays nothing for it. The scene
keeps the lattice the volume was installed on: installing it again with the
same spacing and counts moves its origin by the whole number of spacings
nearest the move (RTXGI's scroll anchor) and scrolls it, the probes that
stay keeping their state and the ones that enter starting afresh, as
RTXGI's infinite scrolling volume keeps its probes; a residual beyond a
small tolerance of the spacing, or another spacing or count, is another
placement and restarts it ([Dynamic diffuse GI](#designs-that-span-stages)).

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
  diffuse light from its lightmap charts, and where it has none its indirect
  diffuse light from the dynamic GI volume where one lights it, and writes
  no motion. Changing it in any way is a static edit.
- A **moving** instance is expected to be posed every frame. Nothing caches
  it: a view that shows it draws it every frame. It takes its indirect
  diffuse light from the dynamic GI volume where one lights it, else from
  the ambient cube the game sets for it, and writes motion from its
  pose in the last submitted frame. It has no motion in a frame where that pose
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

**Render origin.** Every position at the boundary is `f32` in the scene's
render frame. A game whose world is larger than `f32` renders precisely keeps
its own coordinates and gives the scene its content near an origin it
chooses, chunk-aligned in a streamed world. `Scene::move_origin(to)` moves
that origin to `to`, a finite `Vec3` in the current render frame, applied
exactly as given: every position the scene holds becomes what it was less
`to`, and from then on the game expresses the camera, the frame input and
what it edits in the new frame. It is an edit under the rule above and not
a static edit: it records no bounds, makes no cache stale, writes no motion
and cuts no history. The scene translates every position and transform it
retains, CPU state and GPU mirror alike, and uploads them as an edit does:
each instance's pose and the pose its motion is measured from, its bounds
and the culling hierarchy, the object records, the ray source's instances
and whatever is built over them, lights, decals, fog volumes, transient
geometry, the installed probes with their grid, the dynamic GI volume's
origin, and the pending static-edit bounds.
Geometry, deformed vertices and joint matrices are model-local; lightmaps,
irradiance atlases and ambient cubes have no position; a specular probe's
capture, and a dynamic GI probe's irradiance, depth and offset, are about
its centre: none changes, and a move is not a scroll. An inverse transform (a probe's
`world_to_local`, a shadow face's view) is rebuilt from its translated
centre or light, not translated. A translated position rounds once,
at its magnitude in the new frame, so content keeps the precision it was
added with and equal values stay equal; a chunk that should have a new
origin's full precision is re-posed by the game. The scene keeps where its
origin lies in the frame it was created in, a double-precision sum of its
moves, which the frame's history carries ([History](#shared-contracts)) and
the cascades snap about ([Shadows](#designs-that-span-stages)). When and
where the origin moves is the game's, like any content, and nothing about it
is a setting (S3D-6); a game that never moves it pays nothing: no pose or
input gains a field, and a frame compares two values.

## Frame

One frame, as the game sees it: edit the `Scene`;
`Renderer::resize(output size, scale, settings)`, which does nothing unless
they changed; `Renderer::render(scene, input, settings, output)` into the
game's encoder; submit; `Renderer::finish_frame(scene)`. `FrameInput` holds the
camera, the authored look and per-frame state (time, visibility, lights).
The rendering settings a game chooses are `settings::Settings`, one value the
game stores (S3D-6).
Authored look, such as exposure, bloom, colour grading or fog, is frame input,
not a setting.

Stages run in one order, written in one place in `renderer`:

1. **Prepare**: upload scene changes; build the views; cull; cluster
   lights and decals; then deform, in one compute pass before any pass draws scene
   geometry, the instances whose deformation changed since the last
   submitted frame.
2. **Dynamic GI**: the dynamic GI volume's update, while the scene holds one
   and the setting runs it: its probes' ray allocation, their rays through
   the scene's ray source, each hit shaded with one light and one visibility
   ray, and the blends of their irradiance and depth with probe relocation
   ([Dynamic diffuse GI](#designs-that-span-stages)). First after prepare,
   as Wicked Engine updates DDGI in its scene-update list before any camera
   pass: its hits take their visibility from rays, so it needs nothing of
   the shadows, no depth and no camera pass, and everything that shades
   reads it.
3. **Shadows**: the local-light atlas and the directional cascades.
4. **Volumetric fog**: the camera's froxel volume, lit by the frame's lights
   through the shadows and integrated into the frame's one fog, before any
   draw that fogs. It needs no depth, as Godot updates its volumetric fog
   before its opaque pass.
5. **Opaque**: the sky, the G-buffer and lit colour (direct, baked, emitted and
   ambient light, the ambient diffuse also recorded apart, unoccluded), then
   ambient occlusion over the G-buffer's depth and normals. Where the device
   has the colour attachments for it, the G-buffer and lit colour are one
   fused pass, the faster form on the consumer's route; otherwise a G-buffer
   pass and a lighting pass at its depth write the same targets.
6. **Receivers**: the transparent stage draws its reflective blended
   receivers' depth and traced lobe over a copy of the opaque depth, and
   their motion into the G-buffer's, so the surface that reflections and the
   temporal consumers see is the nearest reflective one, opaque or receiver
   ([Surface](#shared-contracts)). It runs only while the scene holds a
   receiver and a screen-space method, TAA, FSR2 or motion blur runs: with
   the method off and TAA on, receivers still draw depth and motion, so TAA
   reprojects a marked material by the receiver, not by what lies behind it.
7. **Reflections**: ambient occlusion of the opaque surfaces' ambient diffuse,
   environment and probe specular, the screen-space method over the surface,
   world-space rays from the opaque surfaces, and their one composition of
   the opaque lobes. Completion fogs the opaque surfaces and the sky.
8. **Transparent**: blended surfaces, additive effects and mist, each fogged
   where it lies, then distortion. It is drawn onto the composed frame and,
   while a screen-space method traces, into the reflection input before
   tracing, so reflections show it. Onto the composed frame, a receiver
   that is the surface at its pixel composes the method's result into its
   traced lobe.
9. **Exposure**: the frame's one exposure, fixed or metered from the complete
   HDR frame at the render size, before both of its readers.
10. **Antialiasing**: TAA or FSR2, from render size to scene size, reprojected
   by the surface's depth and motion. FSR2 reads the exposure.
11. **Motion blur**: the antialiased frame blurred along the surface's
   motion, at the scene size, before bloom, SMAA and tone mapping, as Bevy
   and Wicked Engine run it after TAA.
12. **Post**: bloom, SMAA, then tone mapping with the exposure and colour
   grading, dithered to the output.

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
| View and frame data | `View`, one per view (matrices, previous matrices, jitter, eye, viewport), and `Frame`, one per frame (time, the directional lights with the shadowed light's cascades, the hemisphere fill, the environment's diffuse lighting, reflection sky and backdrop, the fog volume's slicing and whether the frame has fog, mist, the visibility mask, the scene's baked-lighting constants: the atlas scale and the lightmap's chart transform, and the dynamic GI volume's placement, its scroll and whether it lights the frame), declared with their flag bits in `shading::uniforms`. Named fields; flags are integers with named bits. The renderer packs `Frame` from `FrameInput`'s typed values; no GPU layout is public. |
| Bind groups | For pipelines that draw scene geometry through the shading library. Group 0 has three layouts: lit (view and frame data, lights, decals and the atlas their images are packed in, clusters, shadows, environment, probes, the dynamic GI volume's probe texture, lookup tables and the fog volume), unlit (view and frame data, the frame's environment with its backdrop and the fog volume, for the sky, additive effects and mist) and shadow (view and frame data). The fog volume and its sampler are visible to fragment stages only. A view that renders into one of those binds a neutral stand-in for it; ray hits bind the lit layout with their light and decal lists, the local-light atlas's static layers and, as probe captures do, the installed probes; the dynamic GI probe rays' hits bind it with the volume's lists. Group 1: the object records, one storage buffer bound whole that geometry passes read, and the geometry buffers ray queries read. Group 2: material. Group 3: the stage's own. Ports, full-screen passes and the deform stage lay out their own. Four groups is the limit. Lit group 0 and group 1 bind 8 storage buffers to a fragment stage, wgpu's default limit and S3D-1's floor: `graphics_device::limits` requests the adapter's `max_storage_buffers_per_shader_stage`, and the change that adds another states the floor it needs in S3D-1 or folds two buffers into one (the probe collection holds its world grid; the decals' indices share the clusters' lists). Lit group 0 and a material bind 17 sampled textures to a fragment stage, the floor S3D-1 states, which the dynamic GI volume's probe texture raises to 18 when it lands and the blended pipelines' group 3 (the screen-space method's result and the surface depth) by two more when the receivers land (Dawn tiers `maxSampledTexturesPerShaderStage` at 16 or 48, and Metal, DX12 and Vulkan adapters offer 31 or more, so no device sits between 17 and 20: the practical cut stays above WebGPU's default 16, and each change that lands states its floor in S3D-1, the README and the docs); lit group 0 and the world-space trace's own targets, with the surface depth, bind 17 to a compute stage, 18 with the probe texture, and the dynamic GI trace binds 12 (lit group 0's ten that a compute stage sees, the probe texture it samples for the bounce, and its ray list), where the fog volume does not count: `graphics_device::limits` requests the adapter's, and the change that adds another states the floor it needs in S3D-1 or folds two textures into one (one lookup-table texture holds the rectangle lights' fit and the DFG table; one texture holds the dynamic GI volume's irradiance maps, depth maps and probe data). A stage whose passes bind lit group 0 and group 1 passes its own per-ray or per-probe data as textures, not storage buffers, as the dynamic GI stage does. |
| Layout mirroring | A struct shared between Rust and WGSL is declared once in each, side by side in `shading`. A test compares the Rust layout with naga's layout of the composed WGSL. A vertex buffer's layout is derived once from the Rust type it holds, and a test compares it with naga's inputs of the vertex entry points that read it. |
| G-buffer | What the opaque stage records for later stages, including the ambient diffuse within lit colour before occlusion. Its depth, normals, roughness, F0, anisotropy and source identity are the opaque surface's for the whole frame; its motion is the surface's ([Surface](#shared-contracts)), since nothing reads the opaque surface's motion once the receivers have drawn theirs. One WGSL module defines its targets and encodings with `encode` and `decode`, the receiver layer's included; a port converts at its adapter. |
| Surface | The nearest reflective surface at each pixel, opaque or blended receiver, which the screen-space method, world-space rays, composition, TAA, FSR2 and motion blur see: the **surface depth**, a copy of the opaque depth that the receiver pass draws its receivers over, tested strictly nearer and written, so the nearest receiver wins and one coplanar with opaque geometry leaves it the surface; the **receiver layer**, the traced lobe's normal and perceptual roughness at receiver pixels, in the G-buffer module's encodings; and the G-buffer's motion. A pixel is under a receiver where the surface depth is nearer than the opaque depth; no mask is stored. The renderer owns both targets, allocates them at the render size when the scene first holds a receiver, and lends the opaque depth as the surface depth in a frame that draws no receiver, so a game without receivers pays nothing. Each screen-space method's adapter converts the surface: the receiver layer where a receiver is nearer, else the G-buffer, through one accessor the G-buffer module owns, and the surface depth; TAA's context, FSR2 and motion blur read the surface depth and motion. World-space rays and composition read both depths; completion, probe culling, the transparent stage's depth tests and the diagnostics read the opaque depth. |
| Surface shading | One evaluated `Surface` and one set of functions for direct, environment and baked light, and one for the decals that change a lit surface before it is lit. Raster shading, probe captures and ray hits call the same functions (S3D-5). One determination gives a receiver its indirect diffuse light: its lightmap, else its irradiance atlas chart, else the dynamic GI volume where it lights the frame, reaches the receiver and has a blended probe about it, else a moving instance's ambient cube, else the frame's ambient (the environment's diffuse light and the hemisphere fill); the light loop's baked lights and the ambient occlusion's ambient diffuse follow it ([Dynamic diffuse GI](#designs-that-span-stages)). |
| Lights and shadows | One light record, at its identity's index in the scene's light buffer, and one accessor for the lights and decals that reach a point: the cluster in a camera view, culled lists elsewhere, which are a grid of one cluster. One writer packs every view's clusters; each list holds its live lights, then its baked ones, then its decals. The record has six rows: a point or spot light's shading reads its first four and, for its shadow, its sixth (its shadow opacity); a rectangle's reads all six, and `surface_direct_light` integrates its face by linearly transformed cosines, from lit group 0's table, in one loop over its lobes. A light's shadow record, at the same index in a buffer the shadow stage writes each frame, places its faces in the local-light atlas; a light without one is unshadowed. One sampling function per shadow kind, and one set of filters and receiver bias for every 2D shadow map, in `shading::shadow_sampling`: Bevy's Castano '13 kernel, its Jimenez '14 spiral where temporal antialiasing resolves it, its one hardware 2×2 tap for the camera's surfaces at the Low shadow quality (Godot's hard filter; `Settings::shadow_quality` also sets the cascades' and the local-light atlas's sizes, as Godot's desktop and mobile defaults), and for the fog, whose reprojection resolves it, its one hardware 2×2 tap for a local light and Godot's fog tap for the directional cascades (the one cascade at the point's view depth, one linear tap of the occluder's depth, the light fading exponentially with the metres the point lies behind it), chosen by what receives the shadow (a capture's or ray hit's surface, the camera's surface or the fog; a dynamic GI probe ray's hit is a fourth kind that takes no map: its one light's visibility is a ray, [Dynamic diffuse GI](#designs-that-span-stages)), and, but for Godot's fog tap, which takes none, its normal offset scaled by the map's texel size plus a depth offset toward the light. Each kernel tap is clamped to the map's rectangle in its texture, as Wicked Engine clamps to a light's atlas rectangle. A light's shadow opacity (Godot's `shadow_opacity`, on scene and directional lights) has one owner, `shading::shadow_sampling`'s `shadow_opacity_visibility` and `SHADOW_OPACITY_CUTOFF`: every receiver's visibility of a light is blended toward unshadowed by it, mix(1, visibility, opacity), and at or below the cutoff no visibility is looked up. The directional cascades and the local-light atlas use them. Every shadow kind culls casters as the camera does, as Bevy's shadow pipelines do and its bias assumes: a single-sided material casts from its front faces, a double-sided one from both. |
| Material records | A material's values reach the GPU as one record, `Material` in `shading/material.wgsl`, mirrored by `MaterialUniform`: named fields, and flags as integer `MATERIAL_*` bits (unlit, double-sided, the maps it was added with, its alpha mode and, for a blended one, whether it receives screen-space reflections). The scene packs it from the public typed `SurfaceMaterial` (named fields, `bool`s and the `AlphaMode` enum, whose `Blend { receives_screen_space_reflections }` carries the receiver flag where it applies) and from the maps the material was added with, which no edit changes; group 2 binds it and the ray source holds the same record. No GPU layout is public. |
| Scene records | An instance has one object record, declared in `shading::uniforms`, at its identity's index in the scene's object buffer, a storage buffer. Its flags are named bits; static or moving is one of them, never a range of indices. A deforming instance's record names its deformed vertices this frame and its positions in the last submitted frame. That index is the source identity the G-buffer stores and a ray hit reports; it names an instance within one frame only. Each instance of a draw reaches its record through its draw instance (`shading::vertex::DrawInstance`, a vertex buffer stepped per instance): the record's index and the drawn mesh's record in the ray source, as Bevy's batched draws reach each instance's `MeshUniform` from its instance index. A fragment reads the record at the source identity it carries. Instancing renumbers no instance. |
| Ray source | Each material, texture and model owns ranges of the ray buffers (its record; its level 0 in its image's format: RGBA8 texels, or a block-compressed image's blocks as stored, which a ray decodes texel by texel as raster's level 0 decodes them; its vertices, indices and BVH; a deforming model's influences and morph targets), and each deforming instance its joint matrices, morph weights and deformed vertices (`shading::deformation`), written when it is added or replaced and freed for reuse when it is removed. Above the model BVHs the source is two-level, as DXR and Vulkan acceleration structures, Bevy's ray-traced scene and Wicked Engine's hardware path are (Wald et al. 2003; Meister et al. 2021, §5.3.3). Each instance has one entry in the instance list at its identity's index, holding what its object record lacks (its model's ray words and its inverse pose), written when what it holds changes (its pose, its model or that model's geometry); a deforming instance's, which no ray sees, is never written. The list is never rebuilt for a frame, and a hit names the index raster names, whose object record holds its pose, flags and ambient cube. Two instance BVHs, static and moving, bound the capture-visible instances of each kind that do not deform by their posed model bounds; a leaf names entries. A traversal walks the BVH of the kind it wants, both for all, with the world-space ray, and at a leaf each instance's model BVH with the ray in that model's space, the one acceptance predicate (visibility group, alpha mode, side, cut-out texels, the receiver's own triangle and an open end of the interval) deciding each candidate; nothing tests a kind inside a traversal. The scene builds both BVHs on the CPU with the model BVHs' builder and node record, into ranges of the source whose roots the header names, each kept by its BVH and reallocated only when it outgrows it: the moving BVH on every traced frame and the static one on the first traced frame after a static edit, rebuilt rather than refitted, as Bevy and Wicked rebuild their TLAS every frame and NVIDIA and AMD advise; a frame traces when world-space reflections or the dynamic GI stage run. Nothing else is uploaded for unchanged content. The entries and both BVHs are in the object records' space, so a ray from the G-buffer traverses without a transform; a change of that space's origin (#17) is one scene operation that rewrites every entry and rebuilds both BVHs, never a static edit per instance nor a rewrite per frame. The hardware path (#23) builds its TLAS over the instances the BVHs bound, from their entries, each naming its model's BLAS as the entry names its BVH, with the index as the instance's custom index and its kind as its mask bit, selected by the ray's cull mask, and its candidate loop runs the same predicate, so both paths share one list and one hit: the instance index, mesh, triangle, distance and barycentrics, decoded once from the entry. |
| Draw lists | One builder turns a scene and a view into instanced draws. A view's population is a filter over instances by their flags (static, `visible`, `capture_visible`) and over materials by the visibility mask and alpha mode, not a walk of named collections. Blended materials are the camera's blended population alone, sorted back to front; the receiver pass draws that list's receiver batches, not a second list. Each instance is culled and selects its level of detail on its own; its draws of one mesh then merge with other instances' into one instanced draw per index range when they share geometry (model and mesh, or a deforming instance's own, as that instance deforms it), material, pipeline variant and mobility and draw the same ranges, as Bevy batches its phases: opaque, masked, capture and caster populations wherever they are, in bins ordered by their model's first instance and mesh, so each instance's meshes keep their order; blended ones only where adjacent in their sorted order. A batch names its instances by index, its geometry by model and mesh, and its material by identity. A deforming instance is culled by its deformed bounds and draws no level of detail. Every list of a frame, or of a probe capture, appends its draw instances to one buffer, which the renderer uploads once every list is built and before any pass draws, as Bevy writes one batched instance buffer for every view. Every geometry pass (G-buffer, lighting, shadow, capture) draws from a draw list; none walks the scene. Draw statistics count the draws and triangles of static and moving instances; a draw holds one mobility. |
| Geometry pipelines | One cache keyed by pass and by what the material and instance require (face culling; the alpha mode: opaque, masked or blended; and for pulled passes whether the instance deforms), not a field per variant, and by the lit constants: whether the scene holds a rectangle light and whether it holds a decal, one value (`LitConstants`) that the world-space reflection trace's pipelines are keyed by too. A masked material's pipelines discard the texels it cuts out, so opaque ones keep early depth; masked, blended and deformed pipelines are prepared once the scene holds such content. The lit constants specialise the lit passes and the world-space reflection trace, so a scene without rectangle lights or decals pays nothing for their shading: they shade rectangles (`rect_lights_enabled`) only while the scene holds one, as Godot specialises its clustered pass on `cluster_has_area_light`, and apply decals (`decals_enabled`) only while it holds one. |
| Sizes | Render size up to antialiasing, scene size after it, output size at presentation. Defined once by the renderer. |
| History | A stage owns its history. The receiver pass keeps none: the surface is rebuilt in every frame it runs. The renderer issues one reset for `FrameInput::camera_cut`, a `Renderer::resize` that changed the targets, or a different `Scene`. Content edits and lighting changes restart no history: each history rejects what changed by reprojection and clamping, as its upstream does; the scene's change tracking rebuilds bindings and instance motion and reports static edits to caches of static content ([Scene content](#scene-content)), nothing more. Camera history is the renderer's (S3D-4): the last submitted camera's unjittered view and projection, from which the `View`'s previous matrices come; a stage reprojects through them and keeps no camera of its own. The frame's history carries the scene's render origin, its summed moves as a value ([Scene content](#scene-content)). Each holder of state retained in the render frame records the origin that state is expressed in and, where the frame's differs, translates the state by the difference and records the frame's origin with it: the renderer its camera history, committed at `finish_frame` as that history is, as Filament keeps its antialiasing history in the user's world across its origin snaps; a stage what it retains (a shadow face's light and the poses of the moving casters it drew), committed as that state is. Repeating the step is idempotent, so an abandoned frame, which commits nothing, translates nothing twice: the next frame compares the same origins. What a stage keeps in screen space (colour, depth, motion, confidence, the fog's volume) needs nothing, and nothing restarts; a reset records the frame's origin with the new history. The dynamic GI stage's probe state is world-space history about each probe's centre and takes no renderer reset: the stage keys it on the scene identity and the volume placement it sees in prepare, restarting when either differs (a scroll keeps the probes that stay) and after frames in which it did not run. |
| Settings | The renderer resolves requested settings into one effective configuration per frame. Stages read only that, and report why a choice could not run. `Settings::dynamic_gi` (`DynamicGiQuality`: `Off`, `Low`, `High`; `High` by default) is the dynamic GI volume's quality tier: the most rays a probe traces a frame, Wicked's 256 at High; the volume's placement is content, and everything else about it is SGL3D's (S3D-6). |
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
  culled list: a capture every light that is on, ray hits the lights that
  reach the camera's view, as Wicked Engine's do, and the dynamic GI probe
  rays' hits the lights whose range reaches the volume's extent and the
  decals that reach it.
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
  light that casts a shadow has cascades that `view/cascades.rs` splits and
  fits from the camera: Godot's default splits, fixed shares of the range
  from the camera's near plane to the shadow's distance, and Bevy's
  constant-diameter, texel-snapped fit with its near plane Godot's 20 m
  pancake toward the light beyond the slice; both are SGL3D's, not the
  game's. Each
  cascade is a view with its own draw list in one layer of a depth array. A cascade's
  casters are culled without its near plane and drawn with unclipped depth
  (emulated where the device lacks `DEPTH_CLIP_CONTROL`), so a caster
  between the light and the cascade still casts: one within the pancake at
  its own depth, one beyond it at the near plane's. The camera's surfaces
  take the cascade at their view depth and blend into the next across the
  overlap; a probe capture fits its own cascades about its centre, and its
  surfaces and ray hits take the first cascade that holds them. Shadow views
  render through the common draw-list path. A move of the render origin
  ([Scene content](#scene-content)) leaves every static layer valid: a layer
  is depth from its light, and a face's view is rebuilt from the translated
  light; the cascades snap their texel grid about the frame the scene was
  created in: the scene's summed moves are taken into light space in double
  precision and reduced modulo the texel size, as Filament computes its
  snapping reference from its world origin in double, so a move shifts no
  shadow texel and the sum's magnitude costs none.
- **Opaque and masked surfaces.** Direct, baked and ambient light are computed
  in the forward pass, never from the G-buffer; environment specular and
  reflections are computed from it. Ambient occlusion is applied after the
  forward pass, by source completion, to the ambient diffuse (environment
  diffuse and the hemisphere fill, or the dynamic GI volume's irradiance
  where it stands in for them) that the pass records apart, as completion
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
- **Receivers.** A blended material the game marks
  `AlphaMode::Blend { receives_screen_space_reflections: true }` (glTF
  loads blended materials unmarked) receives the frame's screen-space
  reflections as the surface at its pixels. The game supplies the receiver
  as it supplies any surface: its mesh with the normals it animates (a
  caller-generated mesh replaced with `set_model`, or a deforming
  instance), and its material's roughness, F0 and
  coat; simulation, level and animation policy stay in the game. SGL3D
  owns the rest. Godot, Bevy, Wicked Engine and Filament trace opaque
  surfaces only (Godot b130438 `render_forward_clustered.cpp`, SSR before
  the colour passes and none for `RENDER_LIST_ALPHA`; Bevy 9d12036
  `ssr/mod.rs`, deferred only; Wicked 4323a33 `objectHF.hlsli`, `texture_ssr`
  under `#ifndef TRANSPARENT`, water from a planar `texture_reflection`;
  Filament ef1a133 `surface_light_indirect.fs`, `BLEND_MODE_OPAQUE ||
  BLEND_MODE_MASKED`, a TODO to ray-march blended surfaces), so SGL3D
  follows HDRP's practice for receivers (its `Receive SSR Transparent`
  materials write transparent depth and motion vectors, its SSR pass runs
  over them, and the transparent forward pass samples the result) with
  its own methods, not a second tracer. The transparent stage's receiver
  pass draws the blended list's receiver batches through the blended
  draw's vertex entry with the frame's jittered projection: the surface
  depth, tested strictly nearer and written, the traced lobe (the coat's on a coated
  receiver, else the base's; an unlit receiver is never traced) into the
  receiver layer, and unjittered motion into the G-buffer's. The screen-space
  method then traces the receivers as it traces opaque surfaces, from the
  same surface, at the same resolution, with the same cutoff and fade,
  through the same accumulation, where HDRP forces its Approximation
  algorithm without accumulation and a smooth evaluation on transparent
  receivers; if animated normals trail, the fix belongs in the method's
  rejection, not in a receiver path (AR-3). Drawing onto the composed frame,
  a receiver fragment whose depth equals the surface depth at its pixel (the
  vertex entry's `@invariant` position and the blended pipelines' primitive
  and depth state, no bias, the same culling and clipping, make the equality
  exact; the receiver pass tests strictly nearer where the blended draw
  tests nearer or equal) composes the method's result into its traced lobe
  by the one lobe formula, in place of that lobe's probe and sky specular,
  which is the fallback; every other blended fragment, a receiver
  behind it included, takes probe and sky specular as before. Into the
  reflection input, the receiver draws as before. The blended pipelines'
  group 3 lends the method's result, the surface depth and the method's
  cutoff and fade from the effective configuration. The approximation and
  its limits: the nearest receiver at a pixel is the surface, and alone
  reflects; the receiver's reflection is scaled by its alpha with the rest
  of its colour (glTF's coverage blend, as Godot and Bevy blend); the
  transmitted background is blended under it, undistorted; what is seen
  through a receiver reprojects and blurs by the receiver's motion, so a
  receiver that moves against its background trails the background, not its
  reflection (HDRP leaves that per material; SGL3D decides it once); the
  trace sees this frame's reflection input, so a receiver reflected in another shows
  its probe specular; world-space rays do not fill a receiver's misses;
  additive effects and mist never receive.
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
  screen-space method traces the surface ([Surface](#shared-contracts)),
  converted at its adapter, and returns premultiplied radiance and
  confidence ([D-17](decisions.md)); world-space rays, from the opaque
  surfaces, fill its misses and skip pixels under a receiver; one
  composition adds the opaque lobes, and under a receiver gives the opaque
  lobe its fallback alone, since the result there is the receiver's. The
  lobes' responses and directions, which lobe is traced, the method's cutoff
  test and fade, and the formula response × (reflected × fade + fallback ×
  (1 − confidence × fade)) have one owner, `shading/specular_lobes.wgsl`,
  which completion and composition, lit shading (probe captures, ray hits
  and blended surfaces) and the G-buffer's traced normal and roughness call.
  Another method plugs in beside the existing ones.
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
  each light scaled by its fog energy and skipped at or below 0.001
  (Godot's `volumetric_fog_energy` and cutoff) and its shadow taken at the
  light's shadow opacity, as surfaces take it (`shading::shadow_sampling`'s
  one blend), and the ambient fill;
  blends it with its reprojection into the stage's last volume; filters
  each slice across x and y (`Settings::fog_filter`), leaving the volume the
  next frame reprojects unfiltered; and integrates each column along its
  view ray. The
  integrated volume is the frame's one fog: group 0 lends it, with its
  slicing in `Frame`, to the draws that fog themselves (blended surfaces,
  glow and mist) through `shading::fog`, and source completion samples it
  for the opaque surfaces, the sky (by the fog's sky affect) and the
  incident radiance, as Godot's
  forward pass samples its volume for every material; composition scales
  reflections by its transmittance. No pass fogs the composed frame, and
  probe captures and ray hits have none.
- **Temporal.** Geometry writes unjittered motion. Jitter is applied to the
  projection by the antialiasing in effect, and every history resets together.
  Every temporal consumer (the screen-space method's accumulation, TAA,
  FSR2, motion blur) reads one surface depth and motion
  ([Surface](#shared-contracts)), so a receiver's pixels reproject by the
  receiver, which the receiver pass draws with the frame's jitter as opaque
  geometry is drawn, never by the background behind it; what is seen through
  the receiver follows the receiver's motion too. Motion blur reads
  them after antialiasing, so it blurs the camera's and moving instances'
  motion alike.
- **Exposure.** Lighting stays linear HDR to the tone map. One exposure value,
  a 1×1 texture the exposure stage writes and the renderer passes on, feeds
  FSR2 and the tone map. Automatic exposure keeps its adapted correction as
  history, which takes its target when history restarts and when automatic
  exposure follows a fixed one.
- **Dynamic diffuse GI.** Coloured bounce light from the frame's lights, the
  scene's lights, emitters and the sky on static and moving surfaces, from a
  volume of probes the game places ([Scene content](#scene-content)), kept up
  every frame by rays through the scene's ray source: a port of Wicked
  Engine's DDGI (`ddgi_rayallocationCS`, `ddgi_raytraceCS`, `ddgi_updateCS`,
  `ddgi_updateCS_depth`, `ShaderInterop_DDGI.h`, after Majercik et al. 2019
  and 2021). Each probe's irradiance is the bordered octahedral colour map
  Wicked stored before it moved to spherical harmonics (revision 95e357f:
  `DDGI_COLOR_TEXELS`, `DDGI_COLOR_BORDER_OFFSETS`, `ddgi_probe_color_uv`,
  six by six texels and a border, as Godot's SDFGI probes are), its depth
  the bordered sixteen-by-sixteen map of mean and squared distance both
  revisions keep, each bilinearly filtered through `baked_sampler`; Wicked's
  per-texel multiscale mean estimator, depth blend and probe relocation keep
  them up. Not taken, each for a reason: the later L1 spherical-harmonic
  storage and its dominant-light specular (the specular would count the sky
  a second time beside the environment specular, S3D-5); the colour map's
  BC6H compression (the sample reads what the blend writes); the direct
  light Wicked's rays gather from one static light at the probe itself
  (SGL3D's baked lights reach every receiver without baked lighting in the
  forward pass, and a light enters the probes through the hits it lights);
  and the voxel-grid nudge in relocation (SGL3D has no voxel grid;
  relocation follows the ray depths alone). Wicked's 0.95 damping of the
  bounce is kept. The stage runs first after prepare ([Frame](#frame)). It
  allocates each probe's rays as Wicked's allocation does: the tier's most
  rays scaled by the probe's inconsistency, a tenth of that outside the
  camera's frustum, in buckets of four and at least four; every probe at the
  tier's most on the first frame after a restart, and a probe that enters by
  a scroll likewise; so a converged volume costs four rays a probe and a
  changed one ramps to the most and back. It traces them through
  `scene_trace_nearest` over both kinds, which the hardware path (#23)
  replaces underneath as Wicked's `ddgi_raytraceCS_rtapi` replaces its
  software trace. A hit is shaded through `shade_ray_hit` for its diffuse
  radiance alone, as the fourth shadow receiver kind, the probe hit: its
  direct light is one light drawn uniformly from the frame's directional
  lights and the volume list's lights the hit takes, times their count, its
  visibility one any-hit ray from the hit toward the light (a point's or
  spot's position, a directional light's direction, and for a rectangle a
  point drawn uniformly on its face, as Wicked draws it) through the one
  acceptance predicate over both kinds (`scene_segment_visible`; a ray query
  that accepts the first hit in #23), as Wicked's `ddgi_raytraceCS` samples
  one light per hit with one shadow ray, and never a shadow map; that
  visibility takes the light's shadow opacity through
  `shadow_opacity_visibility`, and at or below `SHADOW_OPACITY_CUTOFF` the
  ray is not cast, as for every other receiver (S3D-5: one lighting model
  across GI), so a light
  the camera's atlas has no room for, or that lies outside the camera's
  view or its cascades, shadows a hit as any other and nothing leaks through
  walls into the probes; its indirect diffuse by the one determination
  ([Surface shading](#shared-contracts)), which gives an unbaked hit the
  volume's own last frame, damped, and so bounces without end; and its
  emission. A miss takes the environment's radiance along the ray as the
  diffuse environment turns and scales it, plus the hemisphere fill as the
  radiance field whose irradiance it is (its sky colour above the horizon
  and its ground colour below, times its intensity over π), so a receiver
  nothing occludes takes from the volume what the ambient gave it. The
  blends then write the probes. The trace binds the volume's lit group 0
  and group 1 and takes its ray list and writes its ray results as textures
  ([Bind groups](#shared-contracts)); the allocation and the blends bind the
  stage's own. The probe texture is the stage's, one RGBA16F texture of
  three regions, the irradiance maps, the depth maps and one texel per
  probe of probe data (its relocated offset as three halves and whether it
  has been blended, RTXGI's probe data), sized for the installed placement
  and lent through lit group 0 to every view that shades, so the camera's
  surfaces, blended surfaces, probe captures, world-space ray hits and the
  probes' own hits sample one volume, as Wicked's world-space DDGI applies
  in every camera. The sample is Wicked's `ddgi_sample_irradiance`: the
  eight probes about the point, weighted trilinearly, by its smooth
  backface test and by Chebyshev visibility from the probe's depth moments,
  reading irradiance by the surface normal; a probe not yet blended weighs
  nothing, and a receiver whose eight probes all weigh nothing keeps its
  fallback. A receiver the volume lights takes its irradiance in place of
  the environment's diffuse light and the hemisphere fill, recorded apart
  as the ambient that ambient occlusion weights; beyond the volume's extent
  a receiver keeps its fallback, the volume's share fading to nothing over
  the one probe spacing past its edge, as RTXGI's volume blend weight fades,
  so no seam shows there. Lightmapped and atlas-charted static receivers
  keep their bake and the ambient as before; moving instances and unbaked
  static ones take the volume where it lights them. Ambient cubes stay as
  the fallback beneath it: the volume takes precedence where it lights a
  receiver, and a moving instance's cube covers it elsewhere (no volume,
  the setting `Off`, beyond the volume's fade), as Wicked, Godot and Bevy
  keep a baked dynamic-object path (Wicked's lightmapped and ambient
  surfaces, the tetrahedral set of probes Godot's LightmapGI bakes, Bevy's
  irradiance volumes) beside their runtime GI; retiring them is revisited
  only with evidence from the consumer's route. Lightmaps and irradiance
  atlases stay too: the volume
  lights at probe spacing and cannot hold a bake's texel detail; whether a
  consumer's atlas still earns its place is settled on its route by the
  owner's look (RD-5) against the volume alone and by the per-pass cost and
  memory of each (RD-6). Specular occlusion is unchanged: completion's
  Lagarde occlusion from the ambient visibility already covers environment
  and probe specular, and the volume adds no specular term. The volume is
  not a cache of static content: a static edit (#19) marks nothing stale,
  and the estimator re-converges the probes the change reaches, as its
  upstream does; a render origin move translates the placement and nothing
  in the probes. Every pass is core WebGPU (compute, indirect dispatch,
  storage textures, workgroup reductions in place of wave intrinsics, packed
  halves in place of `f16`), so the browser and native run the same volume.

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
   Using an optional device feature where the adapter has it (S3D-1), such
   as subgroup operations, with the portable code as the fallback, is that
   stage specialised, not a path around it: take a measured native speed-up
   even where the browser cannot have it.
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

- Where ray-traced shadows and two-phase occlusion culling sit in the stage
  order. Decided by the roadmap steps that add them.
