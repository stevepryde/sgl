# SGL3D architecture

SGL3D follows the internal design established on 2026-09-30
([D-18](decisions.md), roadmap 23). The structural migration is implemented;
every change is held to the [rules](#rules) below.
[SGL3D](sgl3d.md) owns the consumer contract, rendering rules and roadmap;
this spec owns the shape of the code.

## Shape

Games use two objects.

- **`Scene`** is retained content: geometry, materials, textures, instances,
  lights, decals, environments, baked lighting, probes, the irradiance
  volume and the dynamic GI volume's placement, with stable identities
  ([Scene content](#scene-content)). It holds the CPU state and the GPU buffers
  that mirror it, and nothing that depends on a camera, an output size or a
  quality setting, with one exception: the hardware path's acceleration
  structures, which the renderer asks it to build for a frame, follow
  `Settings::hardware_ray_tracing` and the camera's distance orders their
  builds ([Hardware ray tracing](#designs-that-span-stages)).
- **`Renderer`** is the frame: views, stages, pipelines, targets, histories,
  settings and timing. It reads the `Scene` and does not change its content.

A `Scene` has one presenting `Renderer`. `Renderer::finish_frame(scene)`
commits a submitted frame: the `Scene` advances its instances' last-submitted
poses and the `Renderer` its histories (S3D-4).

The crate is six layers. Each may use the layers before it and none after it.

1. **content**: CPU data and loading: meshes, materials, images, light and
   probe descriptions, glTF and KTX2. No wgpu.
2. **shading**: the shared WGSL library and the Rust layouts that mirror it.
3. **scene**: `Scene`, its GPU buffers, change tracking, the ray-query
   structures built from its geometry (the portable BVHs and, where the
   device traces rays in hardware, its acceleration structures), and the
   geometry it takes prepared (`PreparedModel`), which needs neither the
   scene nor a device.
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
| Model | Geometry: an ordered list of meshes, each with vertices, indices, one material and what deforms it: a skin (each vertex's joint influences), morph targets, both or neither. A model with a deforming mesh deforms. Where the device traces rays in hardware, a model that does not deform also owns a BLAS over its meshes, built before the first hardware-traced frame that needs it, and a deforming instance one over its deformed positions, built again in every frame that deforms it ([Hardware ray tracing](#designs-that-span-stages)). | Its whole geometry, with any vertex and index counts, none included; its BLAS is built again. |
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

The irradiance volume is game-authored diffuse light the game keeps up by
regions ([Irradiance volume](#designs-that-span-stages)). Its placement
(`IrradianceVolume`: its origin, the least corner of its first cell; its
cell size on each axis, in metres; and its cell counts, at least one on each
axis) is installed whole (`Scene::set_irradiance_volume`; `None` removes
it), and refused with the typed error when its texture, Bevy's layout of the
six faces (twice the cells on y, three times on z), would exceed the device's
3D texture limit (under WebGPU's default `maxTextureDimension3D` of 2048:
2048 cells on x, 1024 on y, 682 on z); a scene holds one, and a game that installs none pays nothing
for it. Its cells are content the game writes by region
(`Scene::write_irradiance_cells`): a box of cells named by the position of
its least corner, on the lattice within a small tolerance of the cell size,
and its cell counts, with one `IrradianceCell` per cell (its six faces' own
light as an `AmbientCube`, irradiance / PI, finite and nonnegative, and the
sky's visibility toward each face, 0 to 1, both in the cube's face order
+X, −X, +Y, −Y, +Z, −Z, which the type documents), x fastest, then y, then
z; a box that is not on the lattice, lies partly outside the volume or
brings the wrong count or an invalid cell is refused and writes nothing.
The write takes the region prepared: a pure `Send` step
(`PreparedIrradianceRegion`, as `PreparedModel` prepares a model's
geometry, [Prepared geometry](#scene-content))
validates and packs the cells' faces without a device on whichever thread
the game chooses, and the write only copies them to the queue, so a
relight's packing stays off the render-critical thread. A cell the game has not written reads as the frame's
ambient whole with no light of its own: where a receiver's fallback is the
frame's ambient nothing changes, and where the dynamic GI volume or an
ambient cube would light it the volume covers them with that ambient, since
authored light wins across its extent and a game that wants the dynamic GI
volume or its cubes in a region leaves that region uncovered.
The scene keeps the lattice the volume was installed on: installing it again
with the same cell size and counts moves its origin by the whole number of
cells nearest the move and scrolls it, the cells that stay keeping their
content at their new texels and the ones that enter reading as the fallback
until written, as RTXGI's infinite scrolling volume keeps its probes and
Godot's SDFGI scrolls its cascades; a residual beyond a small
tolerance of the cell size, or another cell size or count, is another
placement, whose cells all read as the fallback. A region write is an edit
under the rule below and not a static edit: it records no bounds, makes no
cache stale and cuts no history; a specular probe capture that showed the
old light is the game's to capture again, as any bake.

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

**Prepared geometry.** A model's geometry reaches `add_model` and
`set_model` prepared, a model at a time, since its BVH's leaves name a mesh
and a triangle across all its meshes. `PreparedModel::new(meshes)` is a pure step that needs
no device and no scene and returns a `Send` value, so the game runs it on
whichever thread it chooses (S3D-1); SGL3D starts no thread. It does every
part of building a model that depends only on its meshes: it validates them
(indices name vertices, positions are finite, normals are finite and not
zero, a deformation fits its vertices), records whether each has authored
tangent frames, builds each mesh's culling hierarchy and local-light caster
clusters and the model's BVH, and packs the model's ray-source words (its
mesh records, each mesh's chart table, vertices ([Vertex encoding](#shared-contracts)),
indices and BVH, and a deforming model's influences and morph targets) and raster
geometry, addressed from zero, each mesh's vertex block starting at a
whole number of eight words, since a BLAS addresses its vertices in whole
32-byte strides ([Hardware ray tracing](#designs-that-span-stages)). `add_model` and `set_model` then do only what needs the scene or
the device: check what the prepared model names against the scene (its
materials, an anisotropic material's need for tangents, a deforming model's
static instances) and the device's limits, place its ranges in the ray
source (a model's range at an eight-word boundary, so its vertex blocks
keep theirs) and the geometry buffers ([Raster geometry](#shared-contracts)), add
each range's start to the words that address it, and copy the result to the
queue; where the device traces rays in hardware, a model that does not
deform is then pending a BLAS, which the scene builds before a
hardware-traced frame. An operation consumes the prepared model; one that fails places
nothing and returns the scene's typed error, and the game prepares again if
it wants another try. `add_asset` prepares the asset's meshes itself once
its materials have identities. A prepared model is the only way geometry
enters the scene, and the irradiance volume's cells arrive the same way
([Irradiance volume](#designs-that-span-stages)): a pure `Send` value built
by `new` from plain content data, which the scene operation takes and only
places and copies.

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
  diffuse light from the irradiance volume, else the dynamic GI volume,
  where one lights it, and writes no motion. Changing it in any way is a static edit.
- A **moving** instance is expected to be posed every frame. Nothing caches
  it: a view that shows it draws it every frame. It takes its indirect
  diffuse light from the irradiance volume, else the dynamic GI volume,
  where one lights it, else from the ambient cube the game sets for it, and
  writes motion from its
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
abandoning a frame loses none. The hardware path's acceleration-structure
builds are frame work in the frame's encoder, whose bookkeeping the scene
commits at `finish_frame`, so an abandoned frame leaves them pending and
the next frame builds them again
([Hardware ray tracing](#designs-that-span-stages)).

A static edit (adding, removing or changing a static instance, or replacing
the geometry of a model one uses) also records the world bounds it touched.
The scene keeps each edit's bounds pending until `finish_frame`, merging
them conservatively (spatial neighbours in pairs) only past a cap far above
a streaming frame's edits. A cache of static content marks what each kept
box reaches as stale, never what a union of them would, in any frame that
shows them to it, whether or not it redraws then, as Godot pairs an
instance with the lights its bounds meet and dirties only the paired
lights' shadows when it changes (b130438 `renderer_scene_cull.cpp`,
`_instance_pair` and `_update_instance`); a cache that was not kept up in a
frame starts over. The static instance BVH, rebuilt whole, counts the edits
instead. Static-edit bounds are the scene's part; what else makes a cache
stale (its light, the visibility mask, a material's caster values, another
`Scene`) is its owner's. An abandoned frame commits nothing: the next frame
measures motion from the last submitted frame and sees the same pending
bounds.

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
and whatever is built over them (the instance BVHs, and the hardware
path's TLAS at its next build), lights, decals, fog volumes, transient
geometry, the installed probes with their grid, the dynamic GI volume's
origin, the irradiance volume's origin, and the pending static-edit bounds.
Geometry, deformed vertices and joint matrices are model-local; lightmaps,
irradiance atlases and ambient cubes have no position; the irradiance
volume's cells are about its origin; a specular probe's
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

1. **Prepare**: upload scene changes; build the views; cull the camera's
   blended list on the CPU (the local-light faces plan theirs in the shadow
   stage); cluster
   lights and decals; deform, in one compute pass before any pass draws scene
   geometry, the instances whose deformation changed since the last
   submitted frame; then, while hardware ray tracing is in effect, the
   frame's acceleration-structure builds, the deforming instances' after
   their deform ([Hardware ray tracing](#designs-that-span-stages)); then
   the cull stage's early phase for every GPU-built view: its candidates,
   then their sections, against its frustum and, the camera's while
   occlusion culling runs, the last submitted frame's depth pyramid,
   appended to the draws they pass
   ([GPU draw lists](#designs-that-span-stages)).
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
   pass and a lighting pass at its depth write the same targets. While
   occlusion culling runs, the stage takes its two-pass form whatever the
   device, and the renderer interleaves it with the cull stage: the
   G-buffer pass over the early set; the cull stage's late phase (the depth
   pyramid from the opaque depth, then the late cull); the G-buffer pass
   over the late set; the cull stage's pyramid again, for the next frame;
   then the sky and the lighting pass once over both sets at `Equal`, before
   ambient occlusion ([GPU draw lists](#designs-that-span-stages)). While
   **ray-traced shadows** run ([Ray-traced shadows](#designs-that-span-stages)),
   the stage takes its two-pass form whatever the device, and the renderer
   encodes its named parts (G-buffer, per phase, lighting and ambient
   occlusion) with the ray-traced shadow stage between them: the G-buffer
   parts, which leave the G-buffer complete (the cull stage's late phase,
   late G-buffer and pyramid included where occlusion culling runs), then
   the traced shadow stage, which from the G-buffer's depth and normals,
   and nothing from the pyramid, traces the camera's shadow rays through
   the hardware ray source, denoises them and writes the shadow mask, then
   the sky and the lighting part at that depth, which takes the mask for
   the lights it holds, then ambient occlusion; Wicked Engine traces its RT
   shadows after its depth prepass and before the camera's main pass
   (2ff1d9e `wiRenderPath3D.cpp` 1050–1065, 1171–1179, 1625–1643).
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
   world-space rays from the opaque surfaces (to moving instances, or to
   everything the setting reaches), and their one composition of
   the opaque lobes. Completion fogs the opaque surfaces and the sky.
8. **Transparent**: blended surfaces, additive effects and mist, each fogged
   where it lies, then distortion. It is drawn onto the composed frame and,
   while a screen-space method traces, into the reflection input before
   tracing, so reflections show it. Onto the composed frame, a receiver
   that is the surface at its pixel composes the method's result into its
   traced lobe. While FSR2 runs, the draws onto the composed frame write
   its reactive and transparency and composition masks, which start clear
   there; first, while the scene holds an opaque or masked material whose
   shading moves where its geometry stands still (its normal layers move),
   the camera's sets of such materials are drawn again at the opaque depth,
   tested equal, marking the transparency and composition mask 1, as AMD's
   FSR sample marks its animated textures after its lighting and before its
   translucency (SDK 1.1.4 `AnimatedTexture.hlsl`, `fsrapiconfig.json`).
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
create, resize, prepare, encode and reset history, encode in named parts
where the stage order interleaves two stages (the cull stage's early, late
and pyramid; the opaque stage's G-buffer, per phase, lighting and ambient
occlusion, around the ray-traced shadow stage). It states what it reads,
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
| View and frame data | `View`, one per view (matrices, previous matrices, jitter, eye, viewport), and `Frame`, one per frame (time, the material animation's phase, the directional lights with the shadowed light's cascades, the hemisphere fill, the environment's diffuse lighting, reflection sky and backdrop, the fog volume's slicing and whether the frame has fog, mist, the visibility mask, the scene's baked-lighting constants: the atlas scale and the lightmap's chart transform, the irradiance volume's placement and whether it lights the frame, and the dynamic GI volume's placement, its scroll and whether it lights the frame), declared with their flag bits in `shading::uniforms`. Named fields; flags are integers with named bits. The renderer packs `Frame` from `FrameInput`'s typed values; no GPU layout is public. The phase is where `FrameInput::elapsed_seconds`, a double, falls within the hour over which material animation repeats exactly, reduced on the CPU, so the GPU's `f32` keeps its precision however long a session runs; Godot rolls its shader time over and Bevy wraps its time at the same hour, each with a jump, which the period's whole repeats avoid ([Material records](#shared-contracts)). |
| Bind groups | For pipelines that draw scene geometry through the shading library. Group 0 has three layouts: lit (view and frame data, lights, decals and the atlas their images are packed in, clusters, shadows, environment, probes, the irradiance volume's cells, the dynamic GI volume's probe texture, lookup tables and the fog volume), unlit (view and frame data, the frame's environment with its backdrop and the fog volume, for the sky, additive effects and mist) and shadow (view and frame data). The fog volume and its sampler are visible to fragment stages only. A view that renders into one of those binds a neutral stand-in for it; ray hits bind the lit layout with their light and decal lists, the local-light atlas's static layers and, as probe captures do, the installed probes; the dynamic GI probe rays' hits bind it with the volume's lists. Group 1: the object records, one storage buffer bound whole that geometry passes read, and the geometry buffers ray queries read. Group 2: material. Group 3: the stage's own, or a group the scene lends (a GPU-built cascade's casters: their set's positions slab's, which `scene::geometry` builds over `shading::bind::caster_positions`). Ports, full-screen passes, the deform stage and the cull stage lay out their own. Four groups is the limit. Lit group 0 and group 1 bind 8 storage buffers to a fragment stage, wgpu's default limit and S3D-1's floor: `graphics_device::limits` requests the adapter's `max_storage_buffers_per_shader_stage`, and the change that adds another states the floor it needs in S3D-1 or folds two buffers into one (the probe collection holds its world grid; the decals' indices share the clusters' lists). Lit group 0, the irradiance volume's cell texture and the dynamic GI volume's probe texture among it, and a material bind 19 sampled textures to a fragment stage, and the blended pipelines' group 3 (the screen-space method's result and the surface depth) two more: 21, the floor S3D-1 states (Dawn tiers `maxSampledTexturesPerShaderStage` at 16 or 48, and Metal, DX12 and Vulkan adapters offer 31 or more, so no device sits between 17 and 21: the practical cut stays above WebGPU's default 16); lit group 0 and the world-space trace's own targets, with the surface depth, bind 20 to the trace's fragment stage, where the fog volume counts; the dynamic GI trace binds 13 (lit group 0's eleven that a compute stage sees, the irradiance volume's cells, which its hits sample, among them, the probe texture it samples for the bounce, and its ray list), and the ray-traced shadow trace 14 (lit group 0's eleven and the G-buffer's depth, normals and F0), where the fog volume does not count: `graphics_device::limits` requests the adapter's, and the change that adds another states the floor it needs in S3D-1 or folds two textures into one (one lookup-table texture holds the rectangle lights' fit and the DFG table; one texture holds the dynamic GI volume's irradiance maps, depth maps and probe data). A stage whose passes bind lit group 0 and group 1 passes its own per-ray or per-probe data as textures, not storage buffers, as the dynamic GI stage does. The ray-traced shadow trace writes four storage textures (its visibility words, linear depth, the denoiser's tile masks and its normals), wgpu's default `max_storage_textures_per_shader_stage`; the hardware path it needs is native-only. The hardware path's TLAS is bound only by the passes that trace through it, in each tracing stage's group 3 at the one entry `scene_rays_hardware.wgsl` declares and `shading::bind::tlas_entry` builds (FRAGMENT and COMPUTE visible), lent by the renderer from the scene as the ray-hit group is; group 1 is unchanged ([Hardware ray tracing](#designs-that-span-stages)). The opaque stage's lighting pass, in its two-pass form while ray-traced shadows run, binds the ray-traced shadow stage's mask and slot table at its group 3 (`bind_shadow_mask.wgsl`), one sampled texture more, 20 of the 21 floor; the fused pass and every other lit composition bind none ([Ray-traced shadows](#designs-that-span-stages)). On Metal, wgpu 30 caps a stage's buffers of every kind (storage, uniform, vertex) and acceleration structures together at 29 (`max_buffers_and_acceleration_structures_per_shader_stage`, checked when a pipeline layout is created), down from 31 per kind in wgpu 29; `graphics_device::limits` requests the adapter's value, and SGL3D's largest stage binds about 15. |
| Layout mirroring | A struct shared between Rust and WGSL is declared once in each, side by side in `shading`. A test compares the Rust layout with naga's layout of the composed WGSL. A vertex buffer's layout is derived once from the Rust type it holds, and a test compares it with naga's inputs of the vertex entry points that read it. |
| Vertex encoding | A game gives vertices as `asset::Vertex`, 88 bytes of `f32`; the ray source keeps each in eight words (32 bytes), packed where its model is prepared ([Prepared geometry](#scene-content)). Everything but the position follows Godot's attribute compression (b130438 `servers/rendering/rendering_server.cpp`, `_surface_set_data` and `_get_axis_angle` under `ARRAY_FLAG_COMPRESS_ATTRIBUTES`; decoded by `_unpack_vertex_attributes`, `oct_to_vec3` and `axis_angle_to_tbn` in `forward_clustered/scene_forward_clustered.glsl`), extended to SGL3D's attributes. Its layout and its encoding have one owner in each language, `shading::packed_vertex` and `packed_vertex.wgsl`, side by side (AR-2): the layout test ties the struct, and a round trip on the GPU against independent expectations ties the encoder to the decoder. A vertex holds its position as three `f32` (words 0–2); its normal and tangent as the axis and angle of the rotation whose matrix rows are (tangent, bitangent, normal), as Godot's `Basis` holds the frame in `_get_axis_angle` and `axis_angle_to_tbn` reconstructs its rows, with bitangent = normal × tangent and the handedness carried by the angle's half: the axis octahedral in two 16-bit unorms (`Vector3::octahedron_encode`; word 3) and the angle a 16-bit unorm, Godot's encoding (low half of word 4); its lightmap chart as a 16-bit index into its mesh's table of distinct chart bounds (high half of word 4), since a chart's bounds repeat on every vertex of it; its UV as two 16-bit unorms across its mesh's UV rectangle (word 5); its colour as RGBA8, the colour sRGB-encoded and the alpha linear, clamped to 0..1 as glTF's `COLOR_0` is (word 6); and its lightmap UV as two 16-bit unorms, a negative one packed as (0, 0), which means unassigned alike (word 7). Each element is a whole number of words, as the geometry buffers' allocator requires ([Raster geometry](#shared-contracts)). The frame is made orthonormal before it is encoded: the tangent is projected onto the normal's plane and normalised, as `pbr_tangent_frame` does at shading; a tangent that vanishes under the projection, or whose handedness is not ±1, is absent (`asset::tangent_frames`), and an absent one takes a unit tangent in the normal's plane, Duff et al.'s orthonormal vector (2017, glam's `any_orthonormal_vector`), with handedness +1, which nothing reads, since anisotropy needs authored tangents; Godot's arbitrary tangent (`rendering_server.cpp`, its no-tangent branch) vanishes for a normal along (1, 1, −1). Godot's glTF importer instead leaves a mesh uncompressed when a tangent is not perpendicular to its normal (`modules/gltf/gltf_document.cpp` 1829–1843), and takes the axis and angle from `Basis::get_axis_angle`, which assumes a rotation and loses precision approaching its singularities at 0° and 180°; SGL3D takes them from the frame's unit quaternion, conditioned alike at every angle, and decodes with `axis_angle_to_tbn`. A normal that is zero or not finite has no frame, and its mesh is refused with the typed error, as a position that is not finite is; so is a mesh with more than 65 536 distinct chart bounds, the error naming the mesh's index. Each mesh keeps its own chart table, as it keeps its own UV rectangle, so a model names any number of charts across its meshes, and a chart two meshes name is stored in each, 16 bytes a mesh: one table a model (#136's) refused Hyperdrive's courses, which name over 100 000 charts across their meshes, one a triangle, and at most 18 432 in a mesh (#182); a model-wide table that stores each chart once, each mesh indexing it from its own start, would save only those 16 bytes a shared chart and would need every chart a mesh names to lie within 65 536 entries of that mesh's start; and a wider index would grow the vertex or take its bits from the angle or the lightmap UV. Positions are SGL3D's departure from Godot, which packs them as 16-bit unorms within each surface's box and splits nothing: they stay `f32`, as Bevy keeps them (9d12036 `crates/bevy_mesh/src/mesh.rs`, `ATTRIBUTE_POSITION` as `Float32x3`), because on Hyperdrive's content a 16-bit grid errs by up to 6.25 cm across its 4.5 km environment and 7.8 mm across its 650 m course, tilting its 2 m tunnel panels by about 0.45° against the 0.05° its reflection test holds (#136's review). The ray source therefore keeps exact positions for intersection and for what the scene builds over them, the casters' vertex buffers keep their `f32` positions, and the hardware path (#23) builds its BLAS from them. Every 16-bit and 8-bit value is rounded to the nearest step, where Godot's casts truncate (`(uint16_t)CLAMP(v * 65535, 0, 65535)`), an RD-2 improvement: truncation errs by up to a whole step, which puts a normal or tangent 0.0146° off and a UV, colour or lightmap UV a full step off. Tolerances, which the round trip holds: a normal or tangent within 0.01°; a UV within its rectangle's extent over 131 070 per axis, so a mesh tiled across many repeats loses precision with its extent; a colour within half an 8-bit step of its sRGB encoding; a lightmap UV within 1/131 070. Every reader decodes through `packed_vertex.wgsl`: the pulled raster passes (G-buffer, lighting, receiver, blended and probe capture faces) and the GPU-built cascades' pulled casters, a masked material's UV and colour and the positions of a mesh without slab positions ([GPU draw lists](#designs-that-span-stages)); a masked caster's UV and colour; the deform stage's rest normal and tangent, which need nothing of the mesh, so its dispatch is unchanged; the portable traversal's leaf test, for a cut-out's UV and colour; and hit decoding for world-space reflections and dynamic GI probe rays and their visibility rays. A mesh's record in the ray source carries its UV rectangle, the word where its own chart table starts, its bounds, and its section table, each leaf of its range hierarchy's bounds, mesh-relative first index and triangle count, with a bit of the count's word (`SECTION_PAIRED`) where its triangles pair, in the model's range, which the GPU draw lists cull by ([GPU draw lists](#designs-that-span-stages)). Deformed vertices, which the deform stage writes each frame, and morph targets' displacements stay `f32`. |
| G-buffer | What the opaque stage records for later stages, including the ambient diffuse within lit colour before occlusion. Its depth, normals, roughness, F0, anisotropy and source identity are the opaque surface's for the whole frame; its motion is the surface's ([Surface](#shared-contracts)), since nothing reads the opaque surface's motion once the receivers have drawn theirs. One WGSL module defines its targets and encodings with `encode` and `decode`, the receiver layer's included; a port converts at its adapter. The ambient target (`AMBIENT`, Rgba16Float) carries in its alpha the irradiance volume's sky visibility a(n) at the pixel, 1 where no volume lights it, which completion's occlusion of the sky's specular reads ([Irradiance volume](#designs-that-span-stages)). |
| Surface | The nearest reflective surface at each pixel, opaque or blended receiver, which the screen-space method, world-space rays, composition, TAA, FSR2 and motion blur see: the **surface depth**, a copy of the opaque depth that the receiver pass draws its receivers over, tested strictly nearer and written, so the nearest receiver wins and one coplanar with opaque geometry leaves it the surface; the **receiver layer**, the traced lobe's normal and perceptual roughness at receiver pixels, in the G-buffer module's encodings; and the G-buffer's motion. A pixel is under a receiver where the surface depth is nearer than the opaque depth; no mask is stored. The renderer owns both targets, allocates them at the render size when the scene first holds a receiver, and lends the opaque depth as the surface depth in a frame that draws no receiver, so a game without receivers pays nothing. Each screen-space method's adapter converts the surface: the receiver layer where a receiver is nearer, else the G-buffer, through one accessor the G-buffer module owns, and the surface depth; TAA's context, FSR2 and motion blur read the surface depth and motion. World-space rays and composition read both depths; completion, probe culling, the transparent stage's depth tests and the diagnostics read the opaque depth. |
| Surface shading | One evaluated `Surface` and one set of functions for direct, environment and baked light, and one for the decals that change a lit surface before it is lit. Raster shading, probe captures and ray hits call the same functions (S3D-5). A `Surface` holds its mapped normal, which shading uses, and its geometry normal, which the coat follows and shadow lookups offset along. A material's mapped normal, its normal map or that map's scrolling layers at the frame's animation phase, comes from `shading/material.wgsl`'s functions, which raster's builder (the G-buffer, the receiver layer, lit, blended and capture passes) and ray hits call with their own samples, so every view sees one moving surface. One determination gives a receiver its indirect diffuse light: its lightmap, else its irradiance atlas chart, else the irradiance volume where it lights the frame and reaches the receiver, else the dynamic GI volume where it lights the frame, reaches the receiver and has a blended, active probe about it (a moving receiver also a dormant one, with no surface in its cell; a dynamic GI probe ray's hit wherever the volume reaches it, at the volume's own light), else a moving instance's ambient cube, else the frame's ambient (the environment's diffuse light and the hemisphere fill); the light loop's baked lights and the ambient occlusion's ambient diffuse follow it ([Irradiance volume](#designs-that-span-stages), [Dynamic diffuse GI](#designs-that-span-stages)). |
| Lights and shadows | One light record, at its identity's index in the scene's light buffer, and one accessor for the lights and decals that reach a point: the cluster in a camera view, culled lists elsewhere, which are a grid of one cluster. One writer packs every view's clusters; each list holds its live lights, then its baked ones, then its decals. The record has six rows: a point or spot light's shading reads its first four and, for its shadow, its sixth (its shadow opacity, and whether it casts a shadow, a flag a dynamic GI probe hit reads); a rectangle's reads all six, and `surface_direct_light` integrates its face by linearly transformed cosines, from lit group 0's table, in one loop over its lobes. A light's shadow record, at the same index in a buffer the shadow stage writes when a light's record changes (its copy follows the queued writes, which an abandoned frame does not undo), places its faces in the local-light atlas; a light without one is unshadowed. One sampling function per shadow kind, and one set of filters and receiver bias for every 2D shadow map, in `shading::shadow_sampling`: Bevy's Castano '13 kernel, its Jimenez '14 spiral where temporal antialiasing resolves it, its one hardware 2×2 tap for the camera's surfaces at the Low shadow quality (Godot's hard filter; `Settings::shadow_quality` also sets the cascades' and the local-light atlas's sizes, as Godot's desktop and mobile defaults), and for the fog, whose reprojection resolves it, its one hardware 2×2 tap for a local light and Godot's fog tap for the directional cascades (the one cascade at the point's view depth, one linear tap of the occluder's depth, the light fading exponentially with the metres the point lies behind it), chosen by what receives the shadow (a capture's or ray hit's surface, the camera's surface or the fog; a dynamic GI probe ray's hit is a fourth kind that takes no map: its one light's visibility is a ray, [Dynamic diffuse GI](#designs-that-span-stages)), and, but for Godot's fog tap, which takes none, its normal offset scaled by the map's texel size plus a depth offset toward the light. The normal offset is along the receiver's geometry normal (the interpolated vertex normal toward the side shaded), never the mapped normal (normal or bump map, decals, scrolling layers), so none of them moves a shadow, as Filament ef1a133 offsets its spot and cascade shadows along its geometric normal flipped to the side shaded (`surface_getters.fs`, `getWorldGeometricNormalVector`; `surface_shading_parameters.fs`, `shading_geometricNormal`) and Bevy 9d12036 its point, spot and directional shadows along its geometric normal (`pbr_functions.wesl`, `in.world_normal`, which it flips only without tangents or a normal map); Godot b130438 offsets its directional cascades so (`scene_forward_clustered.glsl`, `geo_normal`) but its omni, spot and area lights along the mapped normal, which SGL3D does not follow. Each kernel tap is clamped to the map's rectangle in its texture, as Wicked Engine clamps to a light's atlas rectangle. A light's shadow opacity (Godot's `shadow_opacity`, on scene and directional lights) has one owner, `shading::shadow_sampling`'s `shadow_opacity_visibility` and `SHADOW_OPACITY_CUTOFF`: every receiver's visibility of a light is blended toward unshadowed by it, mix(1, visibility, opacity), and at or below the cutoff no visibility is looked up. The directional cascades and the local-light atlas use them. Every shadow kind culls casters as the camera does, as Bevy's shadow pipelines do and its bias assumes: a single-sided material casts from its front faces, a double-sided one from both. The camera's opaque surfaces have a fifth source while ray-traced shadows run: the ray-traced shadow stage's mask, a light's visibility at the pixel for each light the mask holds, taken through the one shadow-opacity blend in place of that light's map ([Ray-traced shadows](#designs-that-span-stages)); every other receiver, and a light the mask does not hold, keeps the maps. |
| Material records | A material's values reach the GPU as one record, `Material` in `shading/material.wgsl`, mirrored by `MaterialUniform`: named fields, and flags as integer `MATERIAL_*` bits (unlit, double-sided, the maps it was added with, its alpha mode and, for a blended one, whether it receives screen-space reflections, whether it scrolls its normal map, and whether global illumination gathers the light it gives off itself, `emits_into_gi` ([Dynamic diffuse GI](#designs-that-span-stages))). The scene packs it from the public typed `SurfaceMaterial` (named fields, `bool`s and the `AlphaMode` enum, whose `Blend { receives_screen_space_reflections }` carries the receiver flag where it applies, and `normal_layers`) and from the maps the material was added with, which no edit changes; group 2 binds it and the ray source holds the same record. No GPU layout is public. Normal layers are content (S3D-6): two `NormalLayer`s, each the material's repeating normal map at its own scale, moving across the surface at its velocity, its slopes taken at its strength, the two layers' slopes added (Barré-Brisebois and Hill's partial derivative blend), as Wicked Engine's water draws its normal map twice offset by its material's texture animation and Bevy's water example sums octaves of one map scrolled by velocity times time. The record holds each layer's speed as the whole repeats of its map it moves per animation period, rounded from the velocity and scale and at most 2^24, which `f32` holds exactly, so the frame's phase places it and nothing is uploaded per frame; how they move and blend is SGL3D's. A layer's time is the frame's, so a material moves in every view of a frame alike; nothing it changes is static content a cache holds (shadow layers hold depth, and bakes and probe captures stay the game's to take again), so it is no static edit. |
| Scene records | An instance has one object record, declared in `shading::uniforms`, at its identity's index in the scene's object buffer, a storage buffer. Its flags are named bits; static or moving is one of them, never a range of indices. A deforming instance's record names its deformed vertices this frame and its positions in the last submitted frame. That index is the source identity the G-buffer stores and a ray hit reports; it names an instance within one frame only. Each instance of a draw reaches its record through its draw instance (`shading::vertex::DrawInstance`, a vertex buffer stepped per instance): the record's index, the drawn mesh's record in the ray source, the first index it draws, relative to its mesh as the leaves' ranges are, and its triangle count (a GPU-built draw's vertices past it are dummies), and the base vertex a caster draw of it adds to its vertex indices to reach its positions in their slab (an indexed caster's to its indices, a GPU-built cascade's caster to the vertex index it pulls; `NO_POSITIONS` for a GPU-built draw of a mesh without slab positions) ([Raster geometry](#shared-contracts)), as Bevy's batched draws reach each instance's `MeshUniform`, and its `first_vertex_index`, from its instance index, and its meshlet raster reaches each cluster's instance and meshlet from its slot. A fragment reads the record at the source identity it carries. Instancing renumbers no instance. Beside its record, an instance has its draw candidates, one per mesh, in the scene's candidate buffer, each naming its object record, its mesh's record, its set and its level chain, with the sets they draw in; its `visible`, `capture_visible`, static and deforming bits are the record's, which the cull reads; the scene keeps them up with its edits, as it keeps the records ([GPU draw lists](#designs-that-span-stages)). |
| Raster geometry | What shadow casters draw from vertex and index buffers (each mesh's positions as `CasterVertex`, its indices, and its local-light caster clusters' indices) lives in shared geometry buffers that the scene suballocates, as Bevy packs meshes into slabs (`bevy_render` b56fc29, `mesh/allocator.rs` over `slab_allocator.rs`): slabs of one element layout each (positions; `u32` indices, which mesh and cluster indices share), whose ranges the scene's range allocator (`scene::ranges`, first fit over its free ranges, merged with their neighbours) hands out in elements. An element is a whole number of 4-byte words, the copy alignment, so any range is written and copied whole; a layout whose element is not would take Bevy's slot, the fewest elements that are (`elements_per_slot`), as its unit. With Bevy's defaults, a slab starts at 1 MiB or its first data, whichever is larger, and grows by half again up to 512 MiB or the device's largest buffer, whichever is smaller; data that fits no slab gets a new one, data of 256 MiB or more a slab of its own, and an emptied slab is released. A slab grows by copying it in its own submission through the queue, as Bevy's `reallocate_slab` and the ray source do; the only difference is that Bevy defers a frame's allocations to one commit, where the scene places a model's ranges at its operation, as every edit uploads at its call. A mesh names its slab and first element for each kind; its indices stay its own, so a caster draw binds a slab's buffers only when they differ from the last draw's and draws its index range offset by the first index of the indices it draws (the mesh's own or its caster clusters'), with the mesh's first vertex as the base vertex; a GPU-built cascade's casters pull a mesh's positions from its positions slab, which they bind whole as storage through the slab's group (`shading::bind::caster_positions`), at the first vertex their draw instance carries, and their indices from the ray source ([GPU draw lists](#designs-that-span-stages)), so a positions slab stays within a storage binding (`max_storage_buffer_binding_size`) as well as the device's largest buffer. A deforming model takes no positions range: its instances' caster draws read their positions from the ray source and their indices from the index slab, with base vertex zero. The draw instance carries the base vertex, which a batch's instances share, as they share their geometry, and which a masked caster, reading the ray source's vertex records by vertex index, subtracts, as Bevy's `morph_vertex` subtracts `MeshUniform::first_vertex_index` (`mesh.wgsl`). Removing or replacing a model frees its ranges for reuse; nothing creates a buffer per mesh, and a steady stream of bounded geometry creates none once the slabs hold its peak. The camera's and probes' pulled passes read vertices and indices from the ray source and bind no geometry buffer. |
| Ray source | Each material, texture and model owns ranges of the ray buffers (its record; its level 0 in its image's format: RGBA8 texels, or a block-compressed image's blocks as stored, which a ray decodes texel by texel as raster's level 0 decodes them; its vertices, packed ([Vertex encoding](#shared-contracts)), indices and BVH; a deforming model's influences and morph targets), and each deforming instance its joint matrices, morph weights and deformed vertices (`shading::deformation`), written when it is added or replaced and freed for reuse when it is removed. A model's words arrive prepared and addressed from zero ([Prepared geometry](#scene-content)): placing them names each mesh's material record in its mesh record and adds the range's start to every word that addresses the source (its mesh records' vertex and index words, and the words where each one's chart table and section table start, whose section entries' first indices are mesh-relative and need none; its BVH nodes' escape and first-leaf words, walked in their depth-first order, a zero root, a model without triangles, staying zero; and where a deforming model's influences and morph targets start), the rebase split among the owners of each layout (the mesh records' in `scene::rays`, the nodes' in the BVH builder's module beside the builder, a deformation's starts in `scene::deformation`), so no model BVH is built on the thread that places it. Each BVH level is built as Wicked Engine builds its acceleration structures, its TLAS to build fast and a static BLAS to trace fast (4323a33c `wiScene.cpp` 487, `wiScene_Components.cpp` 1392): a model's, built once where it is prepared, by the binned surface area heuristic (Wald 2007), a node of four or fewer a leaf as Embree's Triangle4 builder makes one (Godot b130438's bundled Embree; practice), and the instance BVHs, rebuilt on the render thread, by an equal-counts median split; a leaf names at most four records (`SCENE_BVH_LEAF_PRIMITIVES`). Above the model BVHs the source is two-level, as DXR and Vulkan acceleration structures, Bevy's ray-traced scene and Wicked Engine's hardware path are (Wald et al. 2003; Meister et al. 2021, §5.3.3). Each instance has one entry in the instance list at its identity's index, holding what its object record lacks (its model's ray words and its inverse pose), written when what it holds changes (its pose, its model or that model's geometry); a deforming instance's is written as any other's though only the hardware path reads it, whose predicate and hit decode take its positions, normals and tangents through the object record's deformed slot. The list is never rebuilt for a frame, and a hit names the index raster names, whose object record holds its pose, flags and ambient cube. Two instance BVHs, static and moving, bound the capture-visible instances of each kind that do not deform by their posed model bounds, on a hardware-traced frame those the TLAS does not hold alone ([Hardware ray tracing](#designs-that-span-stages)); a leaf names entries. A traversal walks the BVH of the kind it wants, both for all, with the world-space ray, and at a leaf each instance's model BVH with the ray in that model's space, the one acceptance predicate (visibility group, alpha mode, side under the ray's side policy, cut-out texels, the receiver's own triangle and an open end of the interval) deciding each candidate; nothing tests a kind inside a traversal. Camera-origin rays reject single-sided back faces as raster does; a dynamic GI probe ray and its visibility ray accept both sides, as Wicked's DDGI trace culls none, the side policy given beside the receiver. The scene builds both BVHs on the CPU with the model BVHs' builder and node record, into ranges of the source whose roots the header names, each kept by its BVH and reallocated only when it outgrows it: the moving BVH on every traced frame and the static one on the first traced frame after a static edit or a change of the static instances it covers ([Portable coverage](#designs-that-span-stages)), rebuilt rather than refitted, as Bevy and Wicked rebuild their TLAS every frame and NVIDIA and AMD advise; a frame traces when world-space reflections or the dynamic GI stage run. Nothing else is uploaded for unchanged content. The entries and both BVHs are in the object records' space, so a ray from the G-buffer traverses without a transform; a change of that space's origin (#17) is one scene operation that rewrites every entry and rebuilds both BVHs, never a static edit per instance nor a rewrite per frame. The hardware path ([Hardware ray tracing](#designs-that-span-stages)) builds its TLAS over the same entries, each instance naming its model's BLAS as the entry names its BVH, with the index as the instance's custom index and its kind as its mask bit, selected by the ray's cull mask, and judges a hardware hit by the same predicate, so both paths share one list and one hit: the instance index, mesh (the BLAS's geometry index), triangle (its primitive index), distance and barycentrics, decoded once from the entry; the hardware's own front-face flag is never read, since winding conventions differ by backend. The predicate takes a found candidate and the ray in the model's space and decides it (the receiver's own triangle, visibility group, alpha mode, side from the triangle's object-space winding, cut-out texels, the interval and its open end); the triangle solve that finds a candidate belongs to the portable traversal alone, the hardware finding its own. The predicate, the ray's validity test and the side policies (`SCENE_SIDES_*`) move out of the portable module into a shared one, `scene_rays_predicate.wgsl`, which every trace module composes. Every scene ray goes through one set of functions (`scene_trace_nearest`, `scene_trace_nearest_except_receiver`, which the `All` reach of world-space reflections takes, `scene_segment_visible` and the moving-nearest and static-visibility pair of the `Moving` reach) with one signature and one side policy per ray, defined once in each program: the portable walk itself (`scene_rays_walk.wgsl`, the BVH traversal over the header's roots) is one module, which the portable implementation of the function set (`scene_rays_portable.wgsl`) and the shared hardware module (`scene_rays_hardware.wgsl`, which walks the portable BVHs over the instances the TLAS does not hold and takes the nearer result) both compose, so a program holds one definition of each function; each form's query module (`scene_rays_query_opaque.wgsl`, `scene_rays_query_candidates.wgsl`) is the composition root of the hardware path and depends on the shared module, as the shadow-mask providers depend on what calls them, and the shared module never lists a query module as a dependency, since `shading::compose` recurses forever on a cycle (`shading/mod.rs` 52–65); a tracing pipeline composes the root of the form in effect. |
| Draw lists | Two builders turn a scene and a view into draws, and one executor issues them. The GPU builder ([GPU draw lists](#designs-that-span-stages)) builds the camera's opaque and masked list and each directional cascade's from the scene's candidates: each candidate culled and its level chosen, then each of its sections culled and appended to its set's region of the view's cluster list; one indirect draw per set and phase, whose instances are the sections that passed, and for a cascade one more per opaque set, indexed, of its sections whose triangles pair. The CPU builder walks the instances for the rest: the camera's blended population, over the instances the scene indexes as holding a blended mesh, sorted back to front, whose receiver batches the receiver pass draws, not a second list; the local-light shadow faces, by their caster clusters; and a probe capture's faces and cascades, unculled. No view is built both ways. In both, a view's population is a filter over instances by their flags (static, `visible`, `capture_visible`) and over materials by the visibility mask and alpha mode, not a walk of named collections. The CPU builder culls each instance and selects its level of detail on its own, by its deformed bounds and no level of detail for a deforming instance, and merges its draws into instanced draws of equal geometry (model and mesh, or a deforming instance's own, as that instance deforms it), material, pipeline variant, mobility and index range, as Bevy batches its phases: capture and caster populations wherever they are, in bins ordered by their model's first instance and mesh, so each instance's meshes keep their order; blended ones only where adjacent in their sorted order. A GPU-built draw holds every visible section of its set, whatever its mesh or mobility, in the order they passed. A batch names its instances by index, its geometry by model and mesh, and its material by identity. Every CPU-built list of a frame, or of a probe capture, appends its draw instances to one buffer, which the renderer uploads once every list is built and before any pass draws, as Bevy writes one batched instance buffer for every view; a GPU-built list's are written on the GPU into the view's own cluster list, whose set region the executor binds as the draw-instance buffer. Every geometry pass (G-buffer, lighting, shadow, capture, FSR2 composition) draws from a draw list through the one executor, which binds a list's pipeline, material, slabs or region and draw instances and issues its draws, direct or indirect; none walks the scene. A pass may issue only the sets of a GPU-built list whose material a predicate holds for: the FSR2 composition pass issues the camera's sets whose material's shading moves (`Material::surface_moves`). Draw statistics count draws and triangles by mobility: a CPU-built draw holds one mobility and counts as it is built; a GPU-built view's are counted on the GPU and read back without blocking, so `Renderer::geometry_stats` describes the most recent completed frame (none before one completes) and `diagnostic_draws` the commands encoded. |
| Geometry pipelines | One cache keyed by pass and by what the material and instance require (face culling; the alpha mode: opaque, masked or blended; and for pulled passes whether the instance deforms), not a field per variant, and by the lit constants: whether the scene holds a rectangle light and whether it holds a decal, one value (`LitConstants`) that the world-space reflection trace's pipelines are keyed by too. A masked material's pipelines discard the texels it cuts out, so opaque ones keep early depth; masked, blended, receiver and deformed pipelines are prepared once the scene holds such content, and the FSR2 composition pipelines (`GeometryPass::Fsr2Composition`, opaque and masked) once it holds an opaque or masked material whose shading moves. The lit constants specialise the lit passes and the world-space reflection trace, so a scene without rectangle lights or decals pays nothing for their shading: they shade rectangles (`rect_lights_enabled`) only while the scene holds one, as Godot specialises its clustered pass on `cluster_has_area_light`, and apply decals (`decals_enabled`) only while it holds one. A pipeline that traces (the world-space trace, the dynamic GI trace, the ray-traced shadow trace) is keyed by the ray form in effect too (portable, hardware baseline or hardware candidates) and composes the portable module, or the shared hardware module with that form's query module ([Hardware ray tracing](#designs-that-span-stages)); the world-space trace is keyed by its reach as well, a pipeline constant (`world_reach_all`) choosing between `Moving`'s pair of functions and `All`'s one. The lit shading library asks one function, `camera_shadow_mask`, for the mask's visibility of a light at the camera's pixel, and two provider modules define it: `shadow_mask.wgsl`, which reads the ray-traced shadow stage's mask and slot table at group 3 and which only the opaque stage's two-pass lighting pipeline composes, and `shadow_mask_none.wgsl`, which reports no slot and which every other lit composition (the fused pass, captures, blended surfaces, ray hits, the fog) composes; a program composes exactly one, which the layout test's validation of every composed program holds. |
| Sizes | Render size up to antialiasing, scene size after it, output size at presentation. Defined once by the renderer. |
| History | A stage owns its history. The receiver pass keeps none: the surface is rebuilt in every frame it runs. The renderer issues one reset for `FrameInput::camera_cut`, a `Renderer::resize` that changed the targets, or a different `Scene`. Content edits, lighting changes and material animation restart no history: each history rejects what changed by reprojection and clamping, as its upstream does (FSR2 takes a blended surface's changing shading from the reactive and composition masks blended surfaces write, and an opaque or masked surface's moving normal layers from the transparency and composition mask the transparent stage marks over it); the scene's change tracking rebuilds bindings and instance motion and reports static edits to caches of static content ([Scene content](#scene-content)), nothing more. Camera history is the renderer's (S3D-4): the last submitted camera's unjittered view and projection, from which the `View`'s previous matrices come, and the jitter that frame applied; a stage reprojects through them, with the jitter where it reprojects what was rasterized jittered, and keeps no camera of its own. The frame's history carries the scene's render origin, its summed moves as a value ([Scene content](#scene-content)). Each holder of state retained in the render frame records the origin that state is expressed in and, where the frame's differs, translates the state by the difference and records the frame's origin with it: the renderer its camera history, committed at `finish_frame` as that history is, as Filament keeps its antialiasing history in the user's world across its origin snaps; a stage what it retains (a shadow face's light and the poses of the moving casters it drew), committed as that state is. Repeating the step is idempotent, so an abandoned frame, which commits nothing, translates nothing twice: the next frame compares the same origins. What a stage keeps in screen space (colour, depth, motion, confidence, the fog's volume) needs nothing, and nothing restarts; a reset records the frame's origin with the new history. The dynamic GI stage's probe state is world-space history about each probe's centre and takes no renderer reset: the stage keys it on the scene identity and the lattice the volume it sees in prepare lies on, restarting when either differs (a scroll keeps the probes that stay, clearing those that enter) and after frames in which it did not run; the placement a frame scrolled to is committed with it. The ray-traced shadow stage's history (its temporal mask pair, and the denoiser's moments and filter history for the slots it denoises) is screen space, reset by the renderer's one reset and after a frame in which the stage did not run; a slot whose light changed restarts alone, through the slot table's restart bit, and a move of the render origin touches none of it. The cull stage's history is the camera's depth pyramid, its own last executed build, so it holds the last submitted frame's; the next frame's early phase reads it through the camera history's previous matrices and jitter and the object records' previous poses, and a reset frame, or one whose last submitted frame built none, reads none and culls by frustum alone; a mismatch costs time, not a surface, since the late phase tests again ([GPU draw lists](#designs-that-span-stages)). |
| Settings | The renderer resolves requested settings into one effective configuration per frame. Stages read only that, and report why a choice could not run. `Settings::dynamic_gi` (`DynamicGiQuality`: `Off`, `Low`, `High`; `High` by default) is the dynamic GI volume's quality tier: the most rays a probe traces a frame, Wicked's 256 at High; the volume's placement is content, and everything else about it is SGL3D's (S3D-6). `Settings::hardware_ray_tracing` (`bool`, `false`: off by default and in every preset, the game opts in, the owner's decision, [D-28](decisions.md)) traces every scene ray through the device's acceleration structures where it has ray queries, else through the portable BVHs; `Settings::ray_traced_shadows` (`bool`, `false`: off by default and in every preset, as hardware ray tracing is, [D-28](decisions.md)) gives the camera's opaque surfaces ray-traced shadows while hardware ray tracing is in effect, reported by `Renderer::ray_traced_shadows_in_effect`; `Settings::world_space_reflections` (`WorldSpaceReflections`: `Off`, `Moving`, `All`; `Off`, and no preset turns it on) is what world-space rays fill the screen-space method's misses with, `All` meant for the hardware path and allowed on the portable one ([Reflections](#designs-that-span-stages)). `Renderer::ray_tracing_in_effect` and `ray_tracing_error` report the hardware path as `antialiasing_in_effect` and `fsr2_error` report FSR2 ([Hardware ray tracing](#designs-that-span-stages), [Ray-traced shadows](#designs-that-span-stages)). `Settings::occlusion_culling` (a `bool`, off by default, as Bevy's `OcclusionCulling` is opt-in, until a net saving measured on the consumer's routes records otherwise) runs the two-phase occlusion test in the opaque stage's two-pass form with its pyramids, a cost that pays only where a frame submits much hidden geometry, which the game knows: on Apple's tile-based GPUs, which discard hidden fragments before shading them, #24's oracle found a saving in a large open view and none on a walk or in a cave, and the real culling cost 0.2–0.5 ms a frame natively on Metal on all three while saving 0.9–1.4 ms in Chrome over a large window seen from the ground; off, the GPU-built views draw everything their frustums hold. Which views build their lists on the GPU is SGL3D's ([GPU draw lists](#designs-that-span-stages)). |
| Timing | Every pass belongs to its stage's timing group. |
| Diagnostics | Behind the `diagnostics` feature. Switches are `Settings::diagnostics`, resolved into the effective configuration, never environment variables; observations return to the game, and the library writes no files. Every layer writes to the GPU, creates buffers (with contents or without; creating or growing a geometry slab or the ray source counts as a creation, and its copy as a growth), prepares and places models and builds instance BVHs through `counters`, a leaf module that counts them on the calling thread with the feature (`diagnostics::counters`) and passes straight through without it; `Scene::diagnostic_resources` and `Renderer::diagnostic_draws` report the buffers content holds (the candidates, chains and sets among them) and each view's draws as encoded; the hardware path's BLAS builds, compactions and TLAS builds count through `counters` too, and `diagnostic_resources` reports the BLASes it holds and their triangles: wgpu 30 reports no acceleration structure's size. `geometry_stats_for_model` reads back each candidate's visible sections under this feature. The `culling` layer makes both builders submit every level-selected draw, the GPU's cull accepting every candidate and section. `Renderer::diagnostic_view_times` reports the CPU time each camera and cascade list took to build and to record. `Diagnostics::dynamic_gi` observes the dynamic GI stage: an observed frame traces through the trace's portable program with its BVH walks counted (`ray_observation_enabled`), a pass sums them, and `Renderer::take_dynamic_gi_reports` returns each observed frame's probes, rays, visits, budget stride and what kept the volume awake, numbered by the frames the renderer finished before it, read back without blocking; at most 8 readbacks wait, and the next report counts the observed frames skipped past them. Off, the stage runs its plain trace program and counts nothing. |

WGSL is composed from named modules by one function, `shading::compose`: each
module declares the modules it uses, and a program is their concatenation in
dependency order, each once, with any `enable` directive a module declares
hoisted to the program's head, where naga alone accepts it. The layout test
also parses and validates every composed program and checks that it holds
exactly the entry points its pipelines are created with: a constant beside
the module that declares each entry point names it, and the pipeline code and
the test use it. A
shader file holds its entry points and its stage's own
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
  moved or deformed; it redraws the layer when a static edit's bounds reach
  it (its light's range, then its face's frustum), or the visibility mask or
  a material's caster values make it stale, and draws
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
- **Ray-traced shadows.** While hardware ray tracing is in effect
  ([Hardware ray tracing](#designs-that-span-stages)) and
  `Settings::ray_traced_shadows` resolves on, the camera's opaque surfaces
  take their shadows from rays through the scene instead of from the maps:
  a port of Wicked Engine's RT shadows (2ff1d9e: `rtshadowCS.hlsl`, which
  is `screenspaceshadowCS.hlsl` under `RTSHADOW` and `RTAPI`, 9–14, 45–46,
  76–84, 97–210, 215–257, 298–301 and 309–319;
  `rtshadow_denoise_tileclassificationCS.hlsl` and
  `rtshadow_denoise_filterCS.hlsl`, which include AMD's FidelityFX shadow
  denoiser (FidelityFX-Denoiser d7dfecb, `ffx-shadows-dnsr/`
  `ffx_denoiser_shadows_tileclassification.h`, `_filter.h` and `_util.h`,
  MIT, the licence `sgl-post-fx` already carries for its reflection
  denoiser, registered for `sgl-3d` too); `rtshadow_denoise_temporalCS.hlsl`;
  `rtshadow_upsampleCS.hlsl`; `Postprocess_RTShadow` in `wiRenderer.cpp`
  15498–15880 with its resources at 15440–15497; and `lightingHF.hlsli`
  58–85, where the mask multiplies the light under `SHADOW_MASK_ENABLED`
  and never for `TRANSPARENT`). The stage, `stages::shadows::traced`, sits
  inside the opaque stage's two-pass form ([Frame](#frame)): the renderer
  encodes the opaque stage's G-buffer parts, per phase, with the cull
  stage's late phase and pyramid where occlusion culling runs, then the
  traced shadow stage's encode, then the opaque stage's lighting part (the
  sky and the lighting pass at the G-buffer's depth) and its ambient
  occlusion; the fused form never carries a mask, so
  the setting's cost is the second geometry pass (which shades each pixel
  once, where the fused pass shades its overdraw) beside the trace,
  measured against the fused form on the consumer's route (RD-6). It
  reads the G-buffer's depth and normals and the surface motion, keeps its
  own previous depth at its half resolution for the denoiser's
  reprojection (as the world-space reflection denoiser keeps its own; the
  two are histories of one depth at two resolutions, which the renderer
  may unify later), and writes the **shadow mask**, a full-resolution
  `Rgba8Unorm` storage array of four layers, one 8-bit visibility per
  slot, `RT_SHADOW_LIGHTS` (16, Wicked's `MAX_RTSHADOWS`) slots in all (a
  core storage format, where Wicked's `R8_UNORM` array is not one in
  wgpu 30), and the **slot table**, a uniform naming each slot's light and
  whether its history restarts, both lent to the lighting pass at its group
  3 ([Bind groups](#shared-contracts)); the lighting pass's camera surfaces
  take a light's slot visibility in place of its map through the one
  shadow-opacity blend ([Lights and shadows](#shared-contracts)), and the
  maps stay for everything else: the fog, blended surfaces, probe captures,
  ray hits, the lights the mask does not hold, and the fallback. Slot 0 is
  the directional light with the frame's cascades; slots 1 to 15 go to the
  casting local lights that reach the camera's view in the local atlas's
  own ranking, a value the renderer passes from the atlas's plan, so a
  light with a slot is one the atlas places too, and a light keeps its slot
  while it is seen, as it keeps its atlas slots, so the slot's history is
  one light's (the denoised slots 1 to 3 are therefore the longest-seen
  lights, not always the highest-ranked: a stable history is worth more
  than rank); a freed slot goes to the highest-ranked light without one
  and restarts. Wicked's slot is the light's index among the first sixteen
  of its sorted entity array (76–84); SGL3D's lights have no such order.
  The trace runs at half resolution (Wicked's `DOWNSAMPLE` 2) and casts,
  per pixel and per slot whose light reaches the surface (a point or spot
  within its range, a spot within its cone, every light on the lit side;
  97–210), one visibility ray from the surface position with Wicked's
  `TMin` of 0.01 to the light (a directional light to infinity, 117), with
  no receiver exclusion, since the two-pass form writes the source identity
  in the lighting pass, after the trace, and Wicked uses none (its `TMin`
  alone keeps a ray from its receiver), under a third side policy,
  `SCENE_SIDES_SHADOW`: a single-sided material occludes only when met
  from behind, a double-sided one from either side, which is the maps' rule
  in a ray's terms (a shadow map draws a single-sided caster's front faces
  from the light, so a ray from the receiver meets that caster's back) and
  Wicked's `RAY_FLAG_CULL_FRONT_FACING_TRIANGLES` (227), applied by the
  shared predicate as every side policy is: a ray whose origin lies a
  rounding behind its lit face meets that face from behind, which the
  policy accepts, so the `TMin` is what keeps it from its own receiver, as
  in Wicked. A pixel the G-buffer drew nothing lit at (an unlit material)
  casts no ray and is the sky to the passes that follow, which keeps its
  zero visibility out of its lit neighbours' upsample. The end of the ray is drawn on the light (Wicked
  104–106, 124–127, 153–159, 186–188): a point on the disc of the light's
  radius (`LightShape::Point` and `Spot` gain `radius`, metres; Wicked's
  `LightComponent::radius`, 0.025) about a point or spot light, a point on
  a rectangle's face as the dynamic GI visibility ray draws it (which from
  now on draws its end on a point's or spot's radius too, S3D-5), and
  within `DirectionalLight::angular_diameter` (radians; the sun's 0.53°,
  SGL3D's own where Wicked spreads a directional light by the same
  `radius` in direction units) about a directional light's direction, one
  draw per pixel per frame from the hash world-space reflections took in
  place of Wicked's blue noise, a departure recorded there whose look the
  owner judges here too (RD-5); a radius of 0 is a hard shadow. Both fields
  are content a light carries and only rays read. The visibilities pack
  into Wicked's 8-bit mask, four slots a word (298–301), with the
  8×4-group hit bitmask the tile classification reads (309–319). Slots 0
  to 3 are denoised by AMD's shadow denoiser as Wicked runs it: tile
  classification against the previous frame's moments and the reprojected
  history, then three filter passes at step sizes 1, 2 and 4, the last
  recovering contrast (`rtshadow_denoise_filterCS.hlsl` 79–82); slots 4 to
  15 take Wicked's temporal blend (`_temporalCS.hlsl` 101–124: a 3×3
  variance clamp, a response from 0.88 to 1 by the change, refreshed by
  velocity); and the upsample weights the four half-resolution texels by
  linear depth against the full-resolution pixel's (`_upsampleCS.hlsl`
  35–55) into the mask's layers. Changed at the port boundary: the trace
  reads normals and depth from the G-buffer at the full-resolution pixel
  of each half-resolution one, as Wicked's denoiser reads its depth
  (`texture_depth[did * 2]`) where its trace samples depth between four
  pixels, and writes that pixel's linear depth and shading normal, the
  half-resolution copies the denoiser reads as Wicked's reads its own
  (its tile classification still reprojects from the G-buffer's depth);
  the filter takes that linear depth as it is, where AMD's linearises a
  projected depth through the inverse projection at every tap; AMD's
  kernel weights are constants, which its compiler folds; the denoised
  slots' result is one word a tracing pixel, which the last filter pass
  writes whole, a slot a byte, and the temporal blend takes as its first
  word; and the
  upsample's bilinear fractions are those of the trace's grid, where
  Wicked's taps and fractions disagree; the temporal blend clamps
  its history to the 3×3 box, which Wicked computes and leaves unused;
  the temporal blend and the upsample read each texel once and work on a
  word's four slots together, skipping the words and layers whose slots
  hold no light, where Wicked's temporal blend reads the 3×3 neighbourhood
  again for each slot and its upsample writes the lights of the pixel's
  tile; a light reaches a pixel as the lit library's `light_reach` decides, a
  directional light where the shading or the geometry normal faces it,
  since the coat takes it along the geometry normal, so the trace casts a
  ray wherever lighting shades the light; the denoiser's wave reduction
  takes its own workgroup fallback
  (`ffx_denoiser_shadows_tileclassification.h` 28–45), the four slots'
  votes bits of one atomic word, subgroups being a measured
  specialisation later (AR-3), and its quad reads exchange
  through workgroup memory; the trace gathers each 8×4 tile's bits by
  workgroup atomics into a storage texture, where Wicked ORs them into a
  buffer, so the trace binds no storage buffer beyond lit group 0's and
  group 1's eight; one invocation of each denoiser pass covers the four
  denoised slots, a slot a lane, sharing the depth, normals, velocity,
  disocclusion and the filters' depth and normal weights, which Wicked's
  dispatch for each light computes again, a lane whose tile upstream
  would skip taking the skip's values, and the denoiser does not run while
  none of the four holds a light; the denoiser's scratch packs its two
  halves in a word (`pack2x16float`), a slot a lane, through one pair of
  functions both passes share, and its history is
  filtered bilinearly by the pass, its motion is the surface's in UV and
  its previous depth the stage's own, linear; and the directional light's
  rays
  ignore the cascades' distance, which bounds the maps alone, so a
  ray-traced shadow reaches as far as the scene. The lights in a pixel's
  mask, the slots, the filter taps and the upsample's four texels are the
  stage's loops, each a constant (AR-12). The history is the stage's
  ([History](#shared-contracts)), restarting after a frame in which the
  stage did not run. The browser has no ray queries and never runs it; a
  device without them, or the setting off, keeps the maps and reports it
  (`Renderer::ray_traced_shadows_in_effect`, which reports the setting in
  effect: on, with hardware ray tracing in effect). The effective
  configuration's `Effective::ray_traced_shadows` is resolved in two
  steps. Before prepare it means the frame traces for the shadows (the
  setting on and hardware ray tracing in effect), so prepare builds the
  acceleration structures. After prepare the renderer narrows it
  (`renderer::effective::traced_shadows`) to whether the stage runs: the
  frame's rays trace in hardware and a slot holds a light. Only then does
  the opaque stage drop its fused form; a frame whose slots hold no light
  keeps the fused pass and the maps. `Settings::ray_traced_shadows`
  is a `bool`, off by default and in every preset: a game opts in, as it
  opts in to the hardware ray tracing the setting needs, whose ray queries
  wgpu marks experimental ([D-28](decisions.md)).
- **Opaque and masked surfaces.** Direct, baked and ambient light are computed
  in the forward pass, never from the G-buffer; environment specular and
  reflections are computed from it. Ambient occlusion is applied after the
  forward pass, by source completion, to the ambient diffuse (environment
  diffuse and the hemisphere fill, or the irradiance volume's or the
  dynamic GI volume's irradiance where it stands in for them) that the pass
  records apart, as completion
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
  as it supplies any surface: its mesh, the normals it animates (its
  material's normal layers, which move with the frame's time and upload
  nothing, so a static instance can hold them; or a caller-generated mesh
  replaced with `set_model`, or a deforming instance), and its material's
  roughness, F0 and coat; simulation, level and animation policy stay in
  the game. SGL3D owns the rest. Godot, Bevy, Wicked Engine and Filament
  trace opaque surfaces only (Godot b130438 `render_forward_clustered.cpp`, SSR before
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
  diffuse whole. Where the irradiance volume lights a surface, its sky
  visibility multiplies the visibility that specular occlusion takes, in
  completion and in lit shading's environment specular alike, so captures
  and ray hits take the sky's visibility alone
  ([Irradiance volume](#designs-that-span-stages)). A
  screen-space method traces the surface ([Surface](#shared-contracts)),
  converted at its adapter, and returns premultiplied radiance and
  confidence ([D-17](decisions.md)); world-space rays, from the opaque
  surfaces, fill its misses and skip pixels under a receiver; one
  composition adds the opaque lobes, and under a receiver gives the opaque
  lobe its fallback alone, since the result there is the receiver's. The
  lobes' responses and directions, their specular occlusion, which lobe is
  traced, the method's cutoff test and fade, and the formula response ×
  (reflected × fade + fallback × (1 − confidence × fade)) have one owner,
  `shading/specular_lobes.wgsl`,
  which completion and composition, lit shading (probe captures, ray hits
  and blended surfaces) and the G-buffer's traced normal and roughness call.
  World-space rays reach what `Settings::world_space_reflections` says:
  `Moving`, the moving instances alone (a nearest hit among them, then
  static visibility to it, so a static blocker leaves the probes and sky in
  charge), the reach the portable BVH affords at half resolution; or `All`,
  one nearest hit over both kinds excluding the receiver's own triangle
  (`scene_trace_nearest_except_receiver`), static geometry included, so an
  off-screen wall reflects as it stands rather than as its probe recorded
  it, as Wicked Engine's RT reflections trace the whole scene (2ff1d9e
  `rtreflectionCS.hlsl` 74–81, every instance in its reflection mask):
  meant for the hardware path ([Hardware ray
  tracing](#designs-that-span-stages)) and allowed on the portable one at
  the cost its BVH walk takes, every ray walking the static BVH where
  `Moving`'s rays walk it, any-hit, only up to a moving hit (#23's
  world-ray workload: the
  trace 2.2 times `Moving`'s on the portable path, 1.2 times on the
  hardware path). Both reaches write the same targets, shade a
  hit through the one function and compose by the one formula.
  Another method plugs in beside the existing ones.
- **Deformation.** Skinned and morphed positions reach every geometry pass the
  same way, with the previous frame's positions for motion. Prepare's deform
  stage morphs and skins each deforming instance whose deformation changed
  since the last submitted frame once, in one compute pass, from its packed
  rest pose into its own `f32` vertices in the ray source (Bevy's skinning
  and morph math, run once per frame as Wicked Engine's `skinningCS` runs
  it, chosen over skinning in every pass's vertex shader by measurement,
  #347): its positions, in the
  slot that does not hold the last submitted frame's, and its normals and
  tangents. Pulled passes read them through the object record, with the
  other slot's positions for motion; casters, which read positions from a
  vertex buffer for every instance, read its slot. Culling uses each mesh's
  skinned bounds (Bevy's `SkinnedMeshBounds`, grown by the weighted morph
  displacements). A deforming instance is moving, so no static layer or
  probe capture holds it, and the portable path's rays do not see it, as
  Bevy's ray-traced scene leaves out meshes with joints; the hardware path
  builds it a BLAS over its deformed positions after each deform, as
  Wicked refits its skinned meshes, so its rays see it
  ([Hardware ray tracing](#designs-that-span-stages)).
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
- **Irradiance volume.** Diffuse light the game computes over a lattice of
  cells and keeps up by region (a voxel world's propagated sky and block
  light at a metre, a level's bake), which every receiver within the lattice
  samples by its position: a port of Bevy's irradiance volume (revision
  9d12036: `crates/bevy_pbr/src/light_probe/irradiance_volume.rs`, the voxel
  texture and its bindings, its layout at 36-57 and its case for ambient
  cubes over spherical harmonics at 71-114; `irradiance_volume.wesl`, the
  sample, 51-74; `light_probe.wesl`, the containing test and the border
  weight, 126-141; `render/pbr_functions.wesl` 683-711, the lightmap, else
  the volume, else the environment's diffuse; `bevy_light/src/probe.rs`
  338-363, `IrradianceVolume`). Each cell is an ambient cube, Valve's six
  colours along +X, −X, +Y, −Y, +Z and −Z (Mitchell 2006, as Bevy cites
  it), the `AmbientCube` a moving instance carries, and a receiver takes the
  three faces its normal points to, weighted by the normal's squared
  components, as Bevy samples (irradiance_volume.wesl 59-73) and as
  `ambient_cube_irradiance` already blends an instance's cube: one formula,
  one owner. Taken: Bevy's one 3D texture of the six faces, (Rx, 2Ry, 3Rz),
  the negative faces below the positive and the X, Y and Z faces in turn
  along z (36-57), filtered by three hardware trilinear taps, which Bevy
  chose its cells for over the fetches that packed spherical harmonics need
  (84-95); the sample clamped to the edge cells' centres (54-57), so any
  filtering sampler serves and `baked_sampler` does; values in irradiance /
  PI on the directional lights' scale; and the volume's place between the
  charts and the environment's diffuse. Changed, each a decision beside the
  port's provenance: a face is one RGBA16F texel, not Bevy's RGB9E5: its rgb
  the face's own light (irradiance / PI from the world's emitters, and
  whatever bounce the game's field carries) and its a the sky's occlusion
  toward that face, one less the share of the frame's ambient (the
  environment's diffuse light and the hemisphere fill) that reaches the cell
  from that side, so a zero texel is the fallback. The sky changes with the
  frame, and a cell that holds it baked, as Bevy's do, rewrites every cell
  when it does (78.6 MB over 160 × 128 × 160 m at 1 m in Bevy's format)
  where a visibility rewrites none, as Frostbite's baker outputs sky
  visibility beside its lightmaps and irradiance volumes (O'Donnell,
  Precomputed Global Illumination in Frostbite, GDC 2018, p. 4) and
  Unreal's stationary sky light bakes its occlusion as a bent normal so its
  colour changes at runtime; the sky-specular occlusion below needs the
  visibility apart from the colour; and RGB9E5 has no fourth channel, so a
  visibility beside it costs a second texture and binding and either a
  second set of taps (a face's visibility, 30 bytes a cell) or its direction
  (a cell's, 25 bytes). RGBA16F holds both in one texture at three taps, 48
  bytes a cell against Bevy's 24 (157 MB against 79 MB for that volume,
  5.3 MB against 2.7 MB for a relight of 27 chunks of 16³), the encoding
  every baked irradiance source shares (`irradiance_half`), its 10-bit
  mantissa above RGB9E5's 9. RGBA8 in an sRGB encoding under a maximum the
  game declares for the volume, Bevy's 24 bytes a cell, keeps the dark
  values, the curve spending its codes on them, about two and a half
  decades below that maximum; its cost is the maximum itself, a field of
  the placement the game must choose and above which light clips, and the
  range beneath it, so it is not taken while memory allows and is the
  measured fallback should the consumer's route prove memory the limit
  (Bevy's case against LDR cells, 84-95, is against a scale per cell, which
  breaks the hardware's filter, not against one for the volume). A game
  whose bake holds the sky,
  as Bevy's Blender bakes do, writes it into rgb at an occlusion of 1 and
  has Bevy's volume at a static sky. One volume, an axis-aligned lattice in
  the render frame as the dynamic GI volume is, in place of Bevy's eight
  transformed cubes blended per fragment: no consumer needs several or a
  rotated one, and the determination has one place for it. A scroll
  ([Scene content](#scene-content)) copies the cells that stay to their new
  texels in place, through a stripe a chunk (16 cells) thick across the
  axis, and clears the ones that enter, as Godot's SDFGI scrolls its
  cascades' textures and probe history by a copy when its camera crosses a
  cell (sdfgi_preprocess.glsl `MODE_SCROLL` 174-183, gi.cpp 2121-2175), in
  one command buffer the scene submits at once, so the region writes the
  game makes next land on the moved field; the faces share one texture,
  whose filter cannot wrap within a slab, so the cells are not stored
  toroidally as the dynamic GI volume's probes are, and Bevy's clamp stands.
  Considered and not taken: stacking the six faces along y alone, (Rx, 6Ry,
  Rz), whose x and z a sampler could repeat so a horizontal scroll moved no
  cell; it is taken up only if the example's measurements show the scroll
  copy hitching. The volume's share is 1 within the extent, fading to 0 over the one cell
  past each face, as the dynamic GI volume's share fades past its edge,
  through the one fade function (AR-2), so a receiver hands over to its
  fallback without a seam; Bevy's falloff is not taken: its default is 0 and
  its ramp lies inside the cube. Before the clamp the position is offset
  along the receiver's geometry normal, not its shading normal, by half a
  cell, the offset that lands a receiver on a cell face at the adjacent
  cell's centre, so a voxel face reads the air cell before it, never the
  solid one behind it, and the hardware filter smooths it across the face's
  corners; the precedent is Godot's SDFGI `normal_bias` (gi.glsl 195; 1.1
  cells by default, environment.h 161), while its VoxelGI's (gi.glsl 538)
  defaults to 0 (voxel_gi.h 53). The faces are blended by the shading
  normal, as Bevy's `N` and an instance's cube are. Not taken: Bevy's
  `intensity` and `affects_lightmapped_meshes` (a charted receiver keeps its
  bake, as with the dynamic GI volume) and its per-view clustering of
  volumes. A receiver the volume lights takes a(n) × ambient(n) + rgb(n),
  the blended visibility (one less the blended occlusion) and the blended
  own light, in place of the environment's diffuse light and the hemisphere
  fill, through the one path the dynamic GI volume's irradiance takes,
  recorded apart as the ambient that ambient occlusion weights: a(n) scales
  both terms of the ambient, of which the environment's keeps the surface's
  `environment_scale` and the hemisphere fill has none, and rgb(n) takes no
  `environment_scale`, as the dynamic GI volume's irradiance takes none; at
  a dynamic GI probe's hit the field is taken whole, `DYNAMIC_GI_BOUNCE`
  damping the dynamic GI volume's own share alone. The field holds the
  metres-scale visibility of caves and overhangs, the screen-space occlusion
  the sub-metre, as Godot occludes its VoxelGI's and SDFGI's ambient by its
  AO (scene_forward_clustered.glsl 2141); the rest of its share, at its
  border, comes from what follows it in the determination. Across its
  extent it covers the dynamic GI volume and the ambient cubes: authored
  light wins, an unwritten cell reading as the frame's ambient whole, and a
  game that wants the dynamic GI volume or its cubes in a region leaves that
  region uncovered. The sky's visibility occludes the sky's specular too, as
  Frostbite's sky visibility and Unreal's baked sky occlusion occlude their
  sky light (O'Donnell p. 4; Unreal's Sky Lights), where Bevy's volume
  touches no specular: where the volume lights a receiver, a(n), the
  visibility blended at its normal, multiplies the visibility from which
  Lagarde's occlusion is derived (Lagarde and de Rousiers 2014; Filament's
  `SpecularAO_Lagarde`, `specular_occlusion` in
  `shading/specular_lobes.wgsl`, the one owner of the specular lobes, so
  completion and `shade_lit` call one function, AR-1), for the sky's share
  of a lobe's environment specular alone: a probe captured in the cave
  already shows the cave, as Unreal's sky occlusion occludes its sky light
  and not its reflection captures, so probe specular keeps the occlusion
  completion derives from the ambient visibility. It applies in completion
  for the camera's opaque surfaces, which read a(n) from the alpha of the
  ambient target ([G-buffer](#shared-contracts)), in `shade_lit`'s
  environment specular for probe captures, world-space ray hits and blended
  surfaces, and in the sky's share of a receiver's traced lobe's fallback; a
  surface without ambient occlusion (a capture, a hit) takes the sky's
  visibility alone. It does not touch what a trace returned (the
  screen-space method's radiance, a world-space ray's hit), which the scene
  itself occludes, nor direct light from any light, nor emission. It is no
  chart: a receiver it lights still takes baked scene lights, so a fixture
  the game writes into the field is not also a scene light, or the game
  leaves the field's share of it out, as for ambient cubes; and it is
  game-authored lighting, which `FrameInput::baked_lighting` turns off with
  the charts and cubes. It has no pass and takes no place in the stage
  order: region writes go through the queue when the game makes them,
  prepare uploads nothing for it, and lit group 0 lends its texture to
  every view that shades, so the camera's opaque, masked and blended
  surfaces (receivers included), probe captures, world-space ray hits and
  the dynamic GI probes' hits sample one field. The fog's ambient share
  does not sample it; Godot's volumetric fog takes its VoxelGI and SDFGI
  (volumetric_fog_process.glsl 705-760), the practice to follow in a change
  of its own. The game owns the field's values, lattice and extent, when it
  scrolls and which regions it writes and when; SGL3D owns storage,
  encoding, layout, upload, sampling and composition, and nothing about it
  is a setting (S3D-6). It costs 48 bytes a cell, three 3D taps per
  fragment it lights, each region write's six faces once through the queue
  (one write per face slab, packed before the write on the game's thread)
  and, on a scroll, two copies of the cells that stay (into the stripe and
  out of it), a clear of the ones that enter, and the stripe and a stripe
  of zeros, kept once for each axis scrolled along; a frame uploads
  nothing. A filtered RGBA16F 3D texture written and copied by region is core WebGPU,
  so the browser runs the same volume within its `maxTextureDimension3D`.
- **Dynamic diffuse GI.** Coloured bounce light from the frame's lights, the
  scene's lights, emitters and the sky on static and moving surfaces, from a
  volume of probes the game places ([Scene content](#scene-content)), kept up
  by rays through the scene's ray source each frame within a budget: a
  port of Wicked Engine's DDGI (`ddgi_rayallocationCS`, `ddgi_raytraceCS`,
  `ddgi_updateCS`, `ddgi_updateCS_depth`, `ShaderInterop_DDGI.h`, after
  Majercik et al. 2019 and 2021). Each probe's irradiance is the bordered octahedral colour map
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
  bounce is kept, without df44c3d's further division by π, which dims every
  bounce by π: the colour map holds irradiance / π, what a hit reflects per
  unit of its diffuse colour, as 95e357f bounces it. The stage runs first
  after prepare ([Frame](#frame)). Each probe asks for rays as Wicked's
  allocation does: its most rays scaled by its inconsistency, a tenth of
  that outside the camera's frustum, in buckets of four and at least four;
  a probe not yet blended its most, as Wicked serves every probe on the
  first frame after a restart, and a probe that enters by a scroll
  likewise. Its most rays fall with the log2 of its distance from the
  camera in the least spacing, from the tier's most within one spacing to
  an eighth of it at 128, as Wicked's surfels' rays fall by their level
  (4323a33c `SURFEL_RAY_BOOST_MAX` to `_MIN`). A frame traces at most the
  tier's budget of rays, its fixed rays included: 128 probes at the tier's
  most, as Wicked's surfel GI traces at most its `SURFEL_RAY_BUDGET` a
  frame, which #120 had left to the per-probe maximum alone. A blended
  probe traces on its turn alone, once in a period that grows with that
  level, from every frame within one spacing to every eighth at 128 and
  doubling beyond to every 32nd, at a phase its hash staggers, as Wicked's
  surfels re-trace by their distance level (`SURFEL_RAY_UPDATE_PERIOD_MAX`
  and `_CAP`), keeping its light between turns; so a volume whose content
  keeps moving costs at most its budget, its near probes keeping up the
  most. Three improvements on Wicked (RD-2). Where its requests past the
  budget trace nothing in dispatch order, so some may starve, every period
  is lengthened by the least power of two (the stride) under which the
  blended probes' requests on their turns fit the budget beside the probes
  that start, so each probe keeps its turns, near ones the more often; a
  probe's phase is Wicked's within its period plus whole periods its hash
  chooses, so its turns under a longer stride are among its turns under a
  shorter one and a stride that changes from frame to frame skips none
  (phases that did not nest starved probes while the stride alternated);
  what still exceeds the budget traces nothing, as Wicked's. A probe whose
  light is changing, its most inconsistent texel above the estimator's
  noise (0.2, below which Wicked's estimator catches a texel up at its
  least), also takes turns at its period shortened toward one as its
  inconsistency rises to one, from the rays the frame's turns and starting
  probes leave, so it lengthens no other probe's turns; neither Wicked nor
  RTXGI shortens periods (Wicked's inconsistency sets rays per turn, RTXGI
  leaves scheduling to the application), so this is SGL3D's own. Counted
  toward the stride, those turns doubled every probe's period on
  Hyperdrive's course from noise alone; taken from what is left, they keep
  a moved lamp answered as quickly (in the `dynamic_gi` example, 90% within
  about 100 frames at High, against about 160 without them and 50 tracing
  every probe every frame), and the noise threshold keeps a still scene's
  probes from them (#196). And where Wicked starts every probe of a
  restarted volume in one frame, a hitch on a large volume, probes not yet
  blended start at their most rays, the nearest the camera first (a
  histogram of their distances in the least spacing), with the budget the
  blended probes leave, at least half of it, all of it after a restart,
  leaving the shortened turns none while they wait; a probe not yet started
  traces nothing and weighs nothing, so its receivers keep their fallback,
  and the blends run over the probes that traced, which Wicked's, whose
  probes always trace, need not. Each probe's
  estimator, depth and offset start afresh when it is first blended, where
  Wicked starts them all on the first frame. A scroll moves no probe: each
  is stored at its lattice coordinate plus the volume's scroll, wrapping, in
  the probe texture and the stage's buffers alike (`ddgi_probe_stored`), and
  the frame's data carries the scroll, as RTXGI's infinite scrolling volume
  stores its probes by its probe scroll offsets (practice only); a frame
  that scrolls first clears the planes that enter, in the allocation's
  pass, which then start as probes not yet blended through the ramp, as
  RTXGI clears its scrolled planes. Wicked's volume does not scroll. The
  rays are traced through `scene_trace_nearest` over both kinds and both
  sides of every triangle, which the hardware path replaces
  underneath as Wicked's `ddgi_raytraceCS_rtapi` replaces its software
  trace ([Hardware ray tracing](#designs-that-span-stages)). A single-sided
  material met from behind (the inside of closed geometry, or the outside
  of a shell built to be seen from within) brings no light and shortens
  the ray's depth to a fifth, as Majercik et al. 2021 and RTXGI's probe
  trace (practice only) treat back faces, so the probe takes nothing from
  behind the surface and receivers beyond it weigh the probe as occluded;
  a double-sided material's back face is a surface, shaded, its depth
  pushed in to 0.9 as Wicked pushes it. A hit is shaded through
  `shade_ray_hit` for its diffuse
  radiance alone, as the fourth shadow receiver kind, the probe hit: its
  direct light is one light drawn uniformly from the frame's directional
  lights and the volume list's lights the hit takes, times their count, its
  visibility one any-hit ray from the hit toward the light (a point's or
  spot's position, a directional light's direction, and for a rectangle a
  point drawn uniformly on its face, as Wicked draws it) through the one
  acceptance predicate over both kinds and both sides
  (`scene_segment_visible`; on the hardware path a query whose traversal
  ends at its first accepted hit, `TERMINATE_ON_FIRST_HIT`),
  as Wicked's `ddgi_raytraceCS` samples one light per hit with one shadow
  ray, and never a shadow map; that visibility takes the light's shadow
  opacity through `shadow_opacity_visibility`, and at or below
  `SHADOW_OPACITY_CUTOFF`, or for a light that casts no shadow (a scene
  light without `casts_shadow`, a directional light without the frame's
  cascades), the ray is not cast and the light reaches the hit
  unoccluded, as it reaches every other receiver (S3D-5: one lighting
  model across GI) and as Godot's VoxelGI traces only a light with a
  shadow, so a casting light the camera's atlas has no room for, or that
  lies outside the camera's view or its cascades, shadows a hit as any
  other and nothing it lights leaks through walls into the probes; its
  indirect diffuse by the one determination
  ([Surface shading](#shared-contracts)), which gives an unbaked hit the
  volume's own last frame, damped, and so bounces without end; and its
  emission. A material that does not emit into GI (`emits_into_gi` false,
  `MATERIAL_EMITS_INTO_GI` clear: a glowing fixture a scene light stands
  for, which the probes would otherwise take twice, from the fixture their
  rays meet and through the light) gives a probe hit none of the light it
  gives off itself, its emission and an unlit material's whole colour; the
  hit still ends the ray and, lit, reflects the light that reaches it. That
  is Unity's emission GI flag None (`MaterialGlobalIlluminationFlags.None`,
  practice only), which keeps a material's glow out of its GI while the
  object stays a GI contributor, the one engine control aimed at a glowing
  surface's light. Not taken: Godot's `GeometryInstance3D.gi_mode`, Unity's
  Contribute GI and Unreal's Affect Dynamic Indirect Lighting, which take
  the whole object out of GI, occluder and bounce surface too, and would let
  the probes' rays through a lamp to what lies behind it; the per-light GI
  scales (Godot's `Light3D.light_indirect_energy`, Unity's
  `Light.bounceIntensity`, Unreal's Indirect Lighting Intensity), which
  remove the light's own bounce, the share the probes should keep, and
  leave the emitter's duplicate. Wicked's DDGI gathers every hit's emission
  (df44c3d `ddgi_raytraceCS.hlsl` 502, under an inclusion mask of 0xFF)
  and Bevy Solari's emissive meshes are its lights, so neither has a
  control. It is the material's content (S3D-6), as a light's `specular`
  pairs with a fixture reflected as an emitter, and `shade_ray_hit` applies
  it for the probe-hit receiver alone, so the portable and hardware paths
  share it and world-space reflection hits and probe captures show the
  fixture glowing. A miss takes the environment's radiance along the ray as the
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
  has been blended and is active or dormant, RTXGI's probe data), sized for the
  installed placement
  and lent through lit group 0 to every view that shades, so the camera's
  surfaces, blended surfaces, probe captures, world-space ray hits and the
  probes' own hits sample one volume, as Wicked's world-space DDGI applies
  in every camera. The sample is Wicked's `ddgi_sample_irradiance`: the
  eight probes about the point, weighted trilinearly, by a backface test and
  by Chebyshev visibility from the probe's depth moments, reading irradiance
  by the surface normal. Improved on Wicked (RD-2) twice. The weights are
  RTXGI's (practice only): the backface test is the whole wrap-shading
  weight and the weight's floor RTXGI's, where Wicked's hard backface test
  and higher floor tie every probe of a receiver facing a nearby wall at the
  floor, so it takes the light beyond the wall. Visibility is tested from
  the point offset by Majercik et al. 2021's self-shadow bias toward the
  viewer and the normal, scaled by the least spacing, in the form of the
  paper's reference implementation in G3D (practice only), where Wicked
  offsets it a millimetre along the normal, so a surface does not shadow
  itself against the probes the wrap weight lets weigh. The receiver's cell
  and trilinear weights stay its own position's, where G3D and RTXGI take
  them from the offset point, so a surface's light does not shift as its
  viewer moves. A receiver nearer a wall than that bias, facing it and seen
  head-on, tests visibility from beyond the wall. Each probe is classified,
  as RTXGI classifies its probes (practice only): one more than a quarter
  of whose fixed rays meet single-sided surfaces from behind, inside
  geometry or beyond a wall, is inactive, weighs nothing in the sample and
  traces the fewest rays. Its fixed rays are RTXGI's 32 directions spread
  evenly and never rotated, unshaded and not blended, as RTXGI blends none,
  so its class holds still while what it sees does; where RTXGI traces all
  of them every update, a probe's first turn is classified from the share
  of its rays that met back faces, its second traces all of them, which
  classify it, and its next seven none, so its first turns cost no more
  rays than any (all of them on its first turn tripled the probes a moving
  camera leaves waiting to start at High, about 6.7 times at Low, the budget leaving starting probes their
  share), then it traces four each turn after its others and is
  classified again from all of them once a cycle of eight turns (the share
  of each frame's rotated rays, even blended over frames, wandered across
  the threshold, and a far probe's first turn traces as few as 32, whose
  class it had kept for its whole first cycle). Its second phase finds
  whether a fixed ray met a front face within the probe's cell, the
  spacing about it on each axis. Improved on RTXGI
  (RD-2), which deactivates a probe without one for every receiver,
  such a probe is dormant: static receivers skip it, so a probe diagonally
  beyond the edge or corner of a room of single-sided walls, which sees few
  of their backs and so passes the first phase, lights no wall; moving
  receivers keep it, so a moving instance in open space is lit from its
  first frame, where RTXGI's probes about it wait for their fixed rays to
  find it; and it traces the fewest rays, but one whose cell the world
  bounds of a drawn moving instance reach into traces as an active probe, so
  the light about the instance keeps up with it. The renderer gives the
  stage those bounds each frame, at most `DDGI_MOST_MOVING_BOUNDS` (256) of
  them, the nearest the camera, the cap on the allocation's walk over them
  (AR-12). A static object so small that no probe's fixed rays meet it
  within their cells takes its other indirect light, and a probe's class
  follows a change in what it sees within a cycle of its turns. A probe not yet blended,
  inactive, or dormant for a static receiver weighs nothing, and a receiver
  whose eight probes all weigh nothing keeps its fallback; a probe ray's hit
  takes the volume's own zero there instead, as Wicked's and RTXGI's hits
  sample their volumes, so the probes' bounce starts from their own light
  and never from the sky's fallback, which would carry the sky about a
  closed room for seconds. A volume whose light has converged pauses, as
  RTXGI's sample pauses one whose probe variability (the mean coefficient of
  variation of the active probes' irradiance texels) has settled
  (RTXGI-DDGI f33e496, samples/test-harness/src/graphics/DDGI_VK.cpp
  1629-1637 and DDGI_D3D12.cpp 1239-1246, practice only): it traces
  nothing and its probes hold their light while what that light follows
  holds still. That is the scene's edits that change what the rays see or
  light (the scene counts them; the transient effects and a value set to
  what it was are none, and a deforming instance's pose and deformation
  are none on the portable path, which never sees it, and edits while
  hardware ray tracing is in effect, whose rays do), the frame's data but for the camera's cascades and
  the clock (whose animation phase counts while an opaque or masked
  material's surface moves, `Material::surface_moves`, which the scene's
  materials count; the rays pass through blended ones), the
  environment bound, the quality, whether the probe hits' light list takes
  the scene's lights (a diagnostic setting) and the placement. Improved on
  RTXGI (RD-2), whose sample pauses below a threshold each scene sets,
  which SGL3D has no scene to ask for: the volume has converged once the
  mean of its variability over a window of 16 turns of every active probe
  (16 times the longest period among them, the stride included, so the
  slowest has taken 16 turns as RTXGI's every probe has in 16 frames;
  each frame's variability weighed by the share of the probes that
  blended) falls by less than a tenth from the last window's, the plateau
  RTXGI describes, and never while a probe has yet to start. The allocation decides it on the GPU from
  the blends' last windows, so nothing is read back, and a paused frame
  costs the allocation alone. A receiver the volume lights takes its
  irradiance in place of the environment's diffuse light and the hemisphere
  fill, recorded apart as the ambient that ambient occlusion weights; beyond
  the volume's extent a receiver keeps its fallback, the volume's share
  fading to nothing over the one probe spacing past its edge, as RTXGI's
  volume blend weight fades, so no seam shows there. Lightmapped and
  atlas-charted static receivers keep their bake and the ambient as before,
  and a receiver the irradiance volume lights keeps it ([Irradiance
  volume](#designs-that-span-stages));
  moving instances and other unbaked receivers take the volume where it
  lights them. Ambient cubes stay as
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
- **Hardware ray tracing.** Where the device has wgpu's
  `EXPERIMENTAL_RAY_QUERY`, the scene keeps acceleration structures beside
  its portable BVHs, and every scene ray traces through them while
  `Settings::hardware_ray_tracing` is on: the world-space trace's queries,
  the dynamic GI probe rays and their visibility rays, and the ray-traced
  shadow rays, through the one set of functions and one hit ([Ray
  source](#shared-contracts)), so the stages that trace do not change.
  Wicked Engine builds its RT reflections, DDGI and RT shadows on one
  `RayQuery` beside its software BVH (2ff1d9e `raytracingHF.hlsli`;
  `rtreflectionCS.hlsl` 74–147; `ddgi_raytraceCS_rtapi.hlsl`), and Bevy
  Solari traces its scene through one `trace_ray` over its TLAS (v0.19.1
  b56fc29 `crates/bevy_solari/src/scene/raytracing_scene_bindings.wgsl`
  96–104, resolving a hit by instance and primitive at 159–162). AR-3: the
  stage specialised on an optional device feature, the portable path its
  fallback where the device lacks the feature or the setting is off,
  reported by `Renderer::ray_tracing_in_effect` and `ray_tracing_error`.
  The setting exists because the trade is real: Metal traces in software
  before M3, and the structures cost memory, so a game chooses. It is off
  by default and no preset turns it on: a game opts in by requesting the
  feature and turning the setting on, the owner's decision
  ([D-28](decisions.md)), which no later change reverses.
  *Device.* wgpu 30 marks the feature experimental: `request_device`
  refuses it unless the descriptor's `experimental_features` is
  `ExperimentalFeatures::enabled()`, an `unsafe` token the game gives
  (S3D-1: the game owns the device), so `graphics_device::features` does
  not include it; `graphics_device::ray_tracing_features(adapter)` returns
  it where the adapter has it, as `fsr2_features` is separate, and the
  consumer guide and examples request it with the token.
  `graphics_device::limits` requests the adapter's
  `max_blas_primitive_count`, `max_blas_geometry_count`,
  `max_tlas_instance_count` and `max_acceleration_structures_per_shader_stage`,
  which `Limits::default()` leaves at zero. The adapters: Vulkan with
  `VK_KHR_ray_query` and its acceleration-structure extensions; DX12 at
  ray-tracing tier 1.1 under shader model 6.5, which wgpu's default
  `Dx12Compiler::Auto` reaches through static DXC where the game compiled it
  in or `dxcompiler.dll` beside its executable, and not through FXC, its
  last resort, so the feature is absent until a Windows game ships DXC
  (the README says how; `sgl-3d` enables no `static-dxc`, since Cargo
  feature unification would force it on every game); Metal from macOS 15 on
  a GPU that supports ray tracing from render stages, Apple silicon, in
  hardware from M3; never the browser, whose WebGPU has none. MoltenVK
  offers no ray query, so Vulkan on a Mac has none either.
  *Two forms, one stage.* naga 29's MSL writer could not run a candidate
  loop: it ran one `intersect` at initialisation and emitted nothing for
  `rayQueryConfirmIntersection`. naga 30's lowers the loop through Metal's
  `intersection_query` (`back/msl/ray.rs`: `reset` 363–368, `next` 386–417,
  `commit_triangle_intersection` 472–495), but the candidate form has not
  been validated or measured there, so Metal keeps the baseline by
  recorded decision until #211 settles it. naga 30's SPIR-V and HLSL
  writers lower the loop (`back/spv/ray/query.rs`:
  `OpRayQueryInitializeKHR` with the flags and
  cull mask, `Proceed`, `ConfirmIntersection`, `Terminate`, candidate and
  committed reads at 75–105; `back/hlsl/ray.rs`: `TraceRayInline` with the
  flags and cull mask at 376–380, `Proceed` 424, `CommitNonOpaqueTriangleHit`
  529, `Abort` 547, candidate reads 190–222 and committed reads 86–119).
  The owner's decision: hardware ray tracing works on the Mac in its first
  version, and Vulkan and DX12 are supported as well, with separate code
  where needed. So the hardware path has one **baseline form**, which every
  native backend runs, and one **candidate form**, a specialisation for
  the backends that may run it (Vulkan and DX12; Metal lowers the loop
  under naga 30 but stays on the baseline by recorded decision until
  #211): `RayQueryForm` (`Baseline`, `Candidates`),
  a typed capability the renderer derives once from the adapter's backend
  (`RayQueryForm::of_backend`: Metal the baseline; Vulkan and DX12 may run
  candidates), selects the
  per-query module a tracing pipeline composes and the geometry flags the
  scene builds with; nothing else branches on it. The form in effect on
  Vulkan and DX12 is the baseline until the candidate form's benefit is
  measured on their hardware (RD-6), a recorded decision kept as a
  constant beside `RayQueryForm` (`shading::LOWERED_FORM`), never a
  setting (AR-3); the owner has no Vulkan or DX12 hardware that traces
  rays, so the candidate form has run on no device yet. Shared by both
  forms, in one hardware module, `scene_rays_hardware.wgsl`: the TLAS
  binding, the query's setup from the ray and its side policy, the
  re-trace, the conversion of a committed hit into the one hit, the
  composition with the portable walk's result and the one per-ray budget;
  and beyond it the instance entries and the one hit decode, the
  acceleration structures with their building, budget, rebuilds and
  compaction, the shared predicate, the settings and reporting, and the
  stage placements. Per form, one function, the query itself, which the
  shared module calls: `scene_rays_query_opaque.wgsl` (baseline) and
  `scene_rays_query_candidates.wgsl` (candidates). It takes the ray, the
  interval's current start, the kinds, whether it asks for any hit, the
  predicate's inputs (receiver, side policy, open end) and the ray's
  budget, and returns a miss, a committed hit for the shared predicate
  and re-trace (`SCENE_HARDWARE_COMMITTED`), or one it accepted itself
  (`SCENE_HARDWARE_ACCEPTED`, a confirmed candidate), so no hit is judged
  twice; the baseline's judges nothing and takes no step.
  *Baseline form.* Every BLAS geometry is `OPAQUE` and a query asks the
  hardware for two things, which every backend enforces, the cull mask
  that selects the kinds (naga 30's MSL passes it to the query's `reset`,
  `back/msl/ray.rs` 365–368; SPIR-V and HLSL pass it) and the interval,
  with opacity forced (`RAY_FLAG_FORCE_OPAQUE`) so that one proceed, never
  looped on, ends the traversal on every backend: forced-opaque geometry
  yields no candidate, so the first proceed (on Metal one
  `intersection_query::next()`, 386–417) completes it; and for nothing else:
  `CULL_BACK_FACING` and `CULL_FRONT_FACING` reach the SPIR-V and HLSL
  queries but naga's MSL writer sets no triangle cull mode, and a global
  cull would be wrong anyway for a mirrored instance, whose world winding
  is reversed, and for a double-sided material, since `TlasInstance`
  carries no per-instance flag to exempt them (Wicked exempts them so,
  `wiScene.cpp` 4782–4791). The shared predicate's checks that need no
  triangle solve judge the committed hit (the visibility group, the alpha
  mode, the side under the ray's policy from the triangle's object-space
  winding, the receiver's own triangle and the interval's open end), and a
  rejected hit **re-traces**: the ray keeps its origin and direction and
  its t_min becomes the next `f32` above the rejected distance (an
  absolute step rounds back to the same distance far from the origin and
  repeats the hit), so t_min rises strictly and the ray ends once t_min
  passes t_max, as Vulkan requires t_min ≤ t_max (a dynamic GI ray's t_max
  is `f32::MAX`). An acceptable triangle at exactly a rejected one's
  distance (back-to-back single-sided quads, coplanar meshes of a hidden
  and a shown group) is skipped, since one opaque query cannot list ties: a
  limitation stated here, which the oracle test pins by accepting either
  triangle of an exactly tied pair, since which one the hardware commits
  is arbitrary. A nearest ray asks
  nearest queries throughout. A visibility ray asks for any hit first
  (`TERMINATE_ON_FIRST_HIT`; Metal's `accept_any_intersection`): an
  accepted hit occludes whatever its order and a miss is unoccluded, each
  one query; a rejected hit carries no order, so the ray then asks nearest
  queries from its original t_min, stepping past each rejected nearest
  hit, so no nearer occluder is skipped. The cost: under
  `SCENE_SIDES_SHADOW` every closed single-sided occluder's entry face is
  rejected, so an occluded shadow ray costs one query when the hardware's
  first hit is the exit face and three when it is the entry face, two on
  average, what nearest queries from the start would cost it; under
  `SCENE_SIDES_BOTH` a rejection is rare (a hidden group, a blended mesh
  of a mixed model), so a dynamic GI visibility ray is one any-hit query
  almost always, where an all-nearest form would make it a slower nearest
  query; hence the two steps. The two steps lose where an unoccluded ray
  crosses an open single-sided surface that faces the receiver, such as a
  sky dome seen from inside: three queries against two; a game keeps such
  a mesh off rays with `capture_visible = false`. The hardware's `front_face` is never read:
  winding conventions differ by backend, and the predicate derives the
  side from the positions it reads anyway. A mirrored instance (a pose
  with a negative determinant) is in the TLAS as any other, its back faces
  rejected and re-traced; a second BLAS built under a mirror transform
  (`USE_TRANSFORM`) to spare that re-trace is an implementation choice
  measured on a consumer that mirrors much. What the predicate cannot
  judge from a committed hit cheaply is a masked material's cut-out texel,
  which needs the hit's UV and a texture sample per crossing, so, under
  the baseline form, a non-deforming instance whose model has a masked
  mesh is a **predicate instance**, left out of the TLAS and covered by the
  portable walk; a deforming instance has no portable BVH, so its committed
  hits take the cut-out test (the hit's UV from the rest-pose packed
  vertices, which deformation leaves) and re-trace under the budget, and
  masked hair or cloth cuts out on the Mac too.
  *Portable coverage.* One rule for both forms: on a hardware-traced frame
  the portable walk covers every capture-visible, non-deforming instance
  the TLAS does not hold, the predicate instances, the instances whose
  model's BLAS is pending and the instances left out at the device's
  limits, with no second copy of scene data: the scene builds its two
  instance BVHs over them alone, from the same entries, leaves naming the
  same indices, the moving one every traced frame as before and the static
  one when its set changes (a static edit; a material's alpha mode edited,
  which moves every instance of every model using it between the TLAS, the
  walk and neither, so the scene keeps each model's ray class from its
  meshes' alpha modes and recomputes it for the material's users through
  the material's use list, a cost proportional to its users; a pending
  BLAS built or an instance left out; the form or the setting), the
  header's roots those BVHs' and the walk module composed into the
  hardware module as into the portable one ([Ray source](#shared-contracts)).
  The scene keeps a material's use list of the models that name it and a
  model's list of the instances that name it, or the implementation adds
  them where the scene keeps reference counts alone. During ramp-in the
  static BVH rebuilds every frame as pending BLASes complete, a cost PR 3,
  which restricts the walk to these instances, measures on the streaming
  example. Every ray then takes the nearer of its two results,
  hardware and portable (a visibility ray is occluded by either), and the
  walk keeps its one shared visit budget. An instance whose model has only
  blended meshes is in neither: rays pass through blended surfaces.
  *Candidate form.* Where the backend may run it (Vulkan and DX12), a masked mesh's
  geometry is built without `OPAQUE` and its instance joins the TLAS: the
  hardware reports each of its triangles as a candidate, and the per-query
  function's candidate loop (`rayQueryProceed`,
  `rayQueryGetCandidateIntersection`, `rayQueryConfirmIntersection`) runs
  the whole predicate on it, the cut-out test included, confirming an
  accepted candidate, so masked content runs through hardware candidates
  instead of the portable walk, as Wicked's `wiRayQuery` confirms its
  alpha-tested candidates (`rtreflectionCS.hlsl` 82–109;
  `screenspaceshadowCS.hlsl` 232–256) with `FLAG_OPAQUE` on every other
  material (`wiScene.cpp` 4232–4244). Opaque geometry never yields a
  candidate, so the re-trace and the side rules stay as the baseline has
  them; the candidate form does not take the side rules, since Wicked's
  per-instance cull exemptions are not available. The loop stops at the
  budget's cap by returning a miss. The portable walk then
  covers the pending and left-out instances alone. A BLAS is built for
  its geometry and its geometries' opacity under the form in effect, so
  under this form a
  material edit that moves a mesh between masked and not re-pends every
  model using it, rebuilt outside the budget before the next traced frame,
  as Wicked rebuilds (4232–4247), and a deforming instance's BLAS is made
  again with its new flags. The form reaches a device only through error
  scopes (validation and internal) around its module and pipeline
  creation, popped on native by polling, a scope still pending counting as
  a failure, sticky for the renderer's device (`DeviceRayForm`, which
  `TracePaths::pipeline` consults): a failure (a compile or validation
  error; not a wrong result or a driver fault) falls the device back to the
  baseline form, typed in `RayQueryForm`, and `ray_tracing_error` says so
  while `ray_tracing_in_effect` stays true. The frame in which it fails
  finishes on the baseline over structures built for candidates, which the
  baseline traces correctly, since it forces opacity and its committed
  masked hits take the cut-out test and re-trace; the next frame builds for
  the baseline.
  *Budget.* One budget per ray, `SCENE_MOST_HARDWARE_STEPS`, counts every
  query the ray starts and every candidate it examines, nested loops
  sharing it (AR-12); at the cap the query stops by returning and the ray
  reports a miss, or a visibility ray unoccluded, as the portable walk does
  at its visit cap. The constant sits above the most steps a ray took on the
  examples and the consumer's content, counted on the GPU by an
  instrumented build of the hardware module, since the CPU oracle's brute
  force does not reach their scale (grazing rays
  through masked foliage, where the candidate form's candidates are every
  non-opaque triangle crossed; shadow rays through nested closed
  occluders), its reason beside it; the portable walk's visits are not
  that measure. The candidate form's steps are unmeasured until it runs on
  Vulkan or DX12 hardware; until then the cap is the baseline's.
  *Structures.* A model that does not deform owns one BLAS, one geometry
  per mesh, over the packed vertices' `f32` positions and the mesh indices
  where they lie in the ray source, whose buffer gains `BLAS_INPUT`
  (`Float32x3`, which needs no extended format, at the 32-byte stride;
  `Uint32` indices), as Bevy builds one BLAS per mesh from its mesh
  allocator's slices (`scene/blas.rs` 55–102, 144–171) and Wicked one per
  mesh LOD (`wiScene_Components.cpp` 1372–1421). `first_vertex` counts
  whole strides, so a mesh's vertex block and a model's range start at an
  eight-word boundary ([Prepared geometry](#scene-content); `scene::ranges`
  allocates aligned), which moves the portable path's words and nothing
  else. Flags `PREFER_FAST_TRACE | ALLOW_COMPACTION`, Bevy's (161–162): a
  game changes a rigid model's geometry by replacement, never by refit. A
  deforming instance owns a BLAS of its own over its deformed positions
  (`DEFORMED_POSITION_WORDS`, a 12-byte stride, its range, which its
  position slots start, placed at a three-word boundary) and its model's
  indices, built whole again after
  the deform pass in every frame that deforms it, with `Build` and
  `PREFER_FAST_BUILD` (wgpu 30 builds `PreferUpdate` as a full build,
  `command/ray_tracing.rs` 1106, 1126, so `ALLOW_UPDATE` buys nothing until
  it refits), outside the budget, as Wicked refits its skinned meshes'
  BLASes every frame (`wiScene.cpp` 4246–4252; `wiRenderer.cpp`
  5749–5760): a crowd of 64 instances of the skinned example's model is
  measured in PR 2, and a nearest-first cap that keeps the rest's last
  BLAS is added only if that calls for it. So deforming casters and
  reflectors are in the TLAS as moving instances: a deforming instance's
  entry is written as any instance's is, though only the hardware path
  reads it (its inverse pose and mesh word, for the predicate's
  model-space ray and mesh records), which spares rewriting every one
  when the setting turns on,
  and the predicate and the hit decode take its positions, normals and
  tangents through the object record's deformed slot, as the pulled
  passes read them; the portable path still sees no deforming instance
  ([Deformation](#designs-that-span-stages)). Only models that a
  capture-visible instance names are built, never a level of detail and,
  under the baseline form, never a predicate instance's model. The TLAS is
  built over the capture-visible instances the forms admit (every
  non-deforming instance whose model has a BLAS, less the predicate
  instances under the baseline form and the all-blended ones, plus the
  deforming instances), from their entries: the entry index as the 24-bit
  custom index, the pose as the transform, the kind as the mask (static 1,
  moving 2), where Wicked's masks select by purpose (`wiRenderer.h`
  38–40; `wiScene.cpp` 4749–4794) and Bevy's take every ray (`binder.rs`
  171–176); created with a capacity of at least one, so wgpu builds it
  even when it holds no instance (wgpu-core 30 sizes its scratch from the
  capacity, `device/ray_tracing.rs` 242–255, and skips only a build with
  nothing at all to build, `command/ray_tracing.rs` 303–308), and grown
  with the entry buffer. Builds are frame work: one
  `build_acceleration_structures` call in the frame's encoder, in prepare
  after the deform pass and before the cull stage's early phase, holds the
  frame's pending model BLASes, its deforming instances' BLASes and the
  TLAS, so the TLAS is rebuilt whole every hardware-traced frame, as Bevy
  (`binder.rs` 74–81, 265–267) and Wicked (`wiRenderer.cpp` 5809–5820)
  rebuild theirs; an abandoned frame marks nothing built in wgpu and the
  scene commits its bookkeeping at `finish_frame`, so the next frame builds
  the same again from the entries, which a render origin move has rewritten.
  A model's BLAS is not built at placement but pending until a
  hardware-traced frame, under a budget of vertices a frame (Bevy's
  compaction budget of 400 000, `blas.rs` 19–21, serves the builds too),
  nearest the camera first, the first pending model always admitted
  whatever its size, so a game whose setting is off builds nothing and pays
  no memory, and one that turns it on, or installs a world, ramps the
  structures in over a few frames rather than one, as the dynamic GI
  volume starts its probes (an RD-2 improvement on Bevy, which builds every
  extracted mesh in the frame it arrives); the portable walk covers an
  instance whose model's BLAS is pending until it is built
  ([Portable coverage](#designs-that-span-stages)). A replaced model
  (`set_model`) and a re-pended one are rebuilt before the next traced
  frame outside the budget, so an edited chunk never drops out of a frame's
  rays. A built BLAS is prepared for compaction and, once ready, compacted
  through the queue under Bevy's budget (`blas.rs` 104–142), the TLAS
  taking the compacted BLAS at its next build; compaction stays, as Bevy
  keeps it, its cost unmeasured until wgpu can timestamp acceleration-structure
  work: wgpu 30's `build_acceleration_structures` and `Queue::compact_blas`
  take no timestamp writes, and a compaction's copy runs in the queue's own
  submission ahead of the frame's, outside the frame's pass timings. What the device cannot hold
  is left out and counted, never reaching wgpu's validation: a model beyond
  `max_blas_primitive_count` or `max_blas_geometry_count`, an instance
  beyond `max_tlas_instance_count` (the farthest from the camera first) or
  past the 24-bit custom index, and a BLAS whose allocation fails under an
  error scope all stay off the TLAS and on the portable walk (a BLAS whose
  compaction fails so stays as built); the one
  allocation no scope reaches is the builds' scratch buffer, which wgpu
  allocates when the game finishes the frame's encoder, so its
  out-of-memory error reaches the game's error handler as any failure of
  its encoder does; and
  `Renderer::ray_tracing_stats`, a plain method as `local_shadow_stats`
  is, counts the instances traced in hardware, on the portable walk and
  left out, so a game without the `diagnostics` feature sees them. The
  scene frees the structures when a frame runs with the setting off and
  builds them again when it turns on. The structures' builds and
  compactions count through `counters` and the BLASes held through
  `diagnostic_resources` ([Diagnostics](#shared-contracts)), and a scene
  on a device without the feature holds none of this.
  *Bindings and composition.* The TLAS is bound only by the passes that
  trace, in each tracing stage's group 3 at one entry
  ([Bind groups](#shared-contracts)); the hardware module declares `enable
  wgpu_ray_query;`, which `shading::compose` hoists to the program's head,
  since naga accepts the directive before any declaration only.
  *Costs.* The structures' memory and build times are unknown until PR 2
  and 3 measure them on the streaming example and at Stevecraft's scale
  (thousands of 16³ chunk models, millions of triangles), beside the ray
  source's 35 to 50 bytes a triangle of portable BVH, which the fallback keeps.
  The ray-source buffer is read at build time only, so its growth, which
  copies it to a new buffer, invalidates no BLAS.
  *Validation.* The first step of the implementation is the baseline form
  on the owner's Mac against the CPU oracle: the ray harness
  (`scene::rays::query`) runs the hardware module against a brute-force
  oracle of f64 world-space planes and edges with the predicate's rules,
  the portable tests' technique (`scene::rays::hardware_tests`), over
  masked, blended, single- and double-sided, mirrored,
  moving and deforming content, the receiver's own triangle, nested closed
  occluders under every side policy and the equal-distance tie, on a
  device with the feature; where the adapter lacks it the test reports
  itself unsupported, never passed. The candidate form runs the same test
  wherever its hardware exists (a device whose backend may run it,
  whatever form the renderer takes there by default): no Vulkan or DX12
  device here traces rays (MoltenVK offers no ray query), and Metal, whose
  naga 30 lowers the loop, stays on the baseline until #211 validates the
  form there, so it ships with less local validation
  than the baseline form, a risk recorded here: locally, naga's SPIR-V and
  HLSL writers write both forms' tracing programs as wgpu's Vulkan and
  DX12 backends do (the layout tests), and the scene's structures and the
  fallback are tested on Metal; DXC, the drivers and the candidate loop's
  results are not. Per-pass GPU timings of the world-space trace, the
  dynamic GI trace and the ray-traced shadows against the portable path on
  the examples and the consumer's route are the measured record (RD-6).
  The baseline form and its Mac validation landed first (#188); the
  candidate specialisation is a separate change after it.
- **GPU draw lists and occlusion culling.** The camera's opaque and masked
  surfaces and each directional shadow cascade draw from lists the GPU
  builds, in the form of Bevy's meshlet hardware raster (9d12036
  `crates/bevy_pbr/src/meshlet/`: `cull_instances.wesl`, one thread an
  instance, frustum then occlusion, the occluded pushed to a second pass;
  `cull_clusters.wesl`, one thread a cluster, each that passes appended to
  the raster list by an atomic add on the draw's `instance_count` (79-91);
  `visibility_buffer_hardware_raster.wesl`, one `draw_indirect` whose
  instances are the visible clusters and whose vertex entry emits a NaN
  dummy vertex past the cluster's triangles (31-54, 69-83);
  `visibility_buffer_raster_node.rs` 592; `cull_shared.wesl`, the frustum
  test 44-64 and the occlusion test 123-203, which projects with the
  previous view-projection and previous pose in the first pass and the
  current ones in the second, 170-186), with the two-phase culling, the
  previous-frame reprojection, the depth pyramid and its SPD downsample of
  Bevy's GPU preprocessing (`crates/bevy_render/src/batching/gpu_preprocessing.rs`,
  the modes at 148-166 and the device test at 1545-1603;
  `crates/bevy_pbr/src/render/gpu_preprocess.rs`, the pipeline keys 274-322
  and their specialisation 1450-1489, `early_gpu_preprocess` 790,
  `late_gpu_preprocess` 1006; `mesh_preprocess.wesl` 263-349;
  `crates/bevy_render/src/occlusion_culling/mod.rs` 45-61;
  `crates/bevy_core_pipeline/src/mip_generation/experimental/depth.rs`,
  `early_downsample_depth` 64, `late_downsample_depth` 147, the dispatches
  683-698 and the pyramid 557-612; `downsample_depth.wesl`, FidelityFX SPD
  v2.1, MIT), after Haar and Aaltonen's two-pass culling (GPU-Driven
  Rendering Pipelines, SIGGRAPH 2015). Bevy's meshlet raster is taken over
  its batched GPU preprocessing for the draws because of what each costs
  to encode: a batched list is one indirect command per batch, which wgpu
  issues one at a time on Metal (`wgpu-hal/src/metal/command.rs` 1589-1629)
  and WebGPU (`wgpu/src/backend/webgpu.rs` 3725-3753) and validates one at a
  time where its indirect validation is on (wgpu 30 `instance.rs` 268-274,
  on by default in release builds), so at the consumer's scale (#19: every
  chunk its own model) a frame would encode tens of thousands of commands
  across its views, no better than the CPU walk; a cluster draw is one
  command per set on every backend. Its meshlets are its own asset format
  with a visibility buffer and a software rasterizer (`mod.rs` 94-128
  requires 64-bit atomics); SGL3D takes the draw form alone, over its
  existing range hierarchy, into its existing geometry passes. Wicked Engine
  4323a33 culls by frustum on CPU jobs and occludes by a hardware query per
  object, drawn as its box and read back the frames later its buffers allow
  (`wiRenderer.cpp` `UpdateVisibility` 3640-3830, `OcclusionCulling_Render`
  5854-5900; `wiScene.cpp` 218-250, `UpdateOcclusionResult` 9890-9910);
  Godot b130438 rasterizes authored occluder meshes on the CPU through
  Embree into a hierarchical depth buffer its cull loop tests
  (`renderer_scene_occlusion_cull.h` `_is_occluded` 56-158,
  `modules/raycast/raycast_occlusion_cull.cpp`, `renderer_scene_cull.cpp`
  2797-2800 and 2946-2949); Filament ef1a133 culls by frustum alone,
  vectorised on the CPU (`Culler.cpp`, `details/View.cpp` `cullRenderables`
  1356-1385). Bevy's leaves the CPU, culls every object as its own occluder
  and needs no authored occluders, which a streamed world cannot supply.
  **Candidates and sections.** Beside its object records the scene keeps a
  GPU list of draw candidates (Scene records), one per instance and mesh,
  holding its bounds in the model's space (the mesh's, copied when the
  candidate is placed; a deforming instance's deformed bounds, written with
  its deformation), its object record's index, its mesh's record word, its
  set, its mesh's first vertex in its positions slab (Raster geometry;
  none for a deforming model's mesh), and for a mesh with registered
  alternatives its chain: a record per
  model and mesh of each level's bounds, error and mesh word, at most
  `MAX_MESH_LODS` (8) alternatives, so the chain's walk is
  bounded (AR-12) and `set_mesh_lods` refuses more with the typed error;
  the cull tests the chosen level's bounds, the candidate's own where there
  is no chain.
  Whether an instance is `visible`, `capture_visible`, static or deforming
  are bits of its object record; a material's visibility group and whether
  it casts the directional shadow are its sets' records: an instance edit
  rewrites one record and a material edit its sets', never the candidates.
  A section is a leaf of the mesh's range hierarchy, at most 128 triangles
  in the mesh's own order, and a mesh's section table (each leaf's bounds,
  first index and triangle count) is part of its model's ray words, packed
  by `PreparedModel` and named by its mesh record (Ray source), so the GPU
  culls at the granularity the CPU walk culled at. Blended materials have
  no candidates. A set is what draws with one pipeline, one material and one
  positions slab (the pipeline variant: the material's sides and alpha
  mode, a mirroring pose, and whether the instance deforms, since pulled
  passes reach deformed vertices through the object record; the slab its
  meshes' positions lie in, which a cascade's draw of it binds, one in all
  but a scene past a slab's size) and holds, per phase it draws, a
  region of each GPU-built view's cluster list of the capacity its
  candidates' sections sum to (a view culling a late phase keeps a cluster
  list for each phase, the set's region placed alike in both; a cascade's
  cluster list holds a second, paired region per set past the last early
  one, placed alike), each candidate counting the most sections
  among its chain's levels, and one indirect command per region; regions and commands
  are placed by `scene::ranges` and
  re-placed only when a set outgrows its region, so a streaming frame's
  edits touch the candidates and sets they change and nothing else. The
  scene keeps candidates, chains and sets up as its edits change what they
  describe (an instance added, removed, given another model or a pose that
  mirrors where it did not; a model's geometry replaced; a material's alpha
  mode or sides changed; alternatives registered; a deformation set) and
  uploads what changed as every edit does. The scene also indexes the
  instances whose models hold a blended mesh, so the CPU builder's blended
  walk is over those alone. The walk that built the camera's and cascades'
  lists on the CPU each frame, and grew with the instances (#19), is gone
  from the frame.
  **The cull stage** (`stages/cull`) owns its pipelines and layouts, its
  bind groups over what the renderer lends it, and the camera's depth
  pyramid, and nothing a pass draws from: each GPU-built view's draw list
  (its lists and their counts, the dispatch buffer that drives its indirect
  dispatches, its cluster regions and commands, its statistics words) is the view's, in the view
  layer, lent to the cull stage to write and to the shadow and opaque
  stages to draw (AR-1). Its layouts and caps have one owner,
  `shading::culling` and `culling.wgsl`, tied by the layout test (AR-2); the
  values Bevy pushes as immediates travel in a uniform, so the browser runs
  it. Three dispatches per view and phase: the instance cull, a finalize
  and the section cull. Producers only bump counts in the view's lists; the
  finalize, one workgroup dispatched directly, derives each indirect
  dispatch's arguments from the final counts, clamped to the lists'
  capacities, into the view's dispatch buffer (the early finalize the early
  section cull's and the late instance cull's, whose list is final after
  the early instance cull; the late finalize the late section cull's),
  which no indirectly dispatched pass binds, since wgpu tracks a buffer
  whole and refuses one dispatch's indirect source as its writable storage
  (`wgpu-core/src/command/compute.rs` 323-333, `track/buffer.rs` 755-767),
  as Bevy's `remap_1d_to_2d_dispatch.wesl` derives its dispatch from a
  count: a dispatch past 65,535 workgroups is remapped to two dimensions
  there, in integers (x the lesser of the count and 65,535, y the count over
  65,535 rounded up, linearised by x, where Bevy takes an inexact
  `ceil(sqrt(f32(count)))`), and the finalize writes the count and x beside
  the count in the lists, from which an invocation linearises its index and
  returns past the count, not from `num_workgroups`, which Bevy reads
  (`visibility_buffer_software_raster.wesl` 36-41) and DX12 reports as zero
  for an indirect dispatch without wgpu's validation (`instance.rs`
  209-215). The instance cull, one invocation per candidate (Bevy's 64 a
  workgroup; the early one dispatched directly, its count and x in the
  uniform from the CPU, the late one from the dispatch buffer), tests the
  population by the object's and the set's bits against
  the view's kind and the frame's mask (the camera: `visible` and the
  material's group enabled; a cascade: `capture_visible` and the material
  casting in its group); chooses the level, the last admissible alternative
  by the projected error bound SGL3D evaluates for the CPU builder, which
  moves to `shading` beside its WGSL twin (Bevy's preprocessing has
  visibility ranges only, `mesh_preprocess.wesl` 212-234; the bound is
  SGL3D's own), a cascade admitting level 0 alone, since shadows keep the
  original geometry; tests the frustum with the view's planes and error
  rows, built pose-free on the CPU in double precision as `view::culling`
  builds them, the GPU applying the pose in `f32` under a margin that covers
  its rounding, so the test stays conservative and a cascade's omits its
  near plane; and, the camera's while occlusion culling runs, Bevy's
  occlusion test, the bounds' screen rectangle and nearest depth against
  the farthest under the rectangle in the pyramid (`cull_shared.wesl`'s
  4×4 texels of the finest level that holds the rectangle; hidden only
  strictly behind it, as Bevy's mesh preprocessing compares, so a surface
  square to the camera is not hidden by its own depth; a box that reaches
  the near plane, or that needs a level the pyramid did not build, kept),
  in the early phase at the object record's previous pose
  through the camera history's previous view-projection with that frame's
  jitter, in the late phase at its pose through the frame's jittered one,
  as Bevy projects with the jittered matrices and re-applies the previous
  jitter to the previous ones (`crates/bevy_render/src/view/mod.rs`
  1214-1226, `crates/bevy_pbr/src/prepass/mod.rs` 782-799). A candidate that
  passes joins the phase's visible list as (candidate, mesh word), the
  chosen level's mesh; one the early phase finds occluded joins the late
  list, which the late instance cull runs over; each list's capacity is the
  candidate count, so an append always has a slot. The section cull takes a
  mesh's section count and table from the mesh record it reads, so it binds
  no chain. The section cull, one workgroup per visible candidate
  by indirect dispatch, its threads striding the chosen level's sections,
  tests each section's bounds as the instance's were (frustum; the camera's
  phases' occlusion): one that passes has its draw instance (Scene records:
  object, mesh word, mesh-relative first index, triangle count, and the
  candidate's first vertex in its positions slab where it draws the
  candidate's own mesh, as a cascade always does) appended to
  its set's region for the phase (a cascade's section whose triangles
  pair, to the set's paired region and command), at the slot an atomic
  add on the set's command's `instance_count` returns, while that slot is within the region's
  capacity; an append past it subtracts its add back, so the count ends at
  the capacity and the draw stays within its region (wgpu's indirect
  validation zeroes a draw whose instances pass the bound buffer,
  `wgpu-core/src/command/render.rs` 430-460, and without it the next region
  would be overwritten), as Bevy's `cull_clusters.wesl` appends to its
  raster list, adding the section and its triangles to the view's
  statistics by mobility once it is appended within capacity; one the early
  phase finds occluded joins the
  view's late section queue as (its early visible entry, which holds the
  candidate and mesh word, and the section), as Bevy's
  first pass pushes an occluded cluster to
  its second-pass queue (`cull_clusters.wesl` 50-58). A deforming
  candidate's sections are all appended untested, since its leaves' bounds
  are its rest pose's and only its instance bounds are deformed, as the CPU
  builder culls it whole. Each phase's visible list, the late list and the
  late section queue, each with its count, are one buffer of the view's,
  the lists, with capacities that make every append a slot: the candidate
  count, and the queue's the cluster list's length, at least the sum over
  candidates of the most sections among each one's levels, as a region's;
  past a capacity a producer skips its write and the finalize clamps the
  count it reads, so a wrong capacity loses a list entry and never a bound
  (AR-12), except the queue's: an occluded section that finds no slot there
  is appended to the early set, drawn rather than lost. The late section cull is one
  dispatch whose workgroups the late finalize derives from both counts: one
  per candidate the late instance cull passed, striding its sections as the
  early cull does, then one per 64 queue entries, a thread each, so no
  invocation walks a mesh's sections serially. The view writes its commands
  whole in its per-frame reset, from the scene's set table, each early,
  late and paired command one five-word `DrawCommand` that reads as either
  indirect form: `SECTION_VERTICES` (384) vertices, first vertex 0, first
  instance 0 and a zero count for a pulled draw, and the same words as 384
  indices, first index 0, base vertex 0 and first instance 0 for a paired
  one, so a reallocated list starts right and the scene writes nothing of
  the view's (AR-1); the reset zeroes the lists' counts too, and
  the dispatch buffer is the finalize's. The
  executor (Draw lists) then issues, per set with candidates and per phase,
  one `draw_indirect`, binding the pipeline, the material and the set's
  region as the draw-instance vertex buffer, so a draw's instances are its
  sections and no builtin carries an offset: `INDIRECT_FIRST_INSTANCE` is
  not needed, the vertex entry reads its draw instance as every geometry
  pass does, and DX12's builtins are right with or without wgpu's indirect
  validation (`instance.rs` 209-215). The frame records one draw per set and
  phase, a cascade one more per opaque set for its paired sections, and
  nothing per instance. The pulled vertex entry takes the
  mesh-relative index from the draw instance's first index plus the vertex
  index, as it takes the vertex index now, so the primitive identity and
  the ray source's rebase are unchanged, and emits a dummy vertex past the
  triangle count: a fixed finite point outside the clip volume, not Bevy's
  NaN, which WGSL leaves indeterminate and Metal's fast math may fold; a
  GPU-built cascade's casters take the same entry's caster form (the
  unclipped-depth variants for a device without `DEPTH_CLIP_CONTROL`): each
  vertex's index from the ray source, as the entry takes it, and its
  position from the positions slab the CPU-built casters draw from
  (Raster geometry), at the draw instance's first vertex, through the
  set's slab group, which the executor binds as group 3 where it changes;
  a deforming instance's from its deformed vertices and a mesh's without
  slab positions (a deforming model's) from the ray source; a masked
  material's texel coordinates and colour from the ray source, as now.
  A cascade's opaque set (its record's `SET_PAIRS`) draws its paired
  sections, those whose triangles pair (Ray source: each even triangle and
  the next are (a, b, c) and (a, c, d), a quad split `[0, 1, 2, 0, 2, 3]`
  in index order and starting on an even triangle, as the examples'
  meshers and Blender's quads give them, a lone last triangle aside), with
  one `draw_indexed_indirect` of its paired command over one fixed index
  buffer (`PAIRED_INDICES`: four slots a pair, drawn (a, b, c), (a, c, d))
  whose instances are its paired region's sections, the paired caster
  entry taking each slot to the corner it stands for, so the
  post-transform cache shades the corners a pair shares once, as the CPU
  builder's indexed draws did; the rest it draws pulled. Opaque casters
  take no screen-space derivatives, so an indexed draw of them is
  deterministic on Apple GPUs (`GeometryPass::pulled`); a masked
  material's casters sample its base map with implicit derivatives
  (`material_base_color`), so a masked set's sections stay pulled, and the
  camera never pairs. Other quad splits (`[0, 1, 2, 2, 3, 0]`),
  cache-optimised index orders and pairs shifted onto odd triangles do not
  pair, and get the slab positions' saving alone.
  The depth-only cascades are bound by the bytes their vertices fetch and
  then by their vertex invocations: a slab position is 12 bytes, a ray
  source vertex record 32, and a section's 384 pulled vertices are half as
  many again as an indexed draw shades of its quads (#192, below). Per-pass GPU time and the encode time per
  view on Metal and Chrome are measured against the CPU builder's at #19's
  scale before the CPU builder's camera and cascade populations are deleted,
  and the section cull's one workgroup a candidate, which idles most of its
  lanes on a one-section mesh, against one thread a section (RD-6).
  **Cascade cost (#192).** #190's cascades, pulling 32-byte records 384
  vertices a section, cost 0.22–0.25 ms more than the CPU builder's
  indexed draws on the `streaming` walk and 0.53–0.61 ms more at its
  headroom scale (Apple M5, median of the four cascades' timing groups,
  two runs each). Measured on those routes before choosing among #192's
  candidates: sections are 88–94% full (each mesh's sections are its
  128-triangle runs, only the last partial), and 192 more dummy vertices a
  section cost 0.02–0.06 ms, so commands bucketed by fill would save about
  0.01 ms; the fixed pattern alone, over the records, saved 0.03–0.16 ms,
  dense positions alone 0.13–0.38 ms, and both together 0.20–0.65 ms. The
  routes' meshers emit only paired quads, so every section there pairs.
  As shipped, against main: the walk 0.76 → 0.56–0.57 ms (before #190
  0.50–0.53), headroom 1.88–1.94 → 1.31–1.32 (1.32–1.33), the
  `irradiance_volume` cave 0.32 → 0.26; in Chrome the walk window 0.79 →
  0.59, the headroom window 2.62–2.69 → 1.84–1.90 (#190 measured 0.59 and
  2.03 before it); every final frame identical, bit for bit. On such
  content that restores the CPU builder's cost; on shared-vertex content
  it does in part: across 100 glTF models from the owner's games, sections
  are 87% full, 19% pair (61% of their consecutive triangle pairs match,
  but a section pairs only when all its pairs do), and a section holds
  0.41 distinct vertices a corner, which an indexed draw would shade once
  each, so most of their sections keep the slab positions' saving alone. Pairing is SGL3D's own
  (RD-2): Bevy's meshlets draw 384 pulled vertices a cluster with no
  reuse, and Wihlidal (GDC 2016) keeps reuse in GPU-driven draws by
  writing compacted index buffers each frame. Not taken, on a memory argument and
  a timing estimate rather than a measurement: Wihlidal's compacted index
  buffer written each frame (GDC 2016), which reaches any mesh's reuse
  but would write and read the visible sections' indices again each
  frame, about 36 MB at headroom, which at the M5's bandwidth is about
  what the reuse saves there, and hold an index buffer as large as 384
  indices a region slot, 39 MB a view at headroom. Also not taken: the
  cascades' own occlusion culling, whose two pyramids a 2048-texel cascade
  a frame would cost about 0.2 ms each (#199 measured 0.1 ms for a 1080p
  one), more than its whole regression; and Bevy's software raster of
  small clusters (`visibility_buffer_software_raster.wesl`, 64-texel
  clusters, `cull_clusters.wesl` 77), a second rasterizer with texture
  atomics the measured fix does not need.
  **Two phases in the two-pass form.** Prepare runs the early phase for
  every GPU-built view after deform, and the shadow and opaque stages draw
  from its commands. With occlusion culling off, the opaque stage keeps the
  form the device gives it, fused or two-pass, and draws the one set. While
  occlusion culling runs, the stage takes its two-pass form whatever the
  device and the renderer interleaves the stages: the G-buffer pass over the
  early set; the cull stage's late phase, which builds the pyramid from the
  opaque depth (Bevy's `downsample_depth.wesl`: R32Float, the depth rounded
  down to a power of two, its minimum per texel, the farthest surface in
  reversed-Z, so the test is conservative), runs the late instance cull over
  the late list, its finalize, then the late section cull over the
  candidates it passed and the queue, so a section hidden last frame inside
  an instance that was not is tested again; the G-buffer pass over the late
  set, loading its targets, and the anisotropy fallback at `Equal` over
  both sets where the device needs it; the cull stage's pyramid, built again
  from the complete depth for the next frame's early phase, as Bevy
  downsamples after its late prepass; then, while ray-traced shadows run,
  their stage; the sky; and the lighting pass once over both sets at
  `Equal`, as Bevy's main pass draws both sets once
  (`occlusion_culling/mod.rs` 51-61), followed by ambient occlusion; the
  lighting and anisotropy passes draw each set's early region then its
  late, in the G-buffer passes' set order. The
  fused pass never splits: splitting it would load and store every opaque
  target a second time, where the G-buffer split loads the G-buffer alone
  and the lighting pass shades each pixel once at the G-buffer's depth. The
  setting's cost is therefore the two-pass form's second geometry pass
  beside the late G-buffer pass's load and store and two pyramid builds,
  measured against the fused form on both routes (RD-6). Wicked's and
  Godot's culling has no late phase: an object occluded a frame ago shows a
  frame late, which the late phase avoids. Two-phase culling corrects
  itself: an instance or a section the early phase sets aside is tested
  again late against this frame's depth, so a pyramid or history that
  mismatches the frame costs time, never a surface. The per-frame counts and lists are
  zeroed by the frame's encoder before the early phase, so an abandoned
  frame leaves nothing stale; the pyramid is the cull stage's history
  (History), the last executed build's, which an origin move leaves
  untouched (screen-space depth, while the previous poses and matrices the
  early phase projects with are translated, Scene content); a frame whose
  history restarts, or whose last submitted frame built no pyramid, runs
  its early phase by frustum alone, as Bevy's first frame reads a
  zero-initialised pyramid, far in reversed-Z, that culls nothing.
  **Capabilities and fallbacks.** GPU draw lists need what S3D-1 requires
  already: compute and indirect execution, core on every backend and in
  WebGPU; no feature is requested for them and no device draws these views
  another way. Occlusion culling needs the pyramid's six storage textures
  per stage, which `graphics_device::limits` requests from the adapter
  (WebGPU's default is four); a device with fewer culls by frustum alone,
  and the effective configuration reports it
  (`Renderer::occlusion_culling_in_effect`). The cull pipelines bind at
  most seven storage buffers to a compute stage, within S3D-1's floor of
  eight (the instance cull: candidates, chains, the object records, the
  sets, the lists, the statistics; the section cull: the lists, candidates,
  the object records, the ray source, the sets, the cluster regions, the
  commands with the statistics; the finalize: the lists and the dispatch
  buffer). Bevy keeps CPU culling and direct draws on
  WebGPU, whose preprocessing-only mode is gated on `IMMEDIATES`, twelve
  storage textures and ten storage buffers (`gpu_preprocessing.rs`
  1563-1600); SGL3D culls on the GPU there with the same draws as native
  (D-27). wgpu validates indirect draws by a compute pass per render pass
  on native backends unless the game's instance clears
  `InstanceFlags::VALIDATION_INDIRECT_CALL` (not on WebGPU); the draws here
  are few and offset-free, so nothing SGL3D does needs it either way, and
  the package README says what the flag costs. The pyramid's port carries
  Bevy's MIT/Apache-2.0 notice and AMD's MIT notice for SPD in a provenance
  header, since Bevy's file has none.
  **Order and ownership.** GPU-built lists draw their sets in set order and
  a set's sections in the order they passed, which an atomic decides, so two
  coplanar surfaces have no defined winner at equal depth: the CPU builder's
  guarantee that each instance's meshes draw in their model's order is
  withdrawn for the views it no longer builds, with a migration note (a
  coplanar overlay takes a depth offset or a decal). Statistics are per
  view, counted on the GPU (visible sections and their triangles by
  mobility: a cluster draw mixes mobilities, so a GPU-built view counts
  sections where a CPU-built one counts draws, and its draws are the
  commands encoded) and
  read back without blocking, the map requested at `finish_frame`, never
  within `render`: `Renderer::geometry_stats` describes the most recent
  completed frame, with the blended list's CPU counts of that same frame,
  and reports none until a frame completes; `diagnostic_draws` reports the
  commands encoded, synchronously; `geometry_stats_for_model` needs the
  `diagnostics` feature, which reads back each candidate's visible sections.
  Probe captures touch neither the cull stage nor the pyramid. Its caps
  (AR-12): the frustum's six planes, eight corners, `MAX_MESH_LODS` + 1
  levels of eight corners each, `MAX_MESH_SECTIONS` (65,536) sections a mesh,
  which `PreparedModel` refuses beyond with the typed error, striding 64 a
  workgroup, the queue's 64 entries a workgroup, and the pyramid's fixed
  mips; the finalize's clamp and two-dimensional remap bound every indirect
  dispatch.
  **Not taken**, each for a reason: Bevy's batched GPU preprocessing for the
  draws (above); Wicked's occlusion queries (a query and a draw per object,
  results frames late, the CPU still walking the objects) and Godot's
  authored occluders; reusing Crystal's or Velvet's depth hierarchy, which
  keep each texel's nearest depth for ray marching (`sgl-post-fx`'s
  `SSR_ComputeHierarchicalDepthBuffer.wgsl` `UpdateClosestDepth`;
  `godot_reflections_hiz.wgsl` 24-27, a maximum in reversed-Z) where the
  occlusion test needs the farthest, and are built after the receivers, at
  the trace's resolution, later than the late phase needs one; Bevy's
  single-layout pyramid build of twelve storage textures under immediates
  (`depth.rs` 399-430): SGL3D's two layouts of six, the second sampling mip
  6, and a uniform, so the floor is six; Bevy's depth prepasses and
  splitting the fused pass (above); GPU-built lists for the local-light
  faces (their cluster cache draws only the faces that changed, #14), probe
  captures (authoring, unculled) and the blended list (sorted on the CPU, as
  Bevy sorts its transparent phase); and a CPU builder kept as a fallback
  for the GPU-built views, a parallel path AR-3 does not allow and that no
  device needs. The cascades take occlusion culling in a change of its own
  once the camera's is measured on the consumer's route: a pyramid and a
  split depth pass per cascade, which Bevy has only in part (its cascades
  carry `OcclusionCullingSubview`, `light.rs` 2044-2053, with GPU culling
  of shadow passes still a TODO at 2058), so it is SGL3D's design, not a
  port; #192 found its pyramids would cost more than the cascades'
  whole regression on the examples' routes (Cascade cost, above).

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
12. **AR-12 — Loops end.** Every GPU loop (`loop`, `while`, `for`) has an
    explicit upper bound that no data can raise: a named compile-time
    constant, or a product of such constants. A pipeline-overridable
    constant set from a Rust constant counts as one. A count read from a
    buffer is not a bound, nor is one only clamped to a buffer's length,
    which can still mean millions of iterations. The bound counts every
    iteration one invocation makes, nested loops included: walks nested in
    one another share one budget, not caps per level whose product dodges
    the rule. Data may end a loop earlier, never later. This covers BVH traversal, a ray query's candidate loop, ray marching, light, decal and probe
    list walks, particle updates, linked lists, work queues, culling passes
    and anything else whose termination depends on data. Each cap has one
    owner (AR-2), with a Rust twin tied by the layout test where it crosses
    the boundary; it sits generously above the legitimate worst case, with
    the reason beside it, citing engine practice where there is some (AMD's
    FidelityFX SSSR caps a ray's `max_traversal_intersections`). A loop that
    reaches its cap fails safe and defined: a ray reports a miss, a list
    stops. A walk along links also makes strict progress, stopping at a link
    that does not lead forward. Corrupt data then costs a wrong answer,
    never a hung GPU, which freezes the machine's display with it.

## Open questions

- None at present.
