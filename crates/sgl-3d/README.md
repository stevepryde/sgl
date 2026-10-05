# SGL3D (`sgl-3d`)

Rust/wgpu 3D rendering for native and browser (WASM + WebGPU) games, extracted
from Hyperdrive and consumed as part of SGL. The game supplies geometry, materials, lights, environments, camera data,
and final presentation poses. The library retains GPU resources and encodes
rendering on the game's device and command encoder.

Agents building a game start at [SGL3D for game agents](docs/README.md), which
lists the features and settings. The [SGL3D specification](../../specs/sgl3d.md)
owns the contract and rendering validation rules. [3D development](../../docs/3d-development.md) explains the
integration, native and in the [browser](#browser-wasm--webgpu). This crate does not own a window, event loop,
input, simulation, protocol, save schema, editor, or ECS. It does not depend on
winit or an input package; wgpu supplies platform graphics dependencies.

## Reference map

Start with the [agent guide](docs/README.md) for a short integration path.
This page is the detailed reference:

- [Frame lifecycle](#retained-scene-and-frame-lifecycle), [data conventions](#dependencies-and-data-conventions), [browser differences](#browser-wasm--webgpu)
- [Lights and look](#frame-lights-and-look), [local lights](#point-spot-and-rectangle-lights), [shadows](#local-light-shadows), [decals](#decals)
- [Reflections](#reflections), [TAA, SMAA and FSR2](#temporal-anti-aliasing), [ambient occlusion](#ambient-occlusion)
- [Exposure and grading](#exposure-bloom-and-colour-grading), [motion blur](#motion-blur), [fog](#volumetric-fog)
- [Specular probes](#baked-specular-probes), [diffuse lighting](#baked-diffuse-lighting), [irradiance volume](#irradiance-volume), [dynamic GI](#dynamic-diffuse-gi), [asset limits](#asset-and-environment-limits)
- [Skinning and morphs](#skinned-meshes-and-morph-targets), [scrolling normals](#scrolling-normal-layers), [mesh LOD](#spatial-mesh-lod), [soft effects](#soft-additive-effects), [heat shimmer](#bounded-heat-shimmer)
- [Settings and fallbacks](#settings-and-capability-fallback), [GPU timing](#gpu-pass-timing), [diagnostics](#validation-and-diagnostics)

## Dependencies and data conventions

Use this workspace crate from the same SGL revision as other SGL packages.
Use the wgpu dependency in the [workspace manifest](../../Cargo.toml). A game
may share one device/queue with the `sgl-2d` UI or 2D renderer; the game owns
pass ordering and presentation.

Use the public `sgl_3d::glam` re-export for matrices and vectors passed to this
crate. SGL3D, `sgl-2d`, and `sgl-core` share the workspace glam dependency,
so their matching vector and matrix types can cross package boundaries directly.
Dependency versions live in the [workspace manifest](../../Cargo.toml); the
[package manifest](Cargo.toml) selects the features used here.

| Input | Convention |
| --- | --- |
| Positions, light ranges, atmosphere dimensions | Metres, +Y up |
| Camera | Right-handed, local forward -Z; reversed-Z depth (1 near, 0 far), `perspective` builds the infinite-far projection |
| Shadow maps | Reversed-Z (clear 0, nearer = greater). SGL3D fits directional cascades from the camera; the game gives their reach in metres of view depth |
| Matrix arrays | Column-major; view-projection is projection × view |
| Camera cuts | `FrameInput::camera_cut` restarts history; no renderer-owned camera movement |
| Pose | Final presentation transform, after game-owned interpolation |
| Light/material values | Linear RGB; base/emissive images are sampled as sRGB |
| Light directions | Where the light shines (directional lights, spots, rectangles' faces) |
| Light units | Point, spot and rectangle intensity in candela (a rectangle's along its normal), directional illuminance in lux, on one scale |
| Environment/backdrop yaw | Radians about +Y |
| Frame/history identity | Previous submitted rendered frame, independent of simulation ticks |

Instance poses may contain nonuniform or mirrored scale. Supply finite,
nonsingular affine transforms; normals use the inverse transpose, and authored
front/back sides are preserved across raster, shadow, and ray paths. Shadow
casters cull as the camera does, for the directional light and local lights
alike: a single-sided material casts from its front faces, a double-sided one
from both. Author open single-sided geometry that must shadow from behind as
double-sided. A masked material casts from the texels it keeps; a blended one
casts no shadow ([Alpha-masked and blended
materials](#alpha-masked-and-blended-materials)).

The game describes directional lights, the hemisphere fill, the environment's
lighting and backdrop, the atmosphere and shadow projections with typed
`FrameInput` values ([Frame lights and look](#frame-lights-and-look)), and
SGL3D packs its GPU layout itself; point, spot and rectangle lights are
[scene content](#point-spot-and-rectangle-lights). The same inputs feed raster, ray
and probe-capture shading. Visibility groups are required bits in the frame’s enabled mask (zero
is unconditional). Directional shadow casting is a separate material choice;
enabling a group does not silently disable its shadows.

## Frame lights and look

```rust,ignore
use sgl_3d::{Backdrop, DirectionalLight, DirectionalShadow, EnvironmentLight, HemisphereLight};
frame.directional_lights[0] = Some(DirectionalLight {
    direction: Vec3::new(0.4, -1., -0.5), // where it shines
    color: [1., 0.85, 0.7],               // linear RGB
    illuminance: 3.,                      // lux
    shadow: Some(DirectionalShadow {
        distance: 150., // metres of view depth that are shadowed
        cascades: 4,    // 1 to 4
    }),
    shadow_opacity: 0.8,  // lets a fifth of its light through its shadow
    ..Default::default() // fog_energy 1: its full light in the fog
});
frame.hemisphere_light = HemisphereLight {
    sky_color: [0.2, 0.3, 0.5],
    ground_color: [0.05, 0.03, 0.02],
    intensity: 0.2,
};
frame.diffuse_environment = EnvironmentLight { yaw: 0.5, ..Default::default() }; // intensity 1
frame.reflection_environment = EnvironmentLight { yaw: 0.5, ..Default::default() };
frame.backdrop = Backdrop::Environment { yaw: 0.5, brightness: 1. };
```

- Up to two directional lights (Bevy's `DirectionalLight`, Godot's
  `DirectionalLight3D`). A light with zero illuminance, or a zero or
  non-finite direction, is off. The first light that is on and has a
  `shadow` casts it and draws its cascades, even at a `shadow_opacity` of
  at most 0.001, which shows none; the other is unshadowed. `fog_energy` scales the light it scatters in the
  [volumetric fog](#volumetric-fog), and `shadow_opacity` how dark its
  shadow is, as a scene light's do. `DirectionalLight::default()` is Godot's
  `DirectionalLight3D`: white, shining along -Z, π lux (its light energy of
  1, which it scales by π), no shadow (opacity 1 when it has one) and fog
  energy 1.
- The shadow is Bevy's cascaded shadow map, which SGL3D fits from the camera
  every frame: the view depth from the camera's near plane to `distance` is
  split into `cascades`, each a map of 2048 texels (1024 at the Low
  `Settings::shadow_quality`). SGL3D places the splits as
  Godot's `DirectionalLight3D` does by default: each cascade but the last
  ends 0.1, 0.2 and 0.5 of the way from the near plane to `distance` (two
  cascades split at 0.1, three at 0.1 and 0.2), and the last at `distance`,
  which is kept at least 1 mm beyond the near plane, as Godot keeps it, and
  at most 8192 m, the top of Godot's `directional_shadow_max_distance` range; `shadow: None` alone turns the shadow off.
  `DirectionalShadow::default()` is Bevy's 150 m and 4 (cascades ending at
  about 15, 30, 75 and 150 m); a `const` builds from
  `..DirectionalShadow::DEFAULT`, the same values. A cascade
  keeps one size and moves in whole texels, so a still shadow does not
  shimmer as the camera moves and turns (a change of field of view or of
  these values resizes it). Each cascade overlaps the next by a fifth of its
  far bound, and surfaces blend between the two there. Beyond `distance`
  nothing is shadowed. Casters between the light and a cascade still cast
  into it: their depth is unclipped, through
  `wgpu::Features::DEPTH_CLIP_CONTROL` where the device has it
  (`graphics_device::features`) and emulated in the caster's shader where it
  does not. Each cascade's map reaches 20 m toward the light beyond its
  part of the view (Godot's default pancake), so a caster within
  that margin is recorded at its own depth and one beyond it at the
  margin's edge. Receivers are offset along their geometry normal (not the
  normal map's) by Bevy's 1.8 texels (times √2) and toward the light by 2
  cm, so no bias is authored.
  While TAA or FSR2 runs the camera's surfaces filter with Jimenez's 8-tap
  spiral, turned per pixel and per frame for them to resolve; otherwise, and
  in probe captures and ray hits, with Castaño's fixed 9-tap kernel. At the
  Low shadow quality the camera's surfaces take one hardware 2×2 comparison
  instead, Godot's hard filter, with Godot's mobile map sizes. A probe
  capture fits its own cascades about its centre; it and world-space ray
  hits take the first cascade whose map holds a surface, which the pancake
  makes a nearer, finer one for a surface toward the light from a cascade's
  part of the view. Timing groups
  `directional shadow cascade 0` to `3`, nearest first.
- `hemisphere_light` is Three.js's `HemisphereLight`: diffuse irradiance
  blended from `ground_color` facing down to `sky_color` facing up.
- The frame environment lights each lobe through its own `EnvironmentLight`
  (yaw and intensity, by default unturned at intensity 1, as Three.js's
  scene environment): `diffuse_environment` the diffuse lobe, and
  `reflection_environment` the specular lobe wherever no baked probe does
  (source completion beyond the probes, probe captures and ray hits, and the
  sky captures record). Bevy's `EnvironmentMapLight` and Filament's
  `IndirectLight` scale both lobes with one intensity; set both fields alike
  for that. `backdrop` is what the camera sees where there is no surface: the
  environment's panorama or one colour.
- `fog` (`Fog`) is the frame's participating medium
  ([Volumetric fog](#volumetric-fog)) and `mist` (`Mist`) the look of the
  scene's mist billboards: their colours, opacity, size and `drift`, how
  fast and which way their noise moves (`Mist::default()` hides them, and
  drifts them slowly up and to the left); both draw while
  `FrameInput::atmosphere` (off by default, as Godot's fog) and
  `Settings::atmosphere` (which allows them, on by default) are on.

## Exposure, bloom and colour grading

```rust,ignore
use sgl_3d::{AgxLook, AutoExposure, BloomParameters, ColorGrading, CompensationCurve, Exposure};
frame.exposure = Exposure {
    stops: 0., // applied before tone mapping; +1 doubles the light
    automatic: Some(AutoExposure {
        // log2 average luminance -> stops of compensation, linear between points
        compensation: CompensationCurve::new(&[[-6., -3.], [0., -2.5]])?,
        correction_min: -1., // the most it darkens, in stops from `stops`
        correction_max: 2.,  // the most it brightens
        ..Default::default() // Bevy's speeds and an even mask
    }),
};
frame.bloom = BloomParameters { intensity: 0.1 };
frame.color_grading = ColorGrading {
    agx_look: AgxLook::Punchy, // Filament's look; None (the default) adds none
    ..Default::default() // Bevy's sections and white balance, unchanged
};
```

- The frame has one exposure, which FSR2 and the tone map both read. With
  `automatic: None` it is `stops`. Otherwise it is Bevy's auto exposure,
  metered from the complete HDR frame at the render size, before
  antialiasing: a 64-bin histogram of log2 luminance (after `stops`) from -8
  to 8, weighted by `metering_mask` (a 16×16 grid), averaged without the
  darkest and brightest 10% of samples (Bevy's default range and filter,
  which SGL3D keeps). The target correction brings that average to 2 raised
  to the compensation curve's stops (-2.5 is about middle grey); `stops`
  shifts the frame into the range. The correction follows it at
  `speed_brighten` stops per second when the scene got brighter and
  `speed_darken` when it got darker, by `FrameInput::frame_time_ms`, slowing
  within 1.5 stops of it (Bevy's default), and stays within
  `correction_min..=correction_max`. A camera cut, a target-changing resize,
  another scene or a switch from a fixed exposure sets it to its target.
  Timing group `exposure`.
- Bloom is Bevy's energy-conserving bloom: a mip chain 512 texels high (a
  scene too wide for that, such as a window one pixel tall, takes the
  device's widest texture instead), 13-tap downsamples (the first with a
  Karis average against fireflies), 3×3 tent upsamples blended level by
  level, and the result mixed into the scene.
  `intensity` is how much light scatters, 0 none; SGL3D shapes the halo with
  Bevy's `Bloom::NATURAL` low-frequency boost and high-pass. Light only
  moves, so emitters bloom by being bright, with no threshold. The chain is
  `Rg11b10Ufloat` where the device renders to it
  (`graphics_device::features`), else RGBA16F. Timing group `bloom`.
- The tone map multiplies by the exposure, applies Bevy's `ColorGrading`
  (hue, white balance, and saturation, contrast and ASC CDL for shadows,
  midtones and highlights), then Filament's AgX in Rec. 2020 with the
  grading's `agx_look` (`None`, AgX's base contrast; `Punchy`, more contrast
  and saturation; `Golden`, a golden, slightly washed-out tint), then the
  grading's post-saturation. The default grading changes nothing. The output
  is dithered with Bevy's deband dither, up to half an 8-bit step per
  channel, so smooth gradients do not band on an 8-bit surface; the
  tone-mapped diagnostics capture is not. Timing group `tone map`.

## Motion blur

```rust,ignore
use sgl_3d::settings::{MotionBlur, Settings};
let settings = Settings { motion_blur: MotionBlur::Full, ..Settings::default() };
frame.motion_blur.shutter_angle = 0.5; // the default, film's 180° shutter
```

- Each pixel blurs over `shutter_angle` of its motion since the last frame,
  centred on it, times `Settings::motion_blur`: Full 1, Reduced
  0.5; Off (the default) does not run. The motion is the G-buffer's: the
  camera's, and moving and deforming instances' own. Like a shutter, the
  blur follows the frame rate: a faster frame moves less and blurs less.
  A frame that restarts history (a camera cut, a target-changing resize or
  another scene) is not blurred. It needs `perspective`'s projection.
- It is Wicked Engine's tile-max reconstruction filter (Jimenez 2014, after
  McGuire et al. 2012). The longest blur in each 32-pixel tile and in its
  3×3 tile neighbourhood steers 16 samples per pixel along it, and depth
  and speed weight them: a moving silhouette blurs over what is behind it,
  and a slower surface in front, such as a craft the camera follows, stays
  sharp. A tile whose neighbourhood blurs less than a pixel is copied, and
  one of nearly even blur takes a plain average along each pixel's motion.
  A blur is at most two tiles (64 pixels) long, as McGuire clamps it, so
  the neighbourhood carries it.
- It runs on the antialiased frame at the scene size, after TAA or FSR2 and
  before bloom, SMAA and tone mapping, as Bevy and Wicked Engine place it.
  Blended surfaces write no motion, so they blur as what is behind them
  moves.
- Timing group `motion blur`.

## Volumetric fog

```rust,ignore
use sgl_3d::Fog;
frame.atmosphere = true; // fog and mist are off by default
frame.fog = Fog {
    density: 0.002,          // extinction per metre at and below `height`
    albedo: [0.9, 0.95, 1.], // the share of extinction that scatters
    anisotropy: 0.3,         // Henyey-Greenstein g: forward scattering
    ambient: 1.,             // the share of the ambient light it scatters (default 0)
    height: 0.,
    height_falloff: 0.05,    // density halves every 20 m above `height`
    length: 400.,            // metres of view depth the volume covers
    sky_affect: 0.5,         // half the fog on the sky (default 1)
};
// Denser medium in a box, such as a tunnel's haze, added to the frame's.
scene.update_fog_volumes(device, queue, &[FogVolume {
    center, rotation, size,  // metres, about its centre
    density: 0.01,
    albedo: [0.9, 0.9, 1.],
    ..Default::default() // Godot's edge fade, 0.1
}])?;
```

The fog is Godot's volumetric fog (b130438 `volumetric_fog_process.glsl` and
`fog.cpp`, MIT), after Hillaire's "Physically Based and Unified Volumetric
Rendering in Frostbite" (2015). A volume of froxels fills the camera's
frustum out to `length` metres of view depth, more of its slices near the
camera (Godot's default detail spread, 2). Each frame, after shadows and
before opaque, the fog stage:

- sums each froxel's medium: the frame's, whose density halves every
  `1 / height_falloff` metres above `height`, and that of every fog volume
  (`Scene::update_fog_volumes`, Godot's box `FogVolume` with its fog
  material's density, albedo and edge fade) it lies in, of the first 1,024
  in the scene's order that reach the frame;
- lights it with the directional lights through the one shadow cascade at
  the froxel's depth, whose light fades with the metres the froxel lies
  behind its occluder (Godot's fog; an occluder beyond the cascade's 20 m
  pancake counts from the pancake's edge), the camera's clustered
  point, spot and rectangle lights (baked ones too) through the local-light
  atlas, each shadow one tap that the reprojection resolves (a local light's
  one hardware 2×2 tap, as Bevy's volumetric fog samples them), and
  `ambient` (0 by default, as Godot's `volumetric_fog_ambient_inject`) of
  the hemisphere fill and environment diffuse, scattered toward the camera by
  Henyey–Greenstein's phase function of `anisotropy`; each
  light's `fog_energy` scales its share, and a light at or below 0.001 is
  skipped, attenuation and shadow lookup, as Godot does;
- blends each froxel with where it lay in the last frame's volume, keeping
  0.9 of it (Godot's default reprojection amount), and samples another
  point of it each frame (Godot's 16 Halton offsets), so shafts and shadow
  edges in the fog resolve over frames; this history restarts with the
  camera's;
- with `Settings::fog_filter` (on by default, as Godot's `use_filter`),
  blurs each slice with Godot's 7-tap Gaussian across x and then y, which
  smooths what one sample per froxel leaves; the next frame reprojects the
  unfiltered volume, as Godot keeps its history before it filters;
- integrates each column front to back along its view ray into the light
  scattered toward the camera and the transmittance to each slice.

Every draw then fogs from that volume where its point lies, as colour ×
transmittance + scattered light: source completion fogs opaque surfaces and
the sky (as if at `length`, by `sky_affect`: the sky mixed with its fogged
self, as Godot's `volumetric_fog_sky_affect`, 1 by default), and blended
surfaces, glow and mist fog themselves. Reflections composed over a surface
take its transmittance, and screen-space reflections trace the fogged frame.
Probe captures have no fog.
The `offscreen` example's `--fog` shows the frame's medium and a fog volume.

Light shafts are the medium's shadowed scattering: where an opening lets a
light's shadow map through, the medium lights, and forward scattering
(`anisotropy` above 0) brightens it toward the light. A frame
without a medium (no density and no fog volumes) runs no fog and pays
nothing for it.
`Settings::fog_quality` picks the volume's resolution: 64 slices, and Low
is Godot's default of 64 froxels across the frame's mean side, High 128. Timing groups `fog injection` (the lights and shadows, which scale
with froxels and the lights reaching them), `fog filter` (with the filter)
and `fog integration`.

Each frame bounds the froxels every fog volume may reach from its corners,
as Godot does: a froxel evaluates only the volumes whose bounds hold it,
after testing those of every volume in view. A volume behind the camera,
beyond `length`, or wholly in front of the camera and beside the frame costs
nothing on the GPU. A volume that holds or crosses the camera's plane is
evaluated in every froxel of the frame up to its far end, so split a long
one, such as a tunnel's, into segments. A moving light's scattering trails
it by the history it keeps. Froxels are coarse: detail in the fog blurs
along the view, the more the farther.

Where it differs from Godot's fog, and why:

- The frame's medium takes a fog material's height falloff
  (`height_falloff`), so height fog needs no volume.
- The detail spread and the share of history kept are SGL3D's, at Godot's
  defaults, where Godot's `Environment` exposes them (S3D-6), and
  reprojection is always on.
- Every medium scatters `albedo` × density unquantized. Godot quantizes its
  fog volumes: it drops a volume at or below density 0.001, steps its
  density by 1/1024, caps its scattering at density 1 and truncates that
  scattering to 11/11/10 bits, so a dense volume here scatters more light
  than Godot's.
- `albedo`, the frame's and a volume's, is linear RGB, where Godot's fog
  colours are sRGB.
- A fog volume is a box with a density, albedo and edge fade; its box's last
  0.1 m fades under `edge_fade` too, so a volume with none still ends at its
  box. Emission (the frame's medium's too), a volume's height falloff,
  density textures, negative density and other shapes are not ported, and
  there is no GI injection, which in Godot needs VoxelGI or SDFGI.
- The lights are SGL3D's, in its units and falloff.
  The directional shadow has no fade toward the shadow distance: beyond it
  the fog is unshadowed, as surfaces are. A local light's light in the fog
  ends at its occluder instead of fading over about 10 cm behind it. A
  rectangle scatters by its face's solid angle, which stays bounded near
  it, so it takes no distance clamp against flicker as Godot's area lights
  do.
- `ambient` scatters the mean of the hemisphere fill and environment
  diffuse over the sphere, which an isotropic medium scatters, where Godot
  samples its sky upward and along the view.
- A froxel without history (the first fog frame, a new volume size or the
  camera's reset) keeps none, where Godot's fog fades in from a cleared
  volume and blends across cuts.
- The integration steps along each view ray, where Godot's steps view depth
  and so thins its fog toward the frame's edges.
- Fog needs `perspective`'s projection, whose froxels it places; with
  another camera nothing fogs, where Godot's also fogs an orthographic view.

## Point, spot and rectangle lights

Point, spot and rectangle lights are scene content. `Scene::add_light` returns a
`LightId`; `set_light` replaces the light's description (a moving light is set
every frame) and `remove_light` ends it. Directional lights and the
hemisphere fill are [frame input](#frame-lights-and-look).

```rust,ignore
use sgl_3d::{Light, LightShape};
let lamp = scene.add_light(&device, &queue, Light {
    position: Vec3::new(0., 6., -20.),
    shape: LightShape::Spot { direction: Vec3::NEG_Y, inner_angle: 0.3, outer_angle: 0.9 },
    color: [1., 0.9, 0.8], // linear RGB
    intensity: 40.,        // candela
    range: 25.,            // metres
    baked: true,           // the game's bake holds this fixture
    specular: 0.,          // reflections already show its emitter
    ..Default::default()   // no shadow, fog_energy 1
})?;
```

`Light::default()` is Godot's `Light3D`: a white point light at the origin
of π candela (its light energy of 1, which it scales by π) reaching 5 m,
live, physical specular, fog energy 1 and no shadow (opacity 1 when cast).
Set what differs and take the rest with `..Default::default()`.

- Light falls off with the inverse square of distance and fades smoothly to
  nothing at `range`, Filament's punctual lights as Bevy shades them. A spot
  fades from `inner_angle` to `outer_angle` around its direction; the outer
  angle stays below a right angle. Choose the range where the light stops
  mattering: each pixel pays for every light whose range reaches it.
- `LightShape::Rect { direction, width_axis, width, height }` is a one-sided
  rectangle centred on `position`, facing `direction`, `width` metres along
  `width_axis` (its part across `direction`) and `height` across both: a
  panel or strip fixture as Bevy's `RectLight`. It lights the half-space in
  front of its face as a Lambertian emitter of even luminance, so nearby
  surfaces take soft light and stretched highlights; far away it acts as a
  spot light of the same `intensity` (candela along its normal, its
  luminance times its area; Bevy takes π times this in lumens). Its face is
  integrated by linearly transformed cosines (Heitz et al. 2016), the GGX fit
  of selfshadow/ltc_code, and its light fades to nothing at `range` from its
  centre. A rectangle costs more per pixel than a spot: it integrates its
  face once for diffuse light and once more for each specular lobe (base and
  coat) unless `specular` is 0. The lit pipelines shade rectangles only
  while the scene holds one, so a scene without them pays nothing for them;
  the first rectangle added, or the last removed, compiles the other set.
- `baked` is Godot's `BAKE_STATIC`. A baked light lights only receivers
  without baked lighting: moving instances, and static receivers with no
  baked map (baked lighting is off, or the material is not lightmapped and
  no irradiance atlas is installed or the lightmap UV is unassigned). A
  chart's black outside its cropped bounds is part of its bake. Static
  receivers with a baked map take the light from the game's lightmap or
  irradiance atlas, and its moving casters do not shadow them. A game that makes a baked fixture a baked light
  leaves that fixture out of its moving instances' ambient cubes, so its
  light counts once. A live light lights every receiver and is left out of
  the game's bake.
- `specular` scales the light's specular lobes, base and coat (Godot's
  `light_specular`). Use 0 for a fixture whose emitter reflections and
  probes already show, so its highlight does not count twice.
- `fog_energy` scales the light it scatters in the
  [volumetric fog](#volumetric-fog) (Godot's
  `light_volumetric_fog_energy`): 1 is physical and 2 doubles it. At most
  0.001 leaves the light out of the fog, which then skips its attenuation
  and shadow lookup, as Godot does: set 0 on fixtures whose light the fog
  does not need, to save fog injection time. The fog still visits the
  light's cluster entry, and its shadow is still drawn for surfaces, which
  take the light alike at any value. A baked light lights the fog too,
  which has no bake, and its `fog_energy` scales that.
- `casts_shadow` gives the light a shadow in the local-light shadow atlas
  ([Local-light shadows](#local-light-shadows)).
- `shadow_opacity` is how dark that shadow is, 0 to 1 (Godot's
  `shadow_opacity`): its visibility is blended toward unshadowed,
  mix(1, shadow, opacity), on surfaces and in the volumetric fog alike, so
  0.8 lets a fifth of the light through behind its casters, a cheap stand-in
  for bounced light. 1, the default, is the whole shadow; at most 0.001
  draws none and skips the shadow lookup, as Godot does, though the light
  still takes its room in the atlas. The directional lights take it too.
- Each frame the renderer assigns the lights, with the
  [decals](#decals), to clusters of the camera's view on the CPU, as Bevy's
  clustered forward rendering does: a grid of render-target tiles and depth
  slices spaced logarithmically from 5 m to the farthest light's or decal's
  reach. A pixel loops over its cluster's lights, live ones
  first; a receiver with baked lighting stops there. Probe captures shade
  every light that is on; world-space ray hits shade the lights that reach
  the camera's view, as Wicked Engine's ray-traced reflections do. A
  cluster lists at most 8,192 lights and decals (the top of Godot's
  `max_clustered_elements` range), live lights first, then baked lights,
  then decals, in the scene's order. A probe capture's list is the whole
  scene's, a world-space ray hit's that of the whole view and a dynamic GI
  probe ray's that of the whole volume, so for them this is a limit on the
  scene.
- Values must be finite, colour, intensity, specular and fog energy
  nonnegative, the range positive, a spot's direction nonzero, and a
  rectangle's direction nonzero, its width axis not parallel to it and its
  size positive; anything else is refused with `SceneError::InvalidLight`. A light with zero intensity costs nothing.

## Decals

Decals project images through a box onto the lit surfaces inside it, as
Godot's clustered decals do: road markings, signage, grime and damage placed
by the game instead of authored into every texture. They are scene content.
`Scene::add_decal_image` keeps an image (`asset::Image`; a compressed one's
level 0 is decoded) and returns a `DecalImageId`; `Scene::add_decal` returns a
`DecalId`, `set_decal` replaces a decal's description (a moving decal is set
when it moves) and `remove_decal` ends it. An image a decal uses cannot be
removed (`SceneError::DecalImageInUse`).

```rust,ignore
use sgl_3d::{asset::Image, Decal};
let paint = scene.add_decal_image(Image::Rgba8(arrow))?;
let matte = scene.add_decal_image(Image::Rgba8(rough_paint))?;
let marking = scene.add_decal(&device, &queue, Decal {
    position: Vec3::new(0., 0., -40.), // unturned, it projects down its −Y onto the road
    size: Vec3::new(1.2, 0.4, 4.),
    metallic_roughness: Some(matte),
    normal_fade: 0.5,
    ..Decal::new(paint) // Godot's decal defaults
})?;
```

`Decal::new(image)` takes Godot's `Decal` defaults for the rest: a 2 m cube
at the origin, unturned, no normal or metallic-roughness map, a white
`color`, `base_color_mix` 1, fades of 0.3 toward the upper and lower faces
and no normal fade.

- The box spans `size` about `position`, turned by `rotation`. It projects
  down its local −Y: its images lie across its local X (U, left to right) and
  Z (V, top to bottom), and every point between its lower and upper faces
  takes the texel above or below it. Keep the box shallow so it reaches only
  the surface it marks: anything inside it, a passing vehicle included, takes
  the decal.
- The base colour image is sampled as sRGB, times `color` (linear RGB and
  alpha). Its alpha is the decal's coverage: it blends `base_color_mix` of
  the base colour, and weights the normal and metallic-roughness maps, as
  Godot does; a decal with `base_color_mix` 0 changes only what its maps
  change.
- `normal` is a tangent-space normal map in glTF's convention (+Y toward the
  image's top); the covered surface's normal turns toward it about the box's
  +Y. `metallic_roughness` holds roughness in green and metallic in blue, as
  glTF's does, and replaces the surface's.
- `upper_fade` and `lower_fade` fade coverage toward the box's faces
  (`(1 − d)` to their power, `d` the fraction of the way from the centre
  plane; 0 does not fade). `normal_fade` fades it on surfaces turned from
  the box's +Y and removes it where `(1 + n·Y) / 2` is at or below its value
  (0 does not fade).
- Decals change the surface before it is lit, so lighting, the G-buffer and
  every reflection see them: the camera's opaque and blended surfaces, probe
  captures (every decal) and world-space ray hits (the decals that reach
  the camera's view; they sample the atlas's first level, as hits sample
  material textures). Unlit materials take no decals. Overlapping decals
  apply in an order SGL3D chooses.
- The camera's clusters list decals after the lights, each as the sphere
  about its box, as Bevy clusters its decals, so a pixel pays only for the
  decals whose bounds reach its cluster. A cluster lists at most 8,192
  lights and decals together, decals last
  ([lights](#point-spot-and-rectangle-lights)); a probe capture's list is
  the scene's, a ray hit's the view's and a dynamic GI probe ray's the
  volume's.
- The lit pipelines apply decals only while the scene holds one, so a scene
  without them pays nothing for them; the first decal added, or the last
  removed, compiles the other set. Neither Godot (b130438, whose clustered
  pass walks each cluster's decals whatever the scene holds) nor Bevy
  (9d12036, whose `CLUSTERED_DECALS_ARE_USABLE` follows the device)
  specialises on decals; a game that adds its first decal mid-play and
  cannot afford that compile adds its decals at load.
- The scene packs every image its decals use into one atlas of linear
  16-bit texels with five mips, as Godot packs its decal atlas: each image
  once for each colour space its maps sample it in, with a border that keeps
  images apart at every mip. A decal that brings in an image, or a colour
  space, the atlas lacks places a new layout; the next frame (or probe
  capture) packs the atlas anew and uploads every image once, however many
  decals brought images in, as Godot packs its atlas once per frame. Add
  images and decals at load. A removed decal's images stay in the atlas
  until it next packs. A decal whose layout would be larger than the
  device's textures is refused when it is added, with
  `SceneError::DeviceLimit`.
- Values must be finite, the size positive, the rotation nonzero, `color`
  and `base_color_mix` in [0, 1], the fades nonnegative and `normal_fade`
  below 1; anything else is refused with `SceneError::InvalidDecal`.

## Local-light shadows

Lights with `casts_shadow` share one 4096² depth atlas (2048² at the Low
`Settings::shadow_quality`, Godot's desktop and mobile `atlas_size`), split
as Godot's shadow atlas is into four quadrants, of 16 slots of 512 texels, 64
of 256, 256 of 128 and 1024 of 64 (half those sizes at Low). Each frame the casting lights whose range reaches the camera's view
are ranked by screen coverage and given slots of the size their coverage
wants, largest first. A light keeps its slots while it is seen, and moves to
another size only after holding them for half a second at 60 frames per
second; a light not seen gives up its slots, least recently seen first, to
lights that need them. Lights the atlas has no room for are lit without a
shadow. `Renderer::local_shadow_stats` reports how many lights in the view
have a shadow, how many do not, and what the frame drew. With ray-traced
shadows the camera's opaque surfaces take up to fifteen of the placed
lights' shadows from rays instead
([Hardware ray tracing](#hardware-ray-tracing)).

A point light has six cube faces around it, side by side in one quadrant as
Wicked Engine lays them out; a spot light has one face covering its cone, or
the cube faces its cone reaches when it is wider than 45°; a rectangle has a
point shadow from its centre, as Godot shadows its area lights, in the cube
faces that see the half-space in front of it (five for a face along an
axis). Faces reach the light's range.

Each face keeps a static layer in a second atlas: its static instances,
drawn once. A frame draws only what changed. When a moving instance enters,
leaves or moves within a face, the frame copies the face's static layer and
draws the moving instances over it. The layer itself is drawn again when the
face is new to its slot, a static edit reaches it (adding, removing or
changing a static instance, or replacing a model one shows; each edit's own
bounds are tested, so streaming many chunks in one frame redraws only the
faces they reach, until more than 1024 edits in a frame merge in pairs),
the visibility mask changes, or a material's side, visibility group or
alpha mode changes
(or a masked material's cutoff or base alpha). A light that
moved since the last frame, such as one following a craft, has no reusable
layer and draws every caster. A frame in which nothing moved in any shadowed
light's range draws no shadows at all. What a frame draws becomes reusable
only once it is submitted and `finish_frame` is called; a dropped or
unfinished frame is drawn again.

Shadows are filtered and biased as the directional cascades are, with
Bevy's filters (Castaño's 9-tap kernel, or the Jimenez spiral while TAA or
FSR2 resolves it, whether or not a directional light casts; one hardware
2×2 tap at Low) and Bevy's spot-light receiver offset for every face,
cube faces included: Bevy tunes its spot biases for these 2D filters and its
point biases for its cube-map filter, which the atlas does not use. Each
tap stays inside its face, as Wicked Engine clamps to its atlas
rectangles. Probe
captures and world-space ray hits, which show static content, sample the
static layers: a capture places the lights it shades and draws their layers
itself, and ray hits see the camera's lights, except one that moved this
frame, which is unshadowed for them.

A baked light's shadow darkens only what it lights: moving instances and
static receivers without baked lighting. Its moving casters therefore shadow
other moving instances, and its static casters shadow moving instances that
pass behind them.

## Reflections

Source completion adds each lit receiver's environment specular to the forward
beauty: the authored baked probes whose influence contains it, then the sky.

`Settings::screen_space_reflections` (`settings::ScreenSpaceReflections`: Off,
Half or Full resolution tracing; default Off) adds screen-space reflections,
traced by `Settings::reflection_method` (`settings::ReflectionMethod`). Each method
combines passes from several engines, listed with their licences under it.

- `Crystal` (default): one ray per pixel along its lobe's peak, the mirror
  direction, through a hierarchical depth buffer, then a spatial, temporal and
  bilateral denoiser, below perceptual roughness 0.2. Reflections stay sharp.
  SGL3D sets its tracing and denoising parameters, the same at Half and Full
  resolution, and supplies the roughness input (`src/view/post_fx.rs`):
  DiligentFX's `ScreenSpaceReflectionAttribs` defaults, with a perceptual
  `RoughnessThreshold` of 0.2 as in AMD's SSSR sample, Hydrogent's 64
  traversal steps (`MaxTraversalIntersections`), the lobe peak
  (`GGXImportanceSampleBias` 1) and 0.95 of temporal history
  (`TemporalRadianceStabilityFactor`): one stochastic ray per pixel leaves
  blotches on glossy receivers that the denoiser holds.
  - [DiligentFX](https://github.com/DiligentGraphics/DiligentFX/tree/f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b/PostProcess/ScreenSpaceReflection)
    SSR: AMD FidelityFX SSSR's tracing with a confidence output and
    DiligentFX's denoiser. Apache-2.0, the basis of SGL's
    [`sgl-post-fx`](../sgl-post-fx/README.md) (`LICENSE.txt`), with fixes and
    enhancements described in its [PROVENANCE.md](../sgl-post-fx/PROVENANCE.md).
  - [Godot](https://github.com/godotengine/godot/blob/d851ae838de54a0bd3dde9043912f39c74265cdd/servers/rendering/renderer_rd/shaders/effects/screen_space_reflection.glsl)'s
    mirror direction and depth tolerance (DFX-18, DFX-20). MIT,
    `sgl-post-fx/LICENSE-godot.txt`.
  - [Wicked Engine](https://github.com/turanszkij/WickedEngine/tree/2ff1d9e7b36091d6edf9f823af77e6bc9af20e3b/WickedEngine/shaders)'s
    tone-mapped spatial resolve (DFX-16), history weight and 2-deviation
    history clamp (`ssr_resolveCS.hlsl`, `ssr_temporalCS.hlsl`, DFX-25). MIT,
    `src/LICENSE-wicked.txt`.
  - [AMD's reflection denoiser](https://github.com/GPUOpen-Effects/FidelityFX-Denoiser/blob/d7dfecbabe7b9523b14e7b067216e06b86e8d189/ffx-reflection-dnsr/ffx_denoiser_reflections_reproject.h)'s
    virtual-point reprojection and its discard of a surface history far from
    the current neighbourhood, so fast motion does not smear floor reflections
    (DFX-25). MIT, `sgl-post-fx/LICENSE-amd-fidelityfx-denoiser.txt`.
  - [Bevy](https://github.com/bevyengine/bevy/tree/b56fc29d3016e641754765244b5ba3f9cc504671/crates/bevy_pbr/src/ssr)'s
    roughness fade. MIT OR Apache-2.0.
- `Velvet`: one mirror ray per pixel through a hierarchical depth
  buffer, accumulated over frames, then blurred through a Gaussian mip chain:
  the resolve reads each pixel's mip from a cone that widens with its
  roughness and ray length. Nothing is stochastic. It traces below perceptual
  roughness 0.7, fading over the last 0.1, with Godot's `Environment` defaults
  (64 steps, fade in 0.15, fade out 2, depth tolerance 0.5 m). SGL3D converts
  its G-buffer to Godot's view-space normal-roughness and depth, and the rays
  read this frame's radiance. As in Godot, the trace tone maps radiance by
  luminance before RGBA16F storage and roughness filtering and the resolve
  inverts it, so a few very bright hits cannot dominate a blurred reflection.
  Reflected luminance saturates near 2048. TAA's jitter moves the trace's
  inputs every frame, so a hit on thin bright geometry lands on different
  pixels and the Gaussian mips spread each into a blob larger than TAA's
  neighbourhood; Godot itself flickers this way. The temporal pass
  (reprojection by motion and by reflection hit, depth disocclusion, a 3x3
  colour clamp) therefore accumulates the traced hits before the mip chain,
  and restarts with the other histories. It costs about 0.9 ms at 1920x1080
  on an Apple M5.
  - [Godot 4.7.2](https://github.com/godotengine/godot/tree/ed1daf0bf001b61586d9930840f2f1394092c079/servers/rendering/renderer_rd/shaders/effects)'s
    SSR (`screen_space_reflection*.glsl`, as `ss_effects.cpp` runs them): the
    trace, mip chain and resolve, in `src/stages/reflections/velvet/`. MIT,
    `src/LICENSE-godot.txt`.
  - [Wicked Engine](https://github.com/turanszkij/WickedEngine/blob/2ff1d9e7b36091d6edf9f823af77e6bc9af20e3b/WickedEngine/shaders/ssr_temporalCS.hlsl)'s
    SSR temporal pass, in `src/stages/reflections/temporal_reprojection.wgsl`
    (shared with world-space rays) and `velvet/godot_reflections_temporal.wgsl`.
    MIT, `src/LICENSE-wicked.txt`.

Each method returns the same premultiplied radiance and confidence, so a new
one (hardware ray tracing) plugs in beside them.

The methods trace the surface: the nearest of the opaque surfaces and the
[blended receivers](#blended-receivers). Composition adds the opaque lobes;
under a receiver, whose result it is, the opaque lobe keeps its environment
and probe specular, and the receiver composes the result in its blended draw
by the same formula.

`Renderer::new` builds source completion for its settings' screen-space
reflections and ambient occlusion, so a first frame from a `perspective`
camera compiles none of it; turning either on or off later compiles it again
on the next frame.

SSR traces the lit beauty with effects and mist, emitters included. Probe
captures include emitters too, so hits and misses show the same fixtures. A
lobe is traced when its perceptual roughness is
below the threshold: the coat of a coated receiver, else the base. Source
completion leaves out that lobe's environment specular. SSR returns radiance
premultiplied by its confidence (`sgl-post-fx` DFX-17). Composition adds
`response × (reflected × fade + environment × (1 − confidence × fade))`,
fogged as completion fogs:
- `response` is the lobe's split-sum term.
- `environment` is the receiver's own per-pixel probe or sky specular.
- `fade` falls smoothly from 1 to 0 below the method's threshold: over the
  last 0.05 of perceptual roughness for `Crystal`, as Bevy's SSR fades out,
  and over the last 0.1 for `Velvet`, as Godot's forward pass does.

Bevy composites SSR this way, and AMD's SSSR likewise blends the environment
into each missed ray. Misses and low-confidence hits fall back to the same
specular the receiver gets with SSR off, whatever share of a pixel's rays hit.
The fade leaves no seam where a roughness crosses the threshold. Each lobe
counts once. Rougher receivers keep their own environment specular. The G-buffer roughness
includes geometric specular antialiasing (Filament's `normalFiltering`), as in
Filament, HDRP and Unreal.

History restarts on a camera cut, a resize that changes the targets and a
different scene. Switching SSR off releases it. DiligentFX's camera has a finite far
plane (10 km) where SGL3D's is infinite; surfaces beyond it are background to
SSR and TAA.

`Settings::world_space_reflections` (with SSR on) traces what SSR misses, as
Lumen and HDRP's mixed tracing continue failed screen traces in world space:
`Moving` reaches moving objects, which baked probes cannot hold, and `All`
everything, static surfaces included, as Wicked Engine's RT reflections trace
the whole scene. Each traced lobe of an opaque
surface that SSR did not fully resolve, and that no blended receiver covers,
casts one GGX-sampled ray at half resolution through the portable scene BVHs,
or the scene's acceleration structures with
[hardware ray tracing](#hardware-ray-tracing),
reaching 1000 m (Wicked Engine's default `Postprocess_RTReflection`
range). The BVHs are two-level, as hardware acceleration structures are: a BVH over
the static instances and one over the moving instances, rebuilt on the CPU
when static content changes and every traced frame respectively, then each
model's own BVH, so a ray's cost follows the instances it reaches rather than
how many the scene holds. Adding an instance reserves room for its kind's BVH
in the ray source, about 30 bytes an instance in doubling steps, so
`add_instance` can fail with `SceneError::DeviceLimit` when the ray source is
nearly full. All opaque geometry participates in closest-hit visibility. Under
`Moving` only a moving nearest hit supplies secondary radiance: a ray finds its
nearest moving hit, then tests static visibility to it, and a ray that meets no
moving object walks no static geometry. Under `All` a ray takes its nearest hit
of either kind, leaving the reflecting surface's own triangle, so an off-screen
static wall reflects as it stands rather than as its probe recorded it; every
ray then walks the static geometry, which is what `All` costs over `Moving` on
the portable BVHs, so it is meant for [hardware ray tracing](#hardware-ray-tracing).
On an Apple M5, over a glossy 1 km strip with posts, boxes and 60 moving boxes
at 960×540 with full-resolution SSR, the `world reflection rays` pass took
0.49 ms under `All` against 0.22 ms under `Moving` on the portable BVHs, and
0.11 ms against 0.09 ms in hardware.
Neither is on by default or in a preset. Wicked Engine's RT reflection
resolve, temporal and bilateral upsample passes denoise the rays. The result is
premultiplied radiance with the share of rays that hit in alpha, and it composites as
`ssr.rgb + (world.rgb + environment * (1 - world.a)) * (1 - ssr.a)`, so misses
keep the probe and sky specular. Under `Moving` a static nearest hit also keeps
that fallback, while blocking moving objects behind it. Switching world reflections or SSR off
releases world history. Camera cuts, missed effect frames, a different scene and
resize restart accumulation from current data.

## Temporal anti-aliasing

`Settings::antialiasing` (`settings::Antialiasing`) is TAA, SMAA, Off or FSR2;
`Preset` selects TAA on High and SMAA on Low. TAA is derived from DiligentFX in `sgl-post-fx`,
integrated as Diligent's Hydrogent renderer does:
- The frame renders with a 16-sample Halton jitter and a −0.5 texture mip
  bias. Motion vectors (the G-buffer's motion target) stay unjittered; the sky's
  follow the camera's rotation, as a background at infinity does. Motion is
  capped at two screens along its longer axis; a surface or sky direction that
  was behind the last frame's camera gets that length, so every temporal
  effect drops the history it reprojects by motion. The reflections' temporal
  passes likewise take no history by a reflection hit that was behind the
  last frame's camera, and Crystal's and TAA's depth tests none for a surface
  that was.
- SSR and TAA need `perspective`'s infinite reversed-Z projection; with any
  other camera SSR is off and SMAA replaces TAA.
- TAA resolves the complete linear HDR frame, including reflections, fog,
  effects, mist and heat shimmer, before bloom and tone mapping.
- It uses Catmull-Rom history sampling, depth disocclusion and a variance
  clip.

SSR and TAA share DiligentFX's post-effect context. History restarts with
SSR's, and a restarted frame renders without jitter. As Godot's TAA, it keeps
history for fast motion that is consistent between frames, rejects it
gradually where motion changes by more than 2.5 pixels per frame (none left
at about 96 pixels), and clips it towards the neighbourhood mean within a
variance box that narrows with speed, to none at 2% of the screen per frame:
fast motion resolves softened rather than aliased. Upstream rejects by speed
(`sgl-post-fx` PROVENANCE.md DFX-14).

### SMAA

SMAA 1x runs on the linear HDR scene before tone mapping
([`src/stages/post/smaa/README.md`](src/stages/post/smaa/README.md)) at
`Settings::smaa_quality` (`settings::SmaaQuality`), SMAA 2.8's presets:
Low, Medium (the default), High and Ultra. They search up to 4, 8, 16 and
32 steps of two pixels each way along an edge, at a colour contrast
threshold of 0.15, 0.1, 0.1 and 0.05; High and Ultra also detect diagonal
lines and corners.

### FSR2

FSR2 is AMD's FidelityFX Super Resolution 2 from the `sp-fidelity` port (SDK
1.1.4) on `sp-fidelity-wgpu`, integrated in
`src/stages/antialiasing/fsr2.rs` as AMD's
documentation and FSR sample describe; that file's header lists each input and
its source. It antialiases like TAA while upscaling:
- `Settings::fsr2_quality` (`settings::Fsr2Quality`) sets the render size: Native
  AA renders every scene pixel; Quality, Balanced, Performance and Ultra
  Performance render 1/1.5, 1/1.7, 1/2 and 1/3 of it per dimension.
  `Renderer::render_size()` is the size of every pass up to antialiasing;
  `Renderer::scene_size()` stays the `SceneResolution` size, which FSR2 outputs
  and bloom and tone mapping use.
- The frame renders with FSR2's jitter sequence and a texture mip bias of
  `log2(render / scene) - 1`. FSR2 reads the linear HDR frame, the unjittered
  motion vectors and depth, the frame's exposure, and
  `FrameInput::frame_time_ms`. It sharpens with RCAS while
  `Settings::fsr2_sharpening` is on (the default), at `fsr2_sharpness` in
  AMD's 0..=1 (0.8 by default, as AMD's sample).
  The camera's blended surfaces, additive effects and mist also write FSR2's
  reactive and transparency-and-composition masks, as AMD's FSR documentation
  and sample write them from translucent draws.
- Request `graphics_device::fsr2_features(&adapter)` when creating the device.
  `antialiasing` and `fsr2_quality` take effect at the next `Renderer::resize`,
  which creates the FSR2 context. Where the device lacks the features or the
  context cannot be created, TAA runs at the scene size:
  `Renderer::antialiasing_in_effect(&settings)` reports it and `fsr2_error()`
  says why. A frame whose FSR2 dispatch fails is scaled to the scene size
  without it, and TAA runs from the next `resize`, with `fsr2_error()`
  saying why.
  The saved choice is unchanged.
- FSR2 needs `perspective`'s infinite reversed-Z projection; with any other
  camera the render-size frame is scaled to the scene size without FSR2.

## Ambient occlusion

```rust,ignore
use sgl_3d::settings::AmbientOcclusionQuality;
settings.ambient_occlusion = AmbientOcclusionQuality::High; // Settings: a quality level
input.ambient_occlusion_radius = 0.5; // FrameInput: consumer-authored metres
```

The default is Off. Low/Medium/High/Ultra select XeGTAO's 4/8/18/54 samples at
full scene resolution with current-frame spatial denoising. The consumer persists
this choice and includes it in complete Low/High preset bundles. Enabled AO adds
no geometry pass: it adds XeGTAO's depth mip, visibility and denoise passes
after opaque shading, over its depth and normals, and reflection source
completion's read of the ambient diffuse. Measure that complete cost.

Sky diffuse and surviving hemisphere fill are attenuated. Opaque shading
records that ambient diffuse beside its unoccluded colour, and reflection
source completion takes the share the visibility hides out of the colour,
never below zero, before adding specular. Environment and
baked-probe specular (including SSR's fallback where its rays miss) takes
Filament's desktop specular occlusion: Lagarde's specular AO from the
visibility, with GTAO multi-bounce on the base lobe's F0, so bright metals keep
most of their reflection. Screen-space reflection hits, direct light, emission,
baked direct/bounced irradiance and environment multiscattering retain their
existing ownership. Probe captures and secondary rays do not consume
camera-space AO. There are no substitute contact shadows.

The radius is clamped to 0.01–10000 m (XeGTAO's expected minimum and its
settings' maximum); `Settings::ambient_occlusion`
alone turns AO off. AO works with centered reversed-Z perspective cameras.
Orthographic, off-axis and forward-depth cameras leave it ineffective while
preserving the saved preference. Single-layer screen geometry cannot establish
offscreen or thin-occluder visibility; [the pinned XeGTAO catalogue](src/stages/opaque/ambient_occlusion/README.md)
details approximations and numerical evidence.

With `diagnostics`,
`Renderer::diagnostic_target(DiagnosticTarget::AmbientOcclusion)` returns the
last frame's R32Uint visibility texture (0..255), or `None` when AO did not
run. It shares the dimensions and receiver correspondence of the `Depth` and
`Normal` targets; it does not perform readback.

## Baked specular probes

Receivers outside screen-space reflections, and rays SSR misses, take their
environment specular from caller-authored static probes: local image-based
lighting with parallax-corrected cubemaps (Lagarde and Zanuttini, SIGGRAPH
2012), as HDRP, Unreal, Godot, Wicked and Bevy layer it under SSR. The consumer
owns validity across world, lighting, environment and GI variants, and clears
the collection when it has no matching bake.

```rust,ignore
use sgl_3d::{BakedSpecularProbe, SpecularProbeBox, SpecularProbeRadiance};
// Each probe: capture `center`, a rigid `world_to_local` frame, an
// `influence` box with per-axis `blend` distances, an optional `proxy` box
// for parallax, and its prefiltered `radiance`. SGL uploads without capturing.
scene.set_baked_specular_probes(&device, &queue, &probes)?;
// Clear after a content or lighting change the game has no bake for.
scene.set_baked_specular_probes(&device, &queue, &[])?;
```

- `influence` and `blend`: where receivers use the probe. Its weight is one at
  `blend` inside each face on that local axis, falling linearly to zero at the
  face (Godot's blend distance); zero blend is a hard edge. Influences may and
  should overlap: a receiver's probes are normalised by their total weight, so
  neighbours whose blends meet partition it exactly, and where the total is
  below one, as at the collection's edge, the sky fills the rest.
- `proxy`: the geometry the capture sees, for box projection. Keep it separate
  from the influence and as large as the space: a corridor's proxy spans the
  whole corridor, and an opening's proxy extends well past it. `None` treats
  what the probe sees as distant, as for open outdoor spaces.
- `radiance`: a cube with perceptual roughness level/6 at mip `level`, seven
  levels from a power-of-two `face_size` of at least 64 (Filament and Bevy use
  the same roughness-to-mip mapping). Up to 64 probes of one face size and
  texel encoding, and six array layers each within the device's
  `max_texture_array_layers` (`graphics_device::limits` requests the
  adapter's). Captures return `SpecularProbeTexels::Rgba16Float`; ship
  `Bc6hUfloat` blocks, an eighth of the size on disk and in memory, compressed
  offline with a BC6H encoder such as Intel's ISPC Texture Compressor (as Unreal
  compresses its captures). Installing BC6H needs the device feature
  `wgpu::Features::TEXTURE_COMPRESSION_BC`.

For offline authoring, install the static instances' fixed-light atlases, then
`renderer.capture_specular_probe(&device, &queue, &mut scene, &input, &settings,
center, face_size)?` renders six faces of the static capture-visible instances
and the reflection sky of `input`'s environment with `input`'s lights, and
GGX-prefilters them with filtered importance sampling. It includes fixed
emission and the scene's lights (baked ones only on surfaces without baked
lighting), with their static casters' shadows from the local-light atlas,
and excludes moving instances, effects and atmospheric post; the camera is
unused. A capture runs outside a frame, after `finish_frame` and before the
next `render`: it shares the renderer's shadow face views, cascade layers and
local-light atlas with the frame. Like the frame, it draws
only materials whose visibility group `input.visibility_mask` selects, so a
caller can leave geometry out of a probe as a reflection probe's culling mask
does.
Captured surfaces take their environment specular from the installed
collection, as source completion does at runtime, so capturing every probe
again with the first pass installed bakes one more bounce (Unity's reflection
bounces). The sky it records is the one reflections fall back to
(`FrameInput::reflection_environment`), not the camera backdrop, so probes and sky agree
where they blend. It never writes files: the game owns serialization, format
version and explicit refresh.

At runtime source completion first culls the collection per 32x32-pixel tile
(`src/stages/reflections/probe_culling.wgsl`, a port of Wicked Engine's tiled
light culling with its 2.5D depth mask), so each receiver walks only the probes
whose influence can reach its tile's depth range. The forward pass binds no
probes. Captures and world-space ray hits, which have no tiles, walk the probes
listed in their cell of a world grid built when the collection is installed
(`src/scene/probe_grid.rs`, following Wicked Engine's surfel grid), so each
walks only the probes whose influence can reach that cell.

## Visible work and pass selection

The camera's opaque and masked surfaces and each directional shadow cascade
draw from lists the GPU builds every frame ([GPU draw
lists](src/view/culling/README.md)), with nothing to configure. After the
deform pass, a cull stage tests each instance's meshes (the draw candidates
the scene keeps up as it is edited) for the view's population (the camera's
`visible` instances, a cascade's `capture_visible` ones whose material casts,
both under the frame's visibility mask), chooses the camera's level of
detail, tests the view's frustum, then tests each of the chosen mesh's
sections, the 128-triangle leaves of its range hierarchy, and appends those
that pass to their set's draw. A set is what draws with one pipeline and one
material: a material, whether its instances' poses mirror and whether they
deform. Each set draws with one indirect draw, whatever its instances and
their mobility, so recording a view costs the same at a hundred instances as
at a hundred thousand. The CPU walks no instance for these views: it builds
the camera's blended list (culled per instance, sorted back to front), the
local-light shadow faces and probe captures. Hardware culling preserves
authored single/double-sided and mirrored materials. Each probe face uses
its own camera; shadow and scene-ray populations are separate.

A GPU-built list draws its sets in their order and each set's sections in
the order the cull appended them, so two coplanar surfaces of different
draws have no defined winner at equal depth: give a coplanar overlay a depth
offset or make it a decal. wgpu validates indirect draws with a compute pass
before each render pass that issues them, on native backends, unless the
game creates its instance without `InstanceFlags::VALIDATION_INDIRECT_CALL`
(set by default); the lists' few draws need no validation, so a game may
clear the flag, and keeping it costs that small pass.

`Settings::occlusion_culling` (off by default) adds two-phase occlusion
culling to the camera's list, as Bevy's GPU culling runs it: the early phase
also tests each instance and section at its last frame's pose against the
last submitted frame's depth pyramid (each texel the farthest depth it
covers), and sets aside what lies behind it; the opaque stage draws the
rest into the G-buffer; the late phase builds the pyramid from that depth
and tests what was set aside again, at this frame's pose; the G-buffer pass
draws what it passes, and the pyramid is built once more from the complete
depth for the next frame. Lighting then shades both sets once. Something
hidden last frame and visible now is drawn in the same frame, so nothing
appears late; a camera cut or a resize tests nothing early. The opaque stage
runs as two passes while it is on (the fused pass cannot be split), so it
pays only where a frame submits much hidden geometry. On an Apple M5 at
1920×1080 it cost 0.2–0.5 ms a frame natively on Metal on the `streaming`
and `irradiance_volume` examples' routes, though it culled up to 92% of the
camera's triangles, and saved 0.9–1.4 ms in Chrome over a headroom-sized
window seen from a walker's eye (#24). It needs six storage textures a shader stage, which `graphics_device::limits`
requests from the adapter; a device with fewer culls by frustum alone, and
`Renderer::occlusion_culling_in_effect` reports it. The directional cascades
cull by frustum alone.

The CPU-built lists draw instanced: the instances that draw the same mesh
of a model with the same material, face culling and mobility, after their
own culling and level-of-detail choice, share one draw per index range, as
Bevy batches its phases. Shadow and capture draws merge wherever they are;
blended ones only where they follow one another back to front. In every
list each instance keeps its own pose, motion, ambient cube and source
identity: a draw reads every instance's record from the scene's object
buffer. The [instances example](examples/instances.rs) places many props of
three models and prints the camera's sections and the CPU time each frame
took to record:

```sh
cargo run --release -p sgl-3d --example instances -- target/instances.png --count 10000
```

Devices with the attachments for it write stable geometry and primary lighting
in one eight-target pass, with ambient occlusion on or off. Only devices without
them draw a geometry pass before lighting. Use `graphics_device::limits` when
requesting the caller-owned device.

## Scrolling normal layers

`asset::Material::normal_layers` and `SurfaceMaterial::normal_layers` draw a
material's normal map as two layers moving across its surface: water's
waves, with no geometry uploaded per frame. Each `NormalLayer` has a
`velocity` (the material's UV units per second along U and V), a `scale`
(repeats of the map per UV unit, positive) and a `strength` (how much of its
slopes the surface takes, with `normal_scale`). The layers' height fields
add, so their slopes add (Barré-Brisebois and Hill's partial derivative
blend), as Wicked Engine's water and Bevy's water example scroll one map
twice or more.

- **Time.** `FrameInput::elapsed_seconds`, the game's presentation clock in
  seconds as an `f64`, places the layers, so every view of a frame (the
  G-buffer, the receiver pass, blended surfaces, probe captures and
  world-space ray hits) sees one surface. SGL3D reduces it modulo an hour on
  the CPU and rounds each layer's speed to whole repeats of its map per hour
  (steps of 1/(3600 × `scale`) UV units per second, so at most 1/7200 of a
  repeat per second off, and a layer slower than that stands still), so the
  waves keep their precision however long a session runs and nothing jumps
  when the hour turns.
- **Requirements.** A normal map that repeats on both axes, finite
  velocities and strengths, positive finite scales, and at most 2^24
  repeats of the map per hour (about 4660 a second) along each axis;
  otherwise `add_materials`, `add_asset` and `set_material` refuse the
  material with `SceneError::InvalidNormalLayers`. `set_material` changes
  the layers like any value; the glTF loader leaves them `None`.
- **Cost.** Two normal map samples in place of one in each pass that
  evaluates the material. Moving layers are no scene edit: a static instance
  can hold the material, and its shadow layers stay valid.
- **History.** The layers change shading, not geometry: they write no
  motion. TAA and the reflection methods' accumulation reject what they
  change by their colour clamps, as for any change of shading. FSR2 takes a
  blended surface's changing shading, a receiver's included, from the
  reactive and transparency-and-composition masks blended surfaces write;
  an opaque material's layers write no mask.

The [water example](examples/water.rs) scrolls a procedural wave map.

## Alpha-masked and blended materials

`asset::Material::alpha` and `SurfaceMaterial::alpha` are an `AlphaMode`; the
glTF loader reads `alphaMode` and `alphaCutoff` (0.5 when absent). A masked
cutoff must be finite and nonnegative, as glTF bounds it; the scene refuses
others with `SceneError::InvalidAlphaCutoff`. The base
alpha is the material's base alpha times the vertex colour's and the base
map's.

- `AlphaMode::Opaque` ignores alpha.
- `AlphaMode::Mask { cutoff }` (foliage, fences, grilles) is opaque where the
  base alpha reaches `cutoff` and cut out below it, as Bevy's `alpha_discard`
  does: in every opaque pass, probe capture and shadow (the directional
  cascades and the local-light faces and their cached layers), and at ray
  hits, where the portable ray traversal's nearest, any-hit and visibility
  queries pass through cut-out texels. Masked materials draw with their own
  pipelines, so opaque ones keep early depth testing.
- `AlphaMode::Blend { receives_screen_space_reflections }` (glass, screens,
  holograms, water) is blended with its alpha over what lies behind it. The
  transparent stage draws blended surfaces back to front, sorted by the view
  depth of each mesh's bounds centre as Bevy's `Transparent3d` phase is, onto
  the composed frame and, while a screen-space reflection method traces it,
  into the reflection input, before additive effects and mist. They are lit
  as opaque surfaces are: the directional lights and their shadow, the
  clustered point, spot and rectangle lights, baked light, the environment,
  and the installed probes' and reflection sky's specular; then fogged.
  Unmarked (`false`, as glTF loads them), they write no depth, motion or
  G-buffer, so ambient occlusion, screen-space reflections and temporal
  antialiasing see what lies behind them. They cast no shadow and rays pass
  through them, as Godot leaves alpha-pass materials out of its shadow
  passes, and probe captures leave them out. Order is per mesh: split
  intersecting or interleaved blended meshes. Order-independent transparency
  is not provided.

`Scene::set_material` changes a material's alpha mode like its other values.
The `offscreen` example's `--alpha` shows both modes.

### Blended receivers

A blended material marked `receives_screen_space_reflections: true` is a
receiver: water or glass that reflects the scene in front of it. Where it is
the nearest receiver it is the surface that screen-space reflections trace
and temporal effects reproject, as HDRP's `Receive SSR Transparent` materials
are:

- After the opaque stage, a receiver pass draws the camera's receivers over a
  copy of the opaque depth, the nearest winning, with the frame's jitter: their
  depth, their traced lobe's normal and roughness (the coat on a coated
  receiver, else the base) and their motion.
- Crystal or Velvet traces them as it traces opaque surfaces, at the same
  resolution, cutoff and fade, through the same accumulation
  ([Reflections](#reflections)).
- The blended draw composes the result into the traced lobe in place of its
  probe and sky specular, by the formula opaque composition uses. The opaque
  surface seen through a receiver keeps its own probe and sky specular, and
  world-space rays skip it.
- TAA, FSR2 and motion blur reproject and blur a receiver's pixels by the
  receiver's depth and motion. The pass runs while the scene holds a receiver
  and SSR, TAA, FSR2 or motion blur runs, so with SSR off and TAA on, TAA
  still reprojects a receiver by its own motion rather than by what lies
  behind it.

Limits: the nearest receiver at a pixel alone reflects; a receiver behind
another, and one seen in another's reflection, show their probe and sky
specular. The reflection is scaled by the receiver's alpha with the rest of
its colour (glTF's coverage blend, as Godot and Bevy blend). What lies behind
is blended under it undistorted (no refraction), and reprojects and blurs by
the receiver's motion, so a receiver moving against its background trails the
background, not its reflection. World-space rays do not fill a receiver's
misses; probe and sky specular do. Unlit receivers, additive effects and mist
reflect nothing.

The game supplies the receiver as any surface: its mesh, the normals it
animates, and its material's roughness, F0 and coat. Animate water's waves
with its material's [scrolling normal layers](#scrolling-normal-layers):
they move with the frame's time and upload nothing, so the receiver can be a
static instance, and many water chunks cost nothing per frame beyond their
pixels. A mesh replaced every frame belongs on a moving instance, so that
the replacement is no static edit; each replacement prepares the model
again (`PreparedModel::new`, its BVH included) and `Scene::set_model`
places and copies it. A deforming model animated with
`set_instance_deformation` is not seen by rays. The renderer allocates the
surface's targets (a depth and an RGBA16F layer, 12 bytes per render pixel)
in the first frame whose scene holds a receiver and keeps them from then
on; a renderer that has never rendered a receiver pays nothing. The [water example](examples/water.rs) is a lake that
receives reflections, its waves from its material's normal layers; it prints
the GPU time of the passes receivers touch before and after its lake is
marked, and with its waves instead replaced every frame by `set_model`
(`set-model`), with what that uploads and the CPU time preparing and
replacing it take:

```sh
cargo run --release -p sgl-3d --example water
```

## Skinned meshes and morph targets

SGL3D renders the pose a game gives it; sampling and blending animation clips
stay in the game (S3D-1).

- **Loading.** `asset::load` and `load_slice` import a glTF's skins and morph
  targets as each `CpuMesh::deformation` (`deformation::MeshDeformation`): an
  `Influence` per vertex (four joints of the asset's joint list and their
  weights; a fifth influence set is refused) and `MorphTarget`s (a
  displacement of each vertex's position, normal and tangent, scaled by one
  morph weight). `Asset::rig` (`deformation::Rig`) holds the node hierarchy
  with each node's rest transform, the joints (each skin's, in one list: a
  node and its inverse bind matrix), the morph weights (each morphed node's,
  with its rest weights) and the animation clips (`Clip`: channels of
  keyframe times and translation, rotation, scale or morph-weight values,
  with glTF's step, linear or cubic-spline interpolation). A skinned mesh's
  vertices stay in bind space, as glTF ignores its node's transform; a rigid
  node's transform is baked into its vertices and morph displacements, as
  before. Primitives batch by material, skin and morphed node.
- **Procedural.** `ModelMesh::deformation` takes the same data; a model with
  any deforming mesh deforms. `PreparedModel::new` refuses influences or
  targets that do not match their vertices, negative or non-finite weights,
  weights summing to zero, indices above 65535 and more than 256 morph
  targets on a mesh, as Bevy refuses (`SceneError::InvalidDeformation`).
  Weights are normalized.
- **Posing.** A deforming model's instances are `Mobility::Moving`. Each
  frame, `Scene::set_instance_deformation(&queue, instance, &joints,
  &morph_weights)` gives one joint matrix per joint the model's influences
  name (glTF's: the joint's transform in the model's space times its inverse
  bind matrix; `Rig::joint_matrices` composes them from each node's local
  transform) and one weight per morph weight its targets name; further ones
  are ignored, fewer are refused (`SceneError::DeformationMismatch`). An
  instance keeps its deformation until it is set again and starts at its
  bind pose. The instance's pose places the deformed model in the world.
  Every call counts as a change, whatever its values: the frame deforms the
  instance again and redraws the local-light shadow faces it reaches, so
  skip it for an instance whose pose did not change.
- **Rigid nodes.** A rigid mesh's node is baked at its rest transform, so a
  clip that animates it moves nothing; split such parts out by selecting
  their nodes (`LoadOptions::nodes`) and pose them as instances.
- **Rendering.** Each frame the deform stage morphs and skins, in one compute
  pass, every instance whose deformation changed since the last submitted
  frame (Bevy's skinning and morph math, run once per frame as Wicked
  Engine's `skinningCS` runs it), into the instance's own vertices; the
  G-buffer, lighting, cascades, local-light faces and blended pass all draw
  them. Motion is measured from the deformed positions of the last
  submitted frame, under the same rules as a moving instance's pose. Culling
  uses each mesh's skinned bounds (Bevy's per-joint bounds under the joint
  matrices, grown by the weighted morph displacements). A deforming instance
  is never in a cached shadow layer, and redraws the local-light faces it
  reaches whenever its deformation changes.
- **Limits.** A deforming instance keeps its model (`set_instance` to another
  model, or from a rigid model to a deforming one, is refused with
  `SceneError::DeformingModel`; remove and add it), and a deforming model
  takes no part in levels of detail. Scene rays (world-space reflections,
  the dynamic GI volume) see deforming instances only with
  [hardware ray tracing](#hardware-ray-tracing), which traces each one's
  deformed positions; the portable BVHs leave them out, as Bevy's
  ray-traced scene leaves out meshes with joints. Screen-space reflections
  see them. Probe captures, which show static content, never contain them.

The `skinned` example generates a skinned, morphed glTF with a clip, and
samples, poses and renders it:

```sh
cargo run -p sgl-3d --example skinned -- target/skinned.png
```

## Retained scene and frame lifecycle

A game holds a `Scene` (content: geometry, materials, instances, lights,
decals, environments and bakes), a `Renderer` (targets, pipelines and histories) and
its `settings::Settings`. Each frame it describes the camera, the
authored look and per-frame state in a `FrameInput`.

1. Select an adapter and request the renderer's device requirements:
   `graphics_device::limits` gives its adapter-sized limits,
   `graphics_device::features` the optional features SGL3D uses where the
   adapter has them, `graphics_device::fsr2_features` the features FSR2
   needs, and `graphics_device::ray_tracing_features` hardware ray
   tracing's, which a game that opts in requests with wgpu's experimental
   token ([Hardware ray tracing](#hardware-ray-tracing)). The game owns
   surface acquisition, device failure, and window handling.
2. Load assets through `asset::load` (files) or `asset::load_slice`
   (embedded glTF/GLB bytes, as a browser fetches them), each with a
   `_with_options` form, or build an `asset::Asset` from
   `CpuMesh`, `Material` (`Material::default()` is glTF's default
   material), and images: decoded RGBA8, or BC7 mip chains
   ([Compressed material images](#compressed-material-images)). Apply game-specific adaptations
   explicitly; `LoadOptions` can bound emissive strength when the game requests
   that behavior, supply any of the glTF's images (`images`) so the loader
   never reads or decodes them, and select mesh nodes (`nodes`). Default
   loading preserves authored strength, decodes every image and loads every
   node.
3. `Scene::new(&device, &queue)` starts empty. Add content between frames;
   each addition returns its identity (`MaterialId`, `ModelId`, `InstanceId`,
   `LightId`, `DecalImageId`, `DecalId`, `EnvironmentId`), and every operation
   that can fail returns `SceneError`,
   changing nothing:
   - `add_asset` adds a loaded asset whole and returns its materials' and
     model's identities (`AssetIds`); its own indices do not outlive the call.
     `add_materials` (texture indices index the images passed with them) and
     `add_model` add procedural content. A model's meshes (`ModelMesh`es
     naming materials already added) reach `add_model` and `set_model`
     prepared: `PreparedModel::new(meshes)` validates them (a mesh holds at
     most 65,536 sections of 128 triangles, 8,388,608 triangles;
     `SceneError::TooManySections` past it), builds their culling
     hierarchies, shadow-caster clusters and ray-query BVH and packs their
     GPU data without a device or the scene, and the value is `Send`,
     so a game prepares on its own worker threads (SGL3D starts none) and the
     thread that edits the scene only places and copies it. `add_model` and
     `set_model` check what needs the scene (materials, an anisotropic
     material's tangents, a deforming model's static instances) and the
     device's limits; a failed operation places nothing, and the game
     prepares again for another try:

     ```rust
     // On a worker thread.
     let prepared = PreparedModel::new(vec![ModelMesh { vertices, indices, material, deformation: Default::default() }])?;
     // On the thread that edits the scene.
     let model = scene.add_model(&device, &queue, prepared)?;
     ```
   - `add_instance(&device, &queue, InstanceState { model, pose, visible,
     capture_visible }, mobility)` places a model; `InstanceState::new(model)`
     is at the origin and shown in every view. `Mobility::Static`
     instances are what bakes and probe captures contain; they write no motion
     and take baked diffuse light from lightmap charts and the irradiance
     atlas. `Mobility::Moving` instances are posed every frame; they take an
     ambient cube and write motion from their pose in the last submitted frame.
     `visible` is the main camera; `capture_visible` is every other view
     (shadows, rays and, for static instances, probe captures).
   - `add_light` adds a point, spot or rectangle light
     ([Point, spot and rectangle lights](#point-spot-and-rectangle-lights)).
   - `add_decal_image` keeps an image for decals and `add_decal` projects
     images through a box onto surfaces ([Decals](#decals)).
   - `add_environment` adds a panorama and its PMREM atlas;
     `FrameInput::environment` names the one that lights a frame. With none,
     or a removed one, the frame has a black environment.
   - `set_material`, `set_model`, `set_instance`, `set_light` and `set_decal`
     replace values, geometry, state and descriptions; `remove_…` ends an identity, and its index is reused under a
     new one. Content that other content uses (a material by a model, a model
     by an instance or as a level of detail, a decal image by a decal) cannot
     be removed, and a level of
     detail's model cannot be replaced; replacing a model clears the levels of
     detail registered on it.
   - `update_mist`, `update_fog_volumes`, `update_effects` and
     `update_heat_distortion` replace transient geometry whole.
   - `move_origin(&device, &queue, to)` moves the render origin to `to`, a
     finite position in the current render frame. Every position SGL3D
     takes is `f32` in that frame; a game whose world is larger than `f32`
     renders precisely keeps its own coordinates and moves the origin to
     stay near what it renders, chunk-aligned in a streamed world. Every
     position the scene holds becomes what it was less `to`: instances
     (with the pose their motion is measured from), lights, decals, fog
     volumes, mist, glow and heat geometry, installed probes, the dynamic
     GI volume (its probes kept) and the ray source. From then on the game gives the camera, the frame input and
     its edits in the new frame. It is not a static edit: moving instances
     keep their motion, static shadow layers stay valid, histories continue
     (each renderer translates what it keeps), and the directional
     cascades' texel grid stays put. Geometry, bakes and probe captures do
     not change. A translated position rounds once, at its magnitude in the
     new frame, so integer poses such as chunk origins stay exact and a game
     never re-poses its instances after a move. A game that never calls it
     pays nothing.

   Buffers grow as content is added and reuse removed content's ranges;
   content beyond a device limit is refused. Models share their buffers:
   the ray source, and slabs that hold every mesh's shadow-caster positions
   and indices from 1 MiB up to 512 MiB each, grown by half again when
   full, so adding, replacing and removing models creates no buffer per
   mesh. `Renderer::new(&device, &queue,
   output_format, output_size, device_scale, &settings)` creates the targets
   for an output of `output_size` physical pixels in a window of
   `device_scale` physical pixels per logical pixel.
4. Each frame, `set_instance` each moving instance with its pose and
   visibility, and `set_instance_deformation` each deforming one
   ([Skinned meshes and morph targets](#skinned-meshes-and-morph-targets)).
   A moving instance has no motion when it is new, its state
   names another model, or it was not `visible` in the last submitted frame;
   one that must not carry motion across a change is removed and added
   again. `set_light` each light that moves or changes. Fixed emitters light
   static surfaces through the [baked lighting](#baked-diffuse-lighting), and
   moving instances through it or as baked lights.
5. Call `renderer.resize(&device, output_size, device_scale, &settings)`. It
   does nothing unless the output size, the scale or a size-affecting setting
   changed, so call it every frame.
6. Build the frame's `FrameInput` (`FrameInput::new(camera)` holds the
   defaults) and call `renderer.render(&device, &queue, &mut encoder, &mut
   scene, &input, &settings, &output_view, timing)`. It encodes the whole
   frame into `output_view`, of the output format and size; the output may be
   offscreen, so no window or surface is required.
7. Draw game-owned HUD/UI afterward, submit the encoder, then call
   `renderer.finish_frame(&mut scene)`: the frame's camera and moving
   instances' poses and deformations become the next frame's history, and the shadow faces it
   drew become reusable. Do not finish a frame that was not submitted; an
   abandoned encoder leaves history unchanged and its shadow faces are drawn
   again.
   Scene edits upload through the queue, never the frame's encoder, so an
   abandoned frame loses none.

History restarts for `FrameInput::camera_cut`, a resize that changes the
targets and a different scene. Submit each encoded frame before rendering the
next one with the same `Scene` and `Renderer`: frame uniforms and upload
storage are retained, so frames with different inputs cannot be queued in one
submission. Run probe captures between frames, never between a frame's
`render` and its `finish_frame`. Skip rendering zero-sized or minimized
outputs in the game.
Recreate the scene and renderer after replacing the device. Dropping them
releases the library's GPU handles; the game still owns its device, queue,
surface, and asset sources.

The [offscreen example](examples/offscreen.rs) is the loop to copy, with
procedural geometry, caller-created environments, animation and PNG output:

```sh
cargo run -p sgl-3d --example offscreen -- target/sgl3d.png
```

The [streaming example](examples/streaming.rs) is a block world at a block
game's scale: 16 m chunks, each one model placed as a static instance at its
integer chunk origin, added under a per-frame budget as a camera walks or
flies, replaced by block edits and remeshing waves, and removed behind it;
water as moving blended receivers; torches; and the render origin moved
every chunk or every 256 m. A block edit remeshes the chunk holding the
block it changed and the chunks of that block's solid neighbours across a
border, since a solid block owns its faces toward air. Once the first window
has streamed in, it prints each scene operation's CPU time by size, a frame's
time in scene calls apart from the game's meshing and preparing (on four
worker threads, whose build steps it prints apart from the placing and
writing on the thread that edits the scene), its recording time,
uploads by call site, buffers created, the scene's buffer
sizes, draws per view, the local-light shadow faces each frame redraws and
GPU time per pass, and writes each run's last frame to
`target/streaming-example/`. `--check` moves the origin under a still camera
and fails if static content shows motion or a shadow is redrawn, then
remeshes chunks holding shadowed torches and fails unless their static
shadow layers are redrawn once:

```sh
cargo run --release -p sgl-3d --example streaming -- walk fly
cargo run --release -p sgl-3d --example streaming -- --check
```

### GPU pass timing

`timing::GpuTiming::new(&device, &queue)` returns `None` unless the device was
created with `Features::TIMESTAMP_QUERY`; pass `None` wherever a timing argument
is taken to render untimed. Each frame:

```rust
for frame in timing.begin_frame(&device, &queue) {
    // frame.frame, frame.total_ms, frame.passes: [PassTime { name, ms }]
}
renderer.render(&device, &queue, &mut encoder, &mut scene, &input, &settings, &view, Some(&timing));
// Game passes may join: `timestamp_writes: timing.render_pass("HUD")`.
queue.submit([encoder.finish()]);
timing.submitted(&queue);
```

`begin_frame` never blocks. It yields frames whose GPU work has completed,
usually two or three frames late; `latest()` keeps the most recent. A frame is
left untimed when all four query sets are still in flight. Only pass
`timestamp_writes` are used, so no inside-encoder timestamp feature is required.
Tile-based GPUs start a pass long before earlier fragments finish, so raw pass
spans overlap. Each pass is therefore charged from the later of its start and
the end of every earlier timed pass (including the previous frame's) to its
end; a group sums its passes, and groups sum to at most `total_ms`, which runs
from the frame's first charged start to its last timed end. Untimed work
between timed passes is charged to the next timed pass. SGL3D's groups, by
stage (each stage's documentation lists its own), are:

- prepare: `deform` (the frame's skinning and morphing, while an instance's
  deformation changed) and `cull` (the GPU-built lists' early phase);
- dynamic GI: `dynamic GI allocation`, `dynamic GI rays` and `dynamic GI
  blend`, while the scene holds a volume and `Settings::dynamic_gi` runs it;
- shadows: `directional shadow cascade 0` to `directional shadow cascade 3`
  (one per cascade the frame has), `local shadow layers` (static layers) and
  `local shadows` (the frame's faces);
- volumetric fog: `fog injection`, `fog filter` and `fog integration`;
- opaque: `sky` and `opaque geometry + lighting`, or `geometry`, `sky` and
  `opaque lighting` where the device lacks the fused pass's colour
  attachments or occlusion culling or ray-traced shadows run; while
  occlusion culling runs, `depth pyramid` (twice), `cull late` and
  `geometry late` between `geometry` and `sky`; then `ambient occlusion`;
- ray-traced shadows, after the opaque stage's G-buffer passes and their
  culling and before `sky` while they run: `ray-traced shadow rays`,
  `ray-traced shadow temporal` and `ray-traced shadow upsample`;
- reflections: `probe culling`, `reflection source completion`, Crystal's
  `SSR` passes (named after DiligentFX's debug groups), Velvet's `Godot SSR`
  passes, `world reflection rays`, `world reflection denoise` and
  `reflection composition`;
- transparent: `receivers` (the receiver pass, after opaque); `blended`
  (blended surfaces) and `transparent` (additive effects and mist), each
  drawn into the reflection input while screen-space reflections run and onto
  the composed frame; and `heat distortion`;
- exposure: `exposure`, while automatic;
- antialiasing: `TAA` or `FSR2` (all of FSR2's passes);
- motion blur: `motion blur`;
- post: `bloom`, `SMAA` and `tone map`;
- DiligentFX's post-effect context, for TAA and Crystal: `DiligentFX inputs`,
  `DiligentFX blue noise`, `DiligentFX reprojected depth` and
  `DiligentFX previous depth`.

Inactive work reports nothing.

## Baked diffuse lighting

Optional permanent surface lighting uses `static_lighting::Lightmap` and
`Scene::set_lightmap(device, queue, map, &material_ids)`. The map holds linear
irradiance divided by PI, before albedo, finite, nonnegative and within RGBA16F
range. The library uploads it as the uncompressed atlas below: an RGBA16F
texture, with an RGBA8Unorm one for its directionality, filtered bilinearly by
the hardware as Bevy, Godot and Wicked sample their lightmaps. Material UVs map
through the supplied scale/offset; X clamps and Y repeats (the sampler's
address modes), so a chart can wrap around a closed loop. A wrapping chart
cannot be split, so each side must fit the device's `max_texture_dimension_2d`:
`graphics_device::limits` requests the adapter's value rather than wgpu's
default of 8192, and a larger chart is refused with `SceneError::DeviceLimit`.
This single chart lights the listed materials where static instances use them;
the eligibility belongs to each material, so a material added later is not
lightmapped. The caller must regenerate it when the static scene, source
radiance, UV chart, or caster visibility changes.

This opt-in application extension supplies Lambertian diffuse with constant
`1-F0` surface transmission. On a coated material the coat's Fresnel toward
the view dims it, and the irradiance atlas's and a moving instance's
ambient cube's light, as it dims the material's live light and emission
(KHR_materials_clearcoat layers the coat over the whole base).

Static instances' other surfaces can additionally carry `Vertex::lightmap_uv`
and use `Scene::set_static_irradiance_atlas(device, queue, &IrradianceAtlas)`.
Supply equally sized `irradiance` and `back_irradiance` RGB arrays in linear E/PI;
the library uploads two RGBA16F layers and selects the actual material side.
Cropped charts supply the same normalized min/max `Vertex::lightmap_bounds`
on all three triangle vertices; extrapolated UVs outside these bounds return
black before sampling, so a giant triangle need not allocate a full dark chart.
Charts need filtering gutters. Give unassigned surfaces UV (0,0) or negative
UVs: they have no baked lighting, so baked scene lights light them. Lightmapped
materials take precedence. The atlas
must capture illumination inside large triangles; vertex-color lighting is not
a substitute. The game owns chart authoring, physical integration and rebaking.

Ship the atlas block-compressed, as Unity and Godot ship HDR lightmaps:
`Scene::set_compressed_static_irradiance_atlas(device, queue,
&CompressedIrradianceAtlas)` takes BC6H irradiance and BC7 directionality
(front then back layer, sizes multiples of 4), an eighth and a quarter of the
uncompressed GPU size. Its `scale` multiplies the decoded irradiance in the
shader, as Unreal's lightmap scale does, so a uniform source-power change needs
no re-encode. Compress offline with a BC6H/BC7 encoder such as Intel's ISPC
Texture Compressor; installing needs `wgpu::Features::TEXTURE_COMPRESSION_BC`.

To retain normal/bump detail under baked lamps, populate `Lightmap::directionality`
and the atlas's `directionality` / `back_directionality` with one `[f32;4]` per
texel holding a constant+linear irradiance lobe relative to the baked value: the
shader returns `irradiance * max(a + dot(w, n), 0)` for the mapped world normal
`n`, with packed `xyz = w / 8 + .5` and `w = a / 2`. Choose `a + dot(w, N) = 1`
at the bake normal `N` so flat surfaces keep the bake; `w` is the irradiance
gradient toward the light. A distant source along `N` is `w = N, a = 0`; uniform
hemisphere light is `w = N / 2, a = 1 / 2`. Use `[.5;4]` for a neutral lobe or an
empty vector for a nondirectional layer; a lobe that rounds to all-zero RGBA8
is reserved internally for absent layers and refused. Values must be finite and
within [0,1]. The hardware
bilinearly filters the lobe.
Lobes use RGBA8Unorm layers (a map without them allocates only one zero texel
per layer); quantize `w` first and derive `a` from the quantized value to keep
flat surfaces exact.

For moving instances, interpolate a game-authored precomputed field and call
`set_instance_baked_irradiance(queue, instance, AmbientCube { irradiance })`;
a static instance, which takes the charts above, refuses it with
`SceneError::StaticInstance`.
The six RGB E/PI entries are +X,-X,+Y,-Y,+Z,-Z in world axes.
The shader weights them with squared shading-normal components. This diffuse
approximation preserves world orientation but cannot represent sharp angular
variation, fine spatial shadow boundaries or fixed-source offscreen specular.
Measure its errors against physical samples in the game; ordinary IBL still
provides specular response. A state naming another model clears the cube, and
a new instance starts without one.
Neither alters material albedo or paints light into emission.

## Irradiance volume

A game that computes its own light field, such as a voxel world's
propagated sky and block light or a level's bake, lights its world from it
through the irradiance volume: a lattice of cells the game places and
writes by region, which every surface within it, static or moving, samples
by its position (a port of Bevy's irradiance volume, after Valve's ambient
cubes). Changing the light, a torch placed or a cave opened, is a region
write: no geometry replaced, no static edit, no texture installed again.

```rust
let volume = IrradianceVolume {
    origin: Vec3::new(-80., -64., -80.), // the first cell's least corner
    cell_size: Vec3::ONE,                // metres
    cells: [160, 128, 160],
};
scene.set_irradiance_volume(&device, &queue, Some(volume))?;
// On any thread: validate and pack a box of cells, x fastest, then y, then z.
let region = PreparedIrradianceRegion::new(corner, [48, 48, 48], &cells)?;
// Between frames:
scene.write_irradiance_cells(&queue, &region)?;
```

- **Cells.** `IrradianceCell { irradiance: AmbientCube, sky_visibility: [f32;
  6] }`, both in the order +X, −X, +Y, −Y, +Z, −Z, each face what a surface
  facing that way receives. `irradiance` is the cell's own light,
  irradiance / PI on the directional lights' scale: its emitters, block
  light and whatever bounce the game's field carries. `sky_visibility`, 0 to
  1, is how much of the frame's ambient (the environment's diffuse light and
  the hemisphere fill) reaches the cell from that side. A surface the volume
  lights takes `sky_visibility × ambient + irradiance` in place of the
  ambient, so the sky can change with the frame (day and night) without a
  cell being rewritten. A cell never written is `IrradianceCell::default()`:
  the frame's ambient whole and no light of its own.
- **Writes.** A region's corner is a cell's least corner on the volume's
  lattice and the region lies within it, or the write is refused with
  `SceneError::IrradianceRegionOutside` and writes nothing.
  `PreparedIrradianceRegion` is plain `Send` data: prepare relights on
  worker threads; the write only queues one texture write per face.
- **Sampling.** A surface steps half a cell along its geometry normal, so a
  voxel face reads the cell in front of it, then takes three trilinear taps
  blended by its shading normal's squared components. Blending with dark
  solid cells darkens convex edges, as smooth voxel lighting does; fill
  solid cells from their air neighbours where that is unwanted.
- **Which surfaces.** Lightmap and irradiance atlas charts keep their bake.
  Within the volume it covers the dynamic GI volume and ambient cubes, its
  share fading over the one cell past each face to what follows it: leave a
  region uncovered to light it from the dynamic GI volume or cubes. A face
  lying exactly on the volume's boundary and facing out samples half a cell
  past it and takes half the volume's share, so let the volume reach a cell
  past what it should light whole. Ambient
  occlusion occludes it as it does the ambient, and
  `FrameInput::baked_lighting` turns it off with the charts and cubes. Its
  own light is not a scene light: a fixture written into the field is not
  also added as a baked `Light`, or the field leaves its light out.
- **Sky specular.** `sky_visibility` also darkens the sky's share of a
  surface's environment specular (Lagarde's specular occlusion), so a cave's
  walls stop reflecting the sky; specular probes, which a game captures
  where they are, keep theirs. A probe captured before a relight is the
  game's to capture again.
- **Scrolling.** Install the same cell size and counts at a new origin: the
  volume moves by the nearest whole number of cells (read the placement back
  with `Scene::irradiance_volume`), keeps the cells that stay, and starts
  the cells that enter as `IrradianceCell::default()`, for the game to write.
  The copy is submitted on the queue at once. Another cell size or count, or
  an origin off the lattice, is a new placement whose cells all start as the
  default. `Scene::move_origin` translates the volume and keeps its cells.
- **Cost and limits.** 48 bytes a cell (160 × 128 × 160 cells, 157 MB), three
  3D taps per lit fragment, a region write's 48 bytes a cell through the
  queue, and on a scroll the cells that stay copied in place through a
  stripe 16 cells thick across the axis, which stays with a stripe of zeros
  once for each axis scrolled along (each about 2% of a volume 160 cells
  across it). The texture is
  `cells.x × 2 cells.y × 3 cells.z` texels within the device's
  `max_texture_dimension_3d` (2048 at WebGPU's default: 2048, 1024 and 682
  cells), else `SceneError::DeviceLimit`; a placement that is not a lattice
  is `SceneError::InvalidIrradianceVolume` and a region with invalid cells
  `SceneError::InvalidIrradianceRegion`. The browser runs the same volume.
- **Example.** [`examples/irradiance_volume.rs`](examples/irradiance_volume.rs)
  lights a block world's cave from a propagated sky and torch light field at
  a metre, relights a torch as a region write, scrolls the volume with the
  camera while the render origin follows it, and prints the per-pass GPU
  time and what relights, scrolls and whole installs cost.

## Dynamic diffuse GI

A dynamic GI volume gives static and moving surfaces coloured bounce light
from the frame's lights, the scene's lights, emitters and the sky, kept up
by rays each frame without a bake: a port of Wicked Engine's DDGI (Majercik
et al. 2019). The game places a lattice of probes over the part of its world it
wants lit by bounced light:

```rust
scene.set_dynamic_gi_volume(
    &device,
    Some(DynamicGiVolume {
        origin: Vec3::new(-4.7, -0.7, -4.7), // the first probe
        spacing: Vec3::splat(1.33),          // metres between probes
        probes: [8, 5, 8],
    }),
)?;
```

Each frame the dynamic GI stage, first after prepare, traces rays from the
probes through the scene's ray source (static and moving instances that do
not deform), from either side of every triangle. A hit is lit by one light
drawn from the frame's directional lights and the scene lights whose range
reaches the volume, with one ray toward it for its shadow at the light's
shadow opacity where the light casts a shadow (no shadow map, so nothing it
lights leaks through walls into the probes), by its emission, and by the
volume's own last frame for further bounces; a miss takes the environment
and the hemisphere fill along the ray. A ray that meets a single-sided
surface from behind takes no light from it and counts the surface nearer,
so the probe keeps out what lies behind it. Each probe keeps its irradiance and
the distances to what surrounds it, so a surface takes light only from the
probes that see it.

A glowing fixture that is also a scene light, such as a lamp's panel beside
its rectangle light, would reach the probes twice: as the emitter their rays
meet, and through the light, which already lights everything the fixture
would. Mark its material so the probes' rays take none of the light it gives
off itself (its emission, and an unlit material's whole colour):

```rust
for material in &mut asset.materials {
    if material.name == "lamp-panel" {
        material.emits_into_gi = false; // its rectangle light carries it
    }
}
```

`SurfaceMaterial::emits_into_gi` changes it later with `Scene::set_material`.
The surface still glows on screen, shows in reflections, blocks the probes'
rays and, when lit, reflects the light that reaches it, as Unity's emission
"Global Illumination: None" keeps a material's glow out of its GI while the
object stays in it.

Surfaces take their indirect diffuse light by one determination: a
lightmap or irradiance atlas chart keeps its bake; else the
[irradiance volume](#irradiance-volume) where it lights the surface; else
the dynamic GI volume lights the surface, in place of the environment's diffuse light and the hemisphere
fill, which ambient occlusion then occludes as it did them; else a moving
instance takes its ambient cube, else the frame's ambient. The camera's
opaque and blended surfaces, probe captures, world-space ray hits and the
probes' own hits sample the same volume. `SurfaceMaterial::environment_scale`
scales the environment, not the volume. Beyond its extent the volume's share
fades out over one spacing; ambient cubes stay the moving instances'
fallback there, with `Settings::dynamic_gi` at `Off`, and without a volume.

Placement:

- Cover every surface the volume should light: one to a few metres apart
  suits rooms and streets. A volume need not cover the world.
- Probes may lie beyond walls, inside closed geometry or outside a room
  built of single-sided walls facing inward: a probe that sees a
  single-sided surface from behind takes nothing from beyond it, and the
  receivers on the surface's other side weigh it as occluded. A probe more
  than a quarter of whose view is such backs is inactive: it lights nothing
  and traces few rays, so probes inside walls cost little. A probe with no
  surface within a spacing of it lights moving instances alone and traces
  few rays, unless a moving instance comes within that spacing: so a probe
  beyond a room's corner lights none of its walls, and probes in open air
  cost little yet light whatever moves among them. A static object so small
  that no probe about it finds it (a probe classifies from 32 fixed
  directions, all of them on its second turn and over each 8 of its turns
  after) takes its other indirect light. A probe's class follows a change
  in what it sees within 8 of its turns: 8 frames within a spacing of the
  camera, up to 64 at 128 spacings while the budget holds. Keep probes
  off the surfaces themselves, where a probe sees both sides at once:
  offset the lattice so walls fall between probes. Probes near a surface
  move off it, up to half a spacing, as they trace.
- A receiver tests which probes it sees from a point offset toward the
  viewer by about a quarter of the least spacing, so a surface does not
  shadow itself. A surface nearer a wall than that, facing it and seen
  head-on, can take light from beyond the wall; a closer spacing where
  surfaces face walls closely keeps it inside.
- A light that casts no shadow (`Light::casts_shadow` false, or a
  directional light without the frame's cascades) lights the probes as it
  lights surfaces, unoccluded, through walls too: give a light that should
  stay in its room a shadow.
- To keep a volume about a moving camera, install it again each frame with
  its origin moved by whole spacings (round the camera's position on the
  lattice): the scene scrolls it, the probes that stay keeping their light
  and those that enter starting afresh. An origin off the lattice by more
  than a small tolerance of the spacing, or another spacing or count, is
  another placement, which starts every probe afresh, as does a frame that
  does not run them (another scene, the setting `Off`). Installing the same
  placement changes nothing; camera cuts, resizes and `Scene::move_origin`
  keep the probes, and after a move `Scene::dynamic_gi_volume` gives the
  placement in the new frame to scroll from.
- A restart, or a scroll's entering planes, starts as many probes a frame
  as the frame's ray budget holds beside the probes already started (at
  least half of it), nearest the camera first: about 126 near the camera
  at High and 124 at Low, more farther out, where a probe starts with
  fewer rays. Until a probe has started it lights nothing, and the
  surfaces about it keep their other indirect light. Wicked starts every
  probe in one frame, a hitch on a large volume.
- Memory is about 9 KB a probe at High (7 KB at Low): its irradiance and
  depth maps, its share of the frame's ray results and the blends'
  history; and a ray list of the frame's budget, 256 KB at High (128 KB at
  Low). A volume is refused with `SceneError::DeviceLimit` where its
  probe texture (18 texels a probe along x times y) or its rays exceed the
  device's largest 2D texture, and with `SceneError::InvalidDynamicGiVolume`
  for a spacing that is not positive or fewer than two probes on an axis.

`Settings::dynamic_gi` (`DynamicGiQuality`: `Off`, `Low`, `High`; `High` by
default) sets the most rays a probe traces on a turn, 256 or 128, and the
frame's budget of rays, fixed rays included, 32,768 or 16,384 (Wicked's
surfel GI budget). A probe traces on its turns: every frame within a
spacing of the camera, every eighth frame at 128 spacings and less often
beyond, with an eighth of the most rays out there. On a turn it traces
rays by how consistent its irradiance is, a tenth of that outside the
camera's view, at least four: a probe whose light settles traces few, and
one whose light changes traces up to its most, and takes extra turns from
the rays the frame's turns leave, so cost follows change and a moved light
is answered sooner. Where the turns ask for more than the budget, every
probe's turns are spaced out by the same power of two, so none starves and
near probes keep up the most. On Hyperdrive's 3,179-probe course at High
the rays pass costs about 3.6 ms a frame in motion and 1.7 ms with the
camera still on an Apple M5 at 1920x1080 (1.7 and 1.2 ms at Low).
Once its light has converged, the volume pauses: it traces nothing and its
probes hold their light, until something its light follows changes. That
is any edit to the scene that changes what rays see or light (an instance
that does not deform, a model, material, light, decal, environment, bake
or the irradiance volume), the frame's directional lights, hemisphere fill
or environment, `Settings::dynamic_gi`, or the volume's placement; the
camera and the clock are not, nor a deforming instance's animation unless
hardware ray tracing traces the volume's rays, which then see it. Setting a
pose or a value to what it already is changes nothing. A scene whose
materials scroll their normal maps changes every frame and never pauses.
Frames that run the volume trace the ray source, so its instance BVHs
rebuild on them as for world-space reflections. Deforming instances are
lit by the volume, and block its rays only with hardware ray tracing. Per-pass cost is reported in
the `dynamic GI *` timing groups. The [dynamic GI example](examples/dynamic_gi.rs)
lights a room through a window and with a lamp, two boxes moving through
it; `--timing` prints the stage's GPU time in each frame as its probes
start and settle, `--counters` each frame's probes, rays, stride and BVH
node visits (`Diagnostics::dynamic_gi`), `--still` parks the boxes so the
volume pauses and `--edit N` moves the lamp at frame N, which starts it
again, `--quality off` renders the room without the volume, and
`--scroll` walks the camera along the room with a shorter volume that
scrolls to follow it:

```sh
cargo run --release -p sgl-3d --example dynamic_gi -- target/dynamic_gi.png --timing
```

## Hardware ray tracing

A native device with wgpu's ray queries traces the scene's rays in
hardware: world-space reflections' rays and the dynamic GI volume's probe
rays and visibility rays go through the scene's acceleration structures
instead of the portable BVHs, through the same functions and the same hit,
so they see what they saw before and, in addition, skinned and morphed
instances. It is opt-in: off by default and in every preset, a game takes
it by requesting the feature and turning `Settings::hardware_ray_tracing`
on. Elsewhere, or with the setting off, rays traverse the portable BVHs.

- **Device.** Request `graphics_device::ray_tracing_features(&adapter)`
  with `graphics_device::limits(&adapter)`, which requests the adapter's
  acceleration-structure limits. wgpu 29 marks the feature experimental, so
  the descriptor's `experimental_features` must be
  `unsafe { wgpu::ExperimentalFeatures::enabled() }`, the game's
  acceptance of it. Metal has it from macOS 15 on Apple silicon (in
  hardware from M3), Vulkan with ray queries, and DX12 at ray-tracing
  tier 1.1, which needs DXC: enable wgpu's `static-dxc` in the game or
  ship `dxcompiler.dll` beside its executable (wgpu's default
  `Dx12Compiler::Auto` takes either, never FXC). No browser has it, nor
  Vulkan on a Mac. `sgl-3d` enables no `static-dxc` itself.
- **Structures.** While `Settings::hardware_ray_tracing` is on, frames
  that trace rays (world-space reflections, the dynamic
  GI volume, ray-traced shadows) build, in the frame's encoder after the deform pass: a BLAS
  for each model that does not deform, has no masked mesh and has an
  opaque one, over all its meshes' positions in the scene's ray source,
  pending until such a frame and then built nearest the camera first under
  Bevy's budget of 400 000 vertices a frame (the first pending always, a
  replaced model at once), then compacted; a BLAS for each deforming
  instance over its deformed positions, rebuilt in each frame its
  deformation changed; and a TLAS over the capture-visible instances,
  rebuilt every such frame, each instance named by its entry's index with
  its kind (static or moving) as its mask. An instance of a model with a
  masked mesh, of one whose BLAS is pending, or that the device cannot hold
  (a model past `max_blas_primitive_count` or `max_blas_geometry_count`,
  instances past `max_tlas_instance_count`, the farthest left out, or a
  structure its memory cannot hold) stays on the portable BVHs, which then
  cover those instances alone; a deforming instance the device cannot
  hold is seen by no ray, since the portable BVHs never hold deforming
  instances. Turning the setting off frees the
  structures; a scene on a device without the feature holds none. A mesh's
  indices past its last whole triangle are left out of its BLAS, as of its
  BVH. The one allocation no error scope reaches is the builds' scratch
  buffer, which wgpu allocates when the game finishes the frame's encoder:
  its out-of-memory error reaches the game's error handler, as any failure
  of its encoder does.
- **Tracing.** Every native backend runs one form, the baseline: each ray
  asks the hardware for the nearest opaque hit over the kinds it wants
  (any hit for a visibility ray), and the same acceptance rules as the
  portable path judge it: a back face a ray's side policy rejects (a
  camera-origin ray rejects a single-sided material's, judged from the
  triangle's own winding, so mirrored instances keep their sides), a
  blended mesh of a model that also has opaque ones, a hidden visibility
  group, the reflecting surface's own triangle, a cut-out texel of a
  masked deforming instance. A rejected hit is traced past, up to 256
  queries a ray, after which the ray reports a miss: a ray through
  hair cards spends one for each cut-out card it crosses, at most 29 in
  a crowd of 24-card bundles. The portable BVHs trace the
  instances the TLAS does not hold, cut-out texels included, and the
  nearer hit wins. A deforming instance's hits take its deformed
  positions, normals and tangents, and a masked mesh on it cuts out at the
  hit. One limitation: a triangle at exactly the distance of a rejected
  one (back-to-back single-sided faces, coplanar meshes of a shown and a
  hidden group) may be skipped, since one opaque query cannot list ties.
  While hardware ray tracing traces the dynamic GI volume's rays, a
  capture-visible deforming instance's pose or deformation is an edit that
  wakes a converged volume, since its rays see it. Vulkan and DX12, whose
  shader compilers can run the hardware's candidate loop, also have a
  candidate form, in which masked models join the acceleration structures
  and the loop cuts out their texels; it is not their default until it is
  measured on their hardware, and it has not run on such hardware yet. A
  device whose candidate programs fail to compile falls back to the
  baseline for good.
- **Reporting.** `Renderer::ray_tracing_in_effect(&settings)` says whether
  the hardware path traces the scene's rays, as
  `antialiasing_in_effect` does for antialiasing, and
  `Renderer::ray_tracing_error()` why it did not trace the last frame that
  asked for it (the device has no ray queries, or its memory could not
  hold the TLAS), the portable BVHs tracing it instead, or why the device
  fell back from the candidate form to the baseline, which traced it.
  `Renderer::ray_tracing_stats()` returns the last rendered frame's
  `RayTracingStats`: the instances the TLAS held, those that do not deform
  it did not hold (on the portable BVHs), and those the device left out.
  With the `diagnostics` feature, `diagnostics::counters` counts the BLAS
  and TLAS builds and compactions, and `Scene::diagnostic_resources` the
  BLASes held and their triangles.
- **Ray-traced shadows.** With `Settings::ray_traced_shadows` on too (off
  by default and in every preset), the camera's opaque surfaces take their
  shadows from rays instead of the shadow maps, as Wicked Engine's
  ray-traced shadows do, for up to sixteen lights: the directional light
  with the frame's cascades, and up to fifteen of the casting local lights
  the local-light atlas places, in its ranking by screen coverage, each
  keeping its place while the atlas places it. At half the render size,
  each pixel casts one ray toward each of those lights that reaches it,
  from 1 cm past the surface to the light's position, or toward the
  directional light as far as the scene reaches, beyond the shadow's
  distance; a single-sided surface occludes from its back, as a map draws
  its front from the light, and a double-sided one from either side. The
  visibilities are blended with the previous frames' (a light that takes
  another's place, and a camera cut, start afresh) and upsampled to the
  render size by depth, then the lighting pass takes them through the
  light's shadow opacity. Shadows are hard: lights have no size yet, and
  the denoiser Wicked runs on its first four lights is not run. The opaque
  stage takes its two-pass form while they run (a G-buffer pass, then a
  lighting pass), so the setting costs the second geometry pass besides
  the rays. The volumetric fog, blended surfaces, probe captures,
  reflections' ray hits and the lights beyond those sixteen keep the maps,
  which are still drawn. `Renderer::ray_traced_shadows_in_effect(&settings)`
  says whether the setting is in effect; they then run on the frames where
  a light holds a slot. Without hardware ray tracing in effect the maps
  shadow everything.

The [streaming example](examples/streaming.rs) and the
[dynamic GI example](examples/dynamic_gi.rs) opt in with
`--hardware-ray-tracing`; the streaming example takes ray-traced shadows
with `--ray-traced-shadows`.

## Settings and capability fallback

`settings::Settings` holds every rendering setting a game chooses in one serde
value the game stores and passes to `Renderer::new`, `resize` and `render`;
`Settings::default()` is High with atmosphere allowed.
[Settings](docs/settings.md) lists each field. The game owns its settings
record, file format, controls, defaults, and saving. `settings::FrameRate`
describes presentation cadence; the renderer does not run a frame limiter.

Keep a saved explicit choice distinct from `Preset`. Apply the preset only where
the option requests it. Low → High → Low and application restart must preserve
explicit values at the library boundary. A game may implement complete preset
bundles by explicitly replacing its settings fields when it applies one.
`SceneResolution::Hd` and `FullHd` fit the scene within 1280×720 and 1920×1080
physical pixels, preserving aspect ratio without upscaling. `Full` renders at
the full output size; `ThreeQuarter`/`Half` scale it. UI output is unaffected.
FSR2 upscales to that scene size from its quality's render size.
Preserve the highest implemented fidelity as a selectable choice.

## Browser (WASM + WebGPU)

The browser is a first-class, maintained target ([D-20](../../specs/decisions.md),
[D-27](../../specs/decisions.md)), not a parity target: features or
speed-ups the browser cannot have fall back or are absent there. The
same `Scene`, `Renderer` and frame run on the page's WebGPU device, built for
`wasm32-unknown-unknown` (sgl-3d enables wgpu's `webgpu` backend). WebGL2 is
not supported, since SGL3D needs compute.

- **Device.** Request it as natively, with `graphics_device::limits` (the
  adapter's limits: 21 sampled textures and 8 storage buffers per stage are
  the floor, S3D-1) and `graphics_device::features`. Desktop Chrome on Apple
  silicon reports 48 sampled textures and 10 storage buffers from Chromium
  149; Chromium 145 reported 16 and cannot run SGL3D. Render into an
  offscreen texture or a canvas surface; the page owns the canvas.
- **Optional features** degrade as on any device: without
  `DEPTH_CLIP_CONTROL` directional shadow casters emulate unclipped depth in
  their shader; without `RG11B10UFLOAT_RENDERABLE` bloom's chain is RGBA16F;
  without `TEXTURE_COMPRESSION_BC` (typically mobile GPUs, which offer ETC2
  and ASTC instead) BC7 material images, BC6H probes and baked
  lightmaps are refused with `SceneError::CompressionUnsupported` or
  `ProbeError::CompressionUnsupported`, so ship RGBA8 images there; without
  `TIMESTAMP_QUERY` `GpuTiming::new` returns `None`. Chrome quantizes
  timestamps, so browser pass times are coarse.
- **FSR2** needs native-only features (texture atomics, 16-bit normalized and
  adapter-specific formats): chosen in the browser it resolves to TAA,
  `Renderer::antialiasing_in_effect` reports TAA and `fsr2_error()` names the
  missing features. TAA, both screen-space reflection methods, world-space
  reflections, dynamic GI, ambient occlusion, fog, motion blur, bloom and
  SMAA run.
- **Inputs, not clocks or files.** Fetch asset bytes and load them with
  `asset::load_slice` or `load_slice_with_options`; `asset::load` reads a file
  and fails in the browser. The frame time comes from
  `FrameInput::frame_time_ms`; SGL3D reads no clock.
- **No blocking readback.** `Renderer::capture_specular_probe` blocks for its
  readback, which WebGPU cannot: in the browser it fails with
  `ProbeError::Readback`. Capture natively and load the baked probes. The
  `diagnostics` feature's `diagnostics::read` also blocks and is native-only.

`browser_smoke` (`examples/browser_smoke.rs`) is the browser lane's test and a
minimal page integration: device creation; procedural content (a textured
ground, a shadow-casting box, a masked grate with a BC7 image, a blended
pane that receives screen-space reflections, a skinned and morphed box with an ambient cube, a box lit by a BC6H/BC7
static irradiance atlas, point, spot and rectangle lights, a decal, a BC6H
specular probe, a dynamic GI volume, an irradiance volume written by region,
glow, heat shimmer, mist, a fog volume and an environment);
frames under four settings configurations; an asynchronous readback; and
error scopes. It loads no glTF and sets no lightmap or mesh LODs.

## Asset and environment limits

`LoadOptions { nodes: Some(&|name| name == Some("head")), ..Default::default() }`
selects mesh nodes, from a file or bytes, by a caller-owned predicate for
rigid-part animation. Each mesh node is tested independently; excluded
parents still contribute their transforms. Selection combines with the other
options and uses the same decoding, material support, validation, and
batching as a whole load. An empty selection is an error. Embedded imports
require embedded buffers, and embedded images unless the game supplies them
(`LoadOptions::images`); games add their asset label to errors. A
`LoadOptions` borrows its callbacks, which are `Sync` so one value can serve
loads on several threads: build it where it is used, or store a
`LoadOptions<'static>` of `static` or leaked callbacks.

The loader supports glTF triangle meshes, baked rigid node transforms,
skins with four influences per vertex, morph targets, animation clips as data
([Skinned meshes and morph targets](#skinned-meshes-and-morph-targets)),
UV0, metallic/roughness materials, the opaque, masked and blended alpha modes,
normal and bump maps, scalar clearcoat, emissive strength, unlit materials, and
`KHR_materials_anisotropy`. Unsupported visible features return
asset-path errors. Occlusion textures and unimplemented material extensions
require a separate implementation or an explicit game-side export
adaptation.

On the GPU the scene keeps each `asset::Vertex` (88 bytes) in 32, packed by
`PreparedModel::new` after Godot's attribute compression: the position
exact; the normal and tangent as one rotation, each within 0.01°, the
tangent made perpendicular to the normal and unit; the UV as 16-bit
fractions of its mesh's UV rectangle, within the rectangle's extent over
131,070 per axis, so a mesh whose UVs span many repeats loses precision in
proportion; the colour as 8-bit sRGB with linear alpha, clamped to 0..1 as
glTF's `COLOR_0` is; the lightmap UV as 16-bit fractions (a negative one is
unassigned, as before); and the lightmap chart bounds once per distinct
chart in a table of its mesh's. A vertex normal must be finite and not
zero (`SceneError::NonFiniteGeometry`), and a mesh's vertices may name at
most 65,536 distinct `lightmap_bounds` (`SceneError::TooManyLightmapCharts`,
which names the mesh's index); a model's meshes together may name any
number.

Material textures are filtered trilinearly with the material's glTF
wrapping, and anisotropically up to `Settings::anisotropic_filtering`
(`settings::AnisotropicFiltering`: Off, 2×, 4×, 8× by default, or 16×, as
Godot's levels).

### Compressed material images

`asset::Image` is either decoded RGBA8 texels (`Image::Rgba8`, what glTF
loading yields), whose mip chain the scene filters when the image is added,
in linear light where a material samples it as colour, or a block-compressed
chain (`Image::Compressed`), uploaded as stored with no mip generation, at a
quarter of RGBA8's memory. Games compress in their export step and supply
the chains as the glTF loads, so its own images are never decoded:

```rust,ignore
use sgl_3d::asset::{self, CompressedImage, GltfImage, Image, ImageSource, LoadOptions};
// Each glTF image by its index, name and, for an external file, its URI;
// return the game's chain, or `Decode` for the glTF's own.
let sources = |image: GltfImage<'_>| -> asset::Result<ImageSource> {
    Ok(match image.uri.and_then(|uri| uri.strip_suffix(".png")) {
        Some(stem) => ImageSource::Supplied(Image::Compressed(
            CompressedImage::from_ktx2(&std::fs::read(dir.join(format!("{stem}.ktx2")))?)?,
        )),
        None => ImageSource::Decode,
    })
};
let asset = asset::load_with_options(&path, LoadOptions { images: Some(&sources), ..Default::default() })?;
```

The loader asks once per image, in image order, as Bevy's glTF loader
resolves each image's source; a supplied image takes the glTF image's index,
which its materials address. An embedded glTF (`load_slice_with_options`)
may refer to external image files when every one is supplied.
`CompressedImage::from_ktx2(bytes)` reads a KTX2
file of one 2D BC7 image (`VK_FORMAT_BC7_UNORM_BLOCK` or `_SRGB_BLOCK`) with
its stored levels, uncompressed or Zstandard-supercompressed, as Bevy reads
them. As for RGBA8, the channel picks the colour space: base and emissive
maps sample it as sRGB, the others as linear data. Filter a colour image's
levels in linear light. Its stored levels serve every channel that samples
it, so a data channel sampling an image also used as colour reads the
colour-filtered levels: a data use that needs its own filtering needs its
own image. An image sampled both ways is one texture with an sRGB view,
which needs `DownlevelFlags::VIEW_FORMATS` (wgpu's GL backend lacks it:
`SceneError::CompressedImageViews`). `from_ktx2` reads bytes, so its caller
adds the asset's path to its errors. Adding one needs
`wgpu::Features::TEXTURE_COMPRESSION_BC` (`SceneError::CompressionUnsupported`
otherwise); its sides must be multiples of 4, as wgpu requires of a
block-compressed level 0, and it holds one to a full chain of levels, each of
its size (`SceneError::InvalidCompressedImage`). Rays read level 0's stored
blocks, decoding each texel they sample as raster's level 0 decodes it: one
byte a texel in the ray source beside the texture's one. Basis Universal
transcoding, for a device without BC, is not provided. The `offscreen`
example's `--alpha` grate is a BC7 KTX2 chain, which
`cargo run -p sgl-3d --example export_grate` writes.

For anisotropic materials, supply `Vertex::tangent = [tx, ty, tz, handedness]`
with handedness +1 or -1, plus authored normals. Legacy meshes may leave the
appended tangent at `[0.; 4]`. `Material::anisotropy_strength` is finite 0..=1;
`anisotropy_rotation` is counter-clockwise radians; `anisotropy_texture` selects
linear RG direction (remapped to -1..1) and B strength. No texture means +T and
full texture strength. File and embedded glTF imports use the same rules.
Active anisotropy requires authored tangents; automatic tangent generation is
not implemented. Normal maps and anisotropy share that frame. Node and instance
transforms preserve mirrored handedness, nonuniform scaling and shear.

`Scene::set_material` exposes `SurfaceMaterial::anisotropy_strength` and
`anisotropy_rotation`; the anisotropy map, like every map, stays as the
material was added. Invalid values, or activation
while any mesh drawn with the material (in any model, or as a level of detail)
lacks tangent frames, return an error without changing CPU/GPU material state.
Tangent-capable LOD replacements must preserve tangent frames, including when
strength is currently zero. Adding a material with invalid anisotropy, or a
mesh without tangent frames drawn with an anisotropic material, returns a
`SceneError`.

Directional, point and spot lights and secondary hit shading use anisotropic GGX;
rectangle lights use the isotropic GGX fit, as Bevy's do. The base
lobe in environment and connected probes uses the KHR bent-normal approximation;
clearcoat and screen-space rays remain isotropic. Existing isotropic DFG compensation
and reflection filters remain approximations: narrow reflected lights can have
large errors. [Equations and measured limits](ANISOTROPY.md)
separate shader conformance from content fidelity. Receiver transport adds one
RGBA16F target (8 bytes/pixel); fused rendering needs eight color attachments and
64 attachment-budget bytes, with separate material rendering on lower limits.
The procedural example supports `--anisotropy 0.5` to exercise this path.

Stable normal RGBA16F stores signed octahedral world-space base normals in RG and coat
geometry normals in BA. Each pair uses Bevy's signed `[-1, 1]` octahedral
coordinates and `gbuffer_octahedral_decode` (see `src/shading/gbuffer.wgsl`);
there is no unsigned remapping, preserving binary16 precision around zero.
Stable material RGBA16F stores coat roughness, base
roughness, coat strength and environment scale. Metallic reflectance is already
carried by F0; the material target does not duplicate metallic. The mapped base
normal controls base environment/probe lighting; the geometry coat normal controls
coat lighting and the traced coat lobe, including when SSR is off.

`environment::EnvironmentMap` holds a caller-selected panorama and its matching
Three-compatible prefiltered PMREM atlas; `Scene::add_environment` adds one,
and `FrameInput::environment` names the frame's. PMREM data is RGBA16F with
explicit width/height; the library uploads supplied data rather than reading
game paths or requiring a browser at runtime. Environment authoring/export remains with the
game. The current representation is an extracted renderer boundary, not a
complete environment asset pipeline.

## Code structure

[SGL3D architecture](../../specs/sgl3d-architecture.md) is the shape this code
follows: its layers, the stage order, the shared contracts and the rules for
changing them. `src/lib.rs` declares the layers and re-exports the public
surface. The layers, in order: `src/content/` (CPU data and loading: `asset`
and its glTF import, `lighting`, `light`, `decal`, `static_lighting`, `environment`,
probe descriptions, ...; no wgpu), `src/shading/` (the WGSL library composed by
`shading::compose`, and its Rust mirrors), `src/scene/` (`Scene`, its GPU
buffers and the ray-query structure built from its geometry), `src/view/`
(views, draw lists, the geometry pipeline cache, and what the renderer lends
stages: the frame's context, the effective configuration, the shared targets,
group 0 and DiligentFX's context), `src/stages/` (one module per stage, whose
documentation states what it reads, writes and honours) and `src/renderer/`
(`Renderer`, the frame's order in `frame.rs`, the effective configuration's
resolution in `effective.rs` and the probe capture's order in
`probe_capture.rs`).

## Validation and diagnostics

From the SGL root:

```sh
cargo check --locked -p sgl-3d
cargo test --locked -p sgl-3d
cargo check --locked -p sgl-3d --features diagnostics
```

The required check also builds the crate for `wasm32-unknown-unknown` and runs
its pure tests there, and its browser lane renders the `browser_smoke`
example on WebGPU in headless Chromium
([Browser](#browser-wasm--webgpu)). GPU/reference tests marked ignored require their documented adapter/tools or
retained capture inputs; the ordinary test command does not establish those
results. Run relevant cases explicitly when changing their boundary. The
`diagnostics` feature adds `Settings::diagnostics` (`settings::Diagnostics`,
not serialized: switches that turn a layer off, the numerical frame probe
and the tone-target capture), `Renderer::diagnostic_target`,
`Renderer::take_frame_probe_reports`,
`Renderer::geometry_stats_for_model` (the camera's sections of one model's
instances, read back with `geometry_stats`), `diagnostics::read`,
`diagnostics::source_id`, the value the source-identity target holds for an
instance's pixels, `diagnostics::crystal_roughness_threshold`, where
Crystal stops tracing, `diagnostics::counters` (what `sgl-3d` itself counted on
the thread: uploads by file and line, buffers created, the steps of
preparing and placing models and of building instance BVHs, ray-source and
geometry-slab growths, static-edit boxes and the hardware path's
acceleration-structure builds and compactions; subtract
two with `Counters::since`, and compare versions by totals since lines move),
`Scene::diagnostic_resources` (the scene's buffer sizes and BLASes, and the
draw candidates', sets', level chains' and each GPU-built view's cluster
list's bytes), `Renderer::diagnostic_draws` (the last frame's camera, blended
and cascade draws as encoded: a GPU-built view's one per set),
`Renderer::diagnostic_view_times` (the CPU time the camera's and each
cascade's draw list took to build, preparing and encoding its cull, and to
record), and `Diagnostics::dynamic_gi` with
`Renderer::take_dynamic_gi_reports` (`diagnostics::DynamicGiReport`: each
observed frame's number, its dynamic GI probes, rays, BVH node visits and
budget stride and what kept the volume awake, read back without blocking;
take them each frame once its work completes, or a report counts the
frames skipped while 8 readbacks waited). The `streaming` and
`irradiance_volume` examples print these for their routes, the
`dynamic_gi` example's `--counters` its volume's reports, and
`bun scripts/tasks.ts measure-browser` the view times of the streaming world
in headless Chromium.
Diagnostics are
configuration: the library reads no environment variables and writes no files.
Normal rendering does not require the feature.

Follow the [rendering development rules](../../specs/sgl3d.md#rendering-development).
Existing reference fixtures and diagnostics may be deleted or regenerated when
the implementation they pin is replaced.

First-party code is MIT OR Apache-2.0. Retained SMAA and Three.js components
keep their original notices; see [third-party notices](../../THIRD_PARTY_NOTICES.txt).
Ported code keeps its licence text beside the source and is listed in
`scripts/license-notices.ts`.

### Spatial mesh LOD

Games can split a large model's mesh into spatial chunks and add authored
alternatives as other models that no instance places (a model draws all its
meshes, so an alternative never comes from the model it details). Then
register each chunk's detailed-to-coarse alternatives, at most
`lod::MAX_MESH_LODS` (8) a mesh (`SceneError::TooManyLods` past it):

```rust,ignore
scene.set_mesh_lods(base_model, chunk_mesh, vec![
    sgl_3d::lod::MeshLod { model: coarse_model, mesh: coarse_mesh, max_error: 0.01 },
])?;
```

`max_error` bounds displacement in the original mesh's local metres. Alternatives
must use the same coordinate system and preserve interpolated UVs, vertex colors,
normals and baked-light charts; geometric error alone does not bound shading error.
The game owns simplification and its numerical evidence. An empty alternative
can remove a tiny feature only when its error bounds the entire feature.
`set_mesh_lods(..., Vec::new())` restores the detailed mesh, and replacing the
base model's geometry clears its alternatives. A model named as an alternative
cannot be replaced or removed while it is.

All primary raster passes choose the last alternative whose conservative projected
error is at most 0.5 pixels, using the `FrameInput` camera and the render size.
The bound includes scale, perspective division and
distance to the nearest chunk corner; chunks crossing the near plane retain full
detail. The camera's opaque and masked surfaces choose on the GPU, by the
same bound computed in `f32` under margins that cover its rounding, so they
never draw an alternative coarser than the CPU bound admits, and may keep a
finer one at the margin; the blended list chooses on the CPU. Original object/material bindings retain lightmap and atlas lighting. Pulled
reflection-source geometry uses the selected alternative's actual triangle words,
not the original triangle IDs. Probes, shadows and scene rays deliberately retain
the original geometry: this feature reduces primary raster work, not ray geometry
storage or offscreen capture costs. No LOD registration leaves rendering unchanged.

`Renderer::geometry_stats(&device)` returns static- and moving-instance
(draws, triangles) pairs (`GeometryStats`, summed by `total()`) for the
primary raster population of the most recent completed frame, after
visibility, frustum, material and level-of-detail culling and before GPU
backface culling: its opaque and masked surfaces counted on the GPU, a draw
per section of at most 128 triangles, read back without blocking (the map is
requested at `finish_frame`), with its blended surfaces' instanced draws
from the same frame, a draw per batch and range, whose triangles count once
per instance. It is `None` until a frame's readback has arrived, a few
frames after the first. It excludes shadow, probe and ray work. With the
`diagnostics` feature, `Renderer::geometry_stats_for_model(&device, &scene,
model)` counts the same frame's sections of one model's instances and the
blended draws that hold one, with their triangles.

## Soft additive effects

`Scene::update_effects` takes a triangle list of `effects::Glow` vertices,
each with its `kind` (`GlowKind`), the same across a triangle:

- `Uniform`: the interpolated colour.
- `Tapered { uv, profile }`: alpha shaped over `uv` by a `GlowProfile`,
  whole at v = 0 and gone at v = 1 (`taper`), rippling in bands across u
  that slant along v (`ripple_frequency`, `ripple_amplitude`).
  `GlowProfile::default()` is SGL3D's own tapered sine.
- `Line { other, offset }`: a line one pixel wide toward the endpoint
  `other`, the vertex `offset` pixels across it (-0.5 and 0.5 span it).

```rust,ignore
use sgl_3d::effects::{Glow, GlowKind, GlowProfile};
let flame = |position, uv| Glow {
    position,
    color: [4., 1.5, 0.4, 0.8],
    kind: GlowKind::Tapered { uv, profile: GlowProfile { taper: 3., ..Default::default() } },
    soft_distance: 0.2,
};
```

Set `Glow::soft_distance` to a positive distance in metres for a linear
intersection fade against opaque primary geometry; use `0.0` for hard edges and
screen-space motion lines. Keep this field constant across each triangle.
`Glow::default()` is uniform, hard-edged and colourless.
The renderer honors it both where effects are drawn into the reflection input
and onto the composed frame, including own-depth volume transmission. Geometry
generation remains game-owned. See [the reference and numerical
evidence](SOFT_EFFECTS.md) for the supported projection and composition
boundaries.

## Bounded heat shimmer

`Scene::update_heat_distortion(queue, &[heat_distortion::HeatDistortion])`
accepts a world-space triangle list. The game authors coverage,
vertex displacement in scene pixels (X right/Y down), edge weights and animation:

```rust,ignore
use sgl_3d::heat_distortion::HeatDistortion;
let plume = positions.map(|position| HeatDistortion {
    position, displacement: [0.75, 0.25], weight: 1.0,
});
scene.update_heat_distortion(&queue, &plume)?;
settings.heat_distortion = true; // Settings: on or off; default is Off.
```

The retained list holds at most 6,144 vertices (2,048 triangles), with finite
positions, displacement within +/-32 pixels per axis and weights in 0..=1.
Invalid or oversized submissions return an error and retain the previous list;
an empty list clears it. Use zero weights on silhouette vertices. The library
adds no noise, clock, simulation, geometry generation or temporal history.

Composition snapshots the complete camera HDR source after reflections, fog,
blended surfaces, additive effects and mist, then overwrites
only submitted triangle coverage before bloom, antialiasing, tone mapping and
the game's HUD. Medium transport and additive attenuation are not applied again.
Each displacement samples the immutable snapshot; overlapping triangles use
submission order, without repeated refraction. Off/empty skips the copy and draw.
One full-size HDR snapshot is retained and replaced on resize; geometry storage
is retained. Thus Off avoids a full-frame copy plus bounded raster work.

The method follows Tiago Sousa, *GPU Gems 2* (2005), chapter 19,
[§19.1 and §19.2](https://developer.nvidia.com/gpugems/gpugems2/part-ii-shading-lighting-and-shadows/chapter-19-generic-refraction-simulation),
reviewed 2026-09-25. Conformance mapping: immutable scene texture corresponds to
S; submitted interpolated vectors replace the sampled normal-map XY field;
opaque depth replaces the alpha refraction mask. Every contributing bilinear
tap must be behind the plume's interpolated device depth, including at foreground
edges. Offscreen footprints fall back to the undisplaced pixel rather than clamp.
Zero weight and pixels outside coverage remain exact. This is approximate
camera-path shimmer, not physical refraction or reflected-ray distortion; already
composited medium is warped too. It does not reconstruct hidden scene layers,
refract successive overlapping surfaces, or contribute to reflection sources.
`stages::transparent::heat::tests::bounded_sampling` numerically exercises the actual shader
against a linear radiance ramp and independently rasterized foreground stripe,
including resize, zero displacement/weight and viewport rejection.
