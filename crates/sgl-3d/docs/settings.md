# SGL3D settings

The rendering settings a game chooses: which mode or implementation to use,
and how much. They are the fields of `settings::Settings`
([`src/settings.rs`](../src/settings.rs)), one serde value: store it in the
game's own settings record and pass it to `Renderer::new`, `resize` and
`render`. `Settings::default()` is High with every choice at its default. The
game decides which settings, if any, to show players, and owns the menu, the
save file and its presets. A setting says which one or how much, never how:
SGL3D chooses each feature's internals for the value.

## Presets

`Settings::preset` (`RenderPreset`: `High`, the default, or `Low`) is SGL3D's
quality tier.

A setting whose saved value is `Preset` follows the tier when it is used; its
saved value stays `Preset`. An explicit value is never changed by the tier.
Settings without a `Preset` value have no tier default, so the game's own
presets (for example Low, High and Ultra) must set them. A game preset should
replace every choice with a complete bundle.

## Settings

| Setting | Field | Values (default first) | `Preset` gives | Notes |
| --- | --- | --- | --- | --- |
| Antialiasing | `Settings::antialiasing` | `Preset`, `Taa`, `Smaa`, `Off`, `Fsr2` | Low: SMAA. High: TAA | FSR2 upscales as it antialiases. TAA runs where the device cannot run FSR2, which includes every browser (WebGPU lacks its features). TAA and FSR2 need a `perspective` camera: without one SMAA stands in for TAA, but an FSR2 frame is presented without SMAA. |
| FSR2 quality | `Settings::fsr2_quality` | `Quality`, `NativeAa`, `Balanced`, `Performance`, `UltraPerformance` | | Native AA renders every scene pixel; Quality, Balanced, Performance and Ultra Performance render 1/1.5, 1/1.7, 1/2 and 1/3 of it per axis. Set `FrameInput::frame_time_ms` every frame. |
| FSR2 sharpening | `Settings::fsr2_sharpening`, `Settings::fsr2_sharpness` | `true`, `false`; 0.8, any of 0..=1 | | AMD's RCAS sharpening of FSR2's output, as AMD asks games to offer: 0 sharpens least, 1 most; values outside are clamped, NaN to 0. |
| SMAA quality | `Settings::smaa_quality` | `Medium`, `Low`, `High`, `Ultra` | | SMAA 2.8's presets: Low and Medium search up to 4 and 8 steps of two pixels each way along an edge; High up to 16 and also smooths diagonal lines and keeps sharp corners; Ultra up to 32 and finds fainter edges. Higher costs more. |
| Anisotropic filtering | `Settings::anisotropic_filtering` | `X8`, `Off`, `X2`, `X4`, `X16` | | The most samples material textures take where a surface is seen at a grazing angle: sharper floors and walls at a distance, at more texture bandwidth. Off filters trilinearly. |
| Scene resolution | `Settings::scene_resolution` | `Preset`, `Hd`, `FullHd`, `Full`, `ThreeQuarter`, `Half` | Low: one pixel per logical pixel. High: up to 1.75, never above the output | `Hd` and `FullHd` fit within 1280×720 and 1920×1080. Pass the window's scale factor to `Renderer::new` and `resize`. The game's UI keeps the output size. |
| Bloom | `Settings::bloom` | `Preset`, `Off`, `On` | Low: Off. High: On | |
| Shadow quality | `Settings::shadow_quality` | `High`, `Low` | | The shadow maps' sizes and the camera's shadow filter, as Godot's desktop and mobile defaults. High: 2048-texel cascades, a 4096-texel local-light atlas, and soft filtering (turned each frame under TAA or FSR2). Low: 1024-texel cascades, a 2048-texel atlas, and one hard 2×2 tap. A change reallocates the maps on the next frame, which draws every shadow again. |
| Ambient occlusion | `Settings::ambient_occlusion` | `Off`, `Low`, `Medium`, `High`, `Ultra` | | 4, 8, 18 and 54 samples per pixel at scene resolution. Needs a `perspective` camera. |
| Screen-space reflections | `Settings::screen_space_reflections` | `Off`, `Half`, `Full` | | The resolution rays are traced at. Needs a `perspective` camera. |
| Reflection method | `Settings::reflection_method` | `Crystal`, `Velvet` | | Crystal is sharp and reflects only glossy surfaces (perceptual roughness below 0.2). Velvet blurs with roughness and reaches rougher surfaces (below 0.7), so it traces more of the screen. |
| World-space reflections | `Settings::world_space_reflections` | `Off`, `Moving`, `All` | | What rays fill screen-space reflections' misses with, up to 1000 m from the reflecting surface, at half resolution, through a software BVH, or the scene's acceleration structures with hardware ray tracing. `Moving` shows moving objects, which baked probes cannot hold; a static surface in front of one leaves the probe or sky reflection. `All` shows everything: static surfaces too, as they stand rather than as their probes recorded them, as Wicked Engine's ray-traced reflections do. `All` is meant for hardware ray tracing: on the software BVH every ray walks the static geometry too, which made the pass about 2.2 times `Moving`'s on a glossy test strip, against 1.2 times with hardware ray tracing. Rays are traced only at pixels that need them (smooth enough, not resolved on screen), so a view without such surfaces pays a classification pass (about 0.07 ms at 1920×1080 on an Apple M5) in place of the trace, and about 0.21 ms for the whole stage with its denoise passes, with or without hardware ray tracing. No preset turns either on. Needs screen-space reflections. |
| Hardware ray tracing | `Settings::hardware_ray_tracing` | `false`, `true` | | Opt-in: no preset turns it on. On a device requested with its feature (`graphics_device::ray_tracing_features`), frames that trace rays build acceleration structures over the scene, which cost memory and build time, and world-space reflections', the dynamic GI volume's and ray-traced shadows' rays trace them. On Metal masked models join them, their cut-out texels judged in the hardware's candidate loop; on Vulkan and DX12 they stay on the software BVHs. Those rays then see skinned and morphed instances too. Off frees the structures and traces the software BVHs alone. No feature needs it: without it the software BVHs trace those rays, and the shadow maps stand in for ray-traced shadows. `Renderer::ray_tracing_in_effect` and `ray_tracing_error` say whether it runs, `ray_tracing_stats` what it held. |
| Ray-traced shadows | `Settings::ray_traced_shadows` | `false`, `true` | | Opt-in: no preset turns it on. While hardware ray tracing is in effect, the camera's opaque surfaces take the shadows of the directional light with cascades and of up to fifteen casting local lights from rays at half resolution instead of from the maps: shadows soft by the light's size (a point or spot light's `radius`, the directional light's `angular_diameter`), the first four denoised by AMD's FidelityFX shadow denoiser (as the quality below says) and the rest temporally blended, that reach as far as the scene, beyond the directional shadow's distance. The fog, blended surfaces, reflections and other lights keep the maps. While it runs (a light holds a slot), the opaque stage takes its two-pass form, a second geometry pass. On an Apple M5 it cost 2.7–3.6 ms a frame natively on Metal at 1920×1080 on the examples' routes, the denoiser 1.9–2.1 ms of it at High. `Renderer::ray_traced_shadows_in_effect` says whether the setting is in effect; the stage then runs on the frames where a light holds a slot. A `baked` light casts no rays at receivers with baked lighting, which it does not light: lights a game bakes cost rays only at moving instances and unbaked static surfaces. Without hardware ray tracing the maps shadow everything: on the software BVHs these rays cost 3–5 times as much with local lights and over 8 ms a frame on a racing game's route, too much to offer. |
| Ray-traced shadow quality | `Settings::ray_traced_shadow_quality` | `Preset`, `Low`, `High` | Low: Low. High: High | How much of the ray-traced shadows is denoised while they run; without them it has no effect. High denoises the directional light's shadow and those of three of the local lights (the first three slots after it, each kept by the light that holds it). Low denoises the directional light's alone, in fewer passes, and blends those local lights' with the previous frames' as every other local light's: their soft shadows' edges are noisier, in motion most, for about 1 ms a frame less on an Apple M5 at 1920×1080 (the denoiser 0.9–1.1 ms against 1.9–2.1 ms). Where no local light holds one of those places, High denoises the directional light's alone too: the same result within one 8-bit step, for about 0.8 ms less. A change starts the shadows' history afresh. |
| Occlusion culling | `Settings::occlusion_culling` | `false`, `true` | | Opt-in: no preset turns it on. The camera skips its opaque and masked geometry hidden behind what it draws, tested twice a frame, so nothing visible is lost or shows a frame late. It costs the opaque stage's two-pass form and two depth pyramids a frame, so it pays only where a frame submits much hidden geometry: on an Apple M5 it cost 0.2–0.5 ms a frame natively on Metal on the examples' routes, and saved about 1 ms in Chrome over a large voxel window seen from the ground. Measure the game's own routes. Needs six storage textures a shader stage, else the camera culls by frustum alone. `Renderer::occlusion_culling_in_effect` says whether it runs. |
| Atmosphere | `Settings::atmosphere` | `true`, `false` | | The volumetric fog and mist, while the game turns `FrameInput::atmosphere` on (off by default, as Godot's fog). |
| Fog quality | `Settings::fog_quality` | `High`, `Low` | | The volumetric fog's froxels: 64 slices, and Low 64 square froxels across the geometric mean of the frame's sides (Godot's default), High 128. Higher resolves sharper shafts and shadow edges in the fog at more cost. Needs a `perspective` camera. |
| Fog filter | `Settings::fog_filter` | `true`, `false` | | Blurs each slice of the fog's froxels across the frame before integration (Godot's `use_filter`, on by default): smoother fog with softer shafts and shadow edges in it, for two passes over the froxels. |
| Dynamic GI | `Settings::dynamic_gi` | `High`, `Low`, `Off` | | The most rays each probe of the scene's dynamic GI volume traces on a turn, 256 or 128 at Low, and the frame's budget of rays, 32,768 or 16,384. Probes take turns, near the camera every frame and farther out less often, with fewer rays far away; turns past the budget are spaced out, so a large or busy volume costs at most its budget. A probe whose light has settled traces few at either; one whose light changes settles faster and with less noise at High. Off traces none, and surfaces take their indirect diffuse light as without a volume. Without a volume nothing runs. Off on a device of the `Basic` binding tier (`Renderer::dynamic_gi_in_effect`). |
| Volume paths | `Settings::volume_paths` | `true`, `false` | | Measures how far each view ray travels inside a blended volume whose shader reads `scene_volume_path`, for its absorption. Costs frames that draw such a material three copies of the opaque depth and three depth passes over its meshes (timing group `volume layers`), and three render-size depth targets held from the first such frame until it is turned off. Off, or on a device of the `Basic` binding tier, the shader gets `VOLUME_NONE` and takes its own fallback thickness. `Renderer::volume_paths_in_effect` says whether it runs. |
| Heat shimmer | `Settings::heat_distortion` | `false`, `true` | | Needs geometry from `Scene::update_heat_distortion`. |
| Motion blur | `Settings::motion_blur` | `Off`, `Reduced`, `Full` | | A comfort choice: Full blurs over `FrameInput::motion_blur`'s shutter, Reduced over half of it. Needs a `perspective` camera. |
| Frame rate | the game's loop, from `settings::FrameRate` | `Display`, `Fps60` to `Fps240` | | Not part of `Settings`: SGL3D does not pace frames. `FrameRate::limit` gives the cap. |

## When changes apply

The preset, scene resolution, antialiasing changes to or from FSR2 and FSR2
quality size the targets, so they apply at the next `Renderer::resize`, which
the game calls every frame. The rest apply on the next rendered frame; that
frame rebuilds SMAA's pipelines after an SMAA quality change, and every
material's sampler after an anisotropic filtering change.
`Renderer::antialiasing_in_effect(&settings)` reports what actually runs and
`fsr2_error()` why FSR2 did not; show the effective choice without rewriting
the saved value.

## Frame and scene values

These are the game's authored look or per-frame state, fields of `FrameInput`
([`src/frame_input.rs`](../src/frame_input.rs)), and its content on the scene
types. Under S3D-6 a game says which one or how much (a setting above, a
diagnostics switch below, or a value here), each with a default; how a feature
is done (its algorithm and internal parameters) is SGL3D's.

- `exposure`: fixed stops, or automatic exposure with its brightening and
  darkening speeds, limits, compensation curve and metering mask. SGL3D
  keeps Bevy's histogram range, outlier filter and exponential blend.
- `bloom`: intensity; SGL3D shapes the halo as Bevy's natural bloom.
- `motion_blur`: the shutter angle, the share of each frame's motion that
  blurs (0.5 by default).
- `color_grading`: white balance, hue, post-saturation, the midtone range,
  each section's saturation, contrast, gamma, gain and lift, and the AgX
  look (`AgxLook::None` by default, `Punchy` or `Golden`).
- `ambient_occlusion_radius`: occlusion reach in metres (0.5 by default),
  clamped to 0.01–10000; `Settings::ambient_occlusion` turns AO off.
- `directional_lights`, `hemisphere_light`: the lights that are not scene
  content. A directional light's `shadow` is its cascades' reach: distance
  (kept 1 mm beyond the near plane to 8192 m) and cascade count
  (`DirectionalShadow::default()` is Bevy's 150 m and 4); SGL3D places the
  splits (Godot's) and the pancake. `None` casts none. Its
  `fog_energy`, as a scene light's
  (`Light::fog_energy`), scales its light in the volumetric fog: 1 by
  default, at most 0.001 leaves it out. Its `shadow_opacity`, as a scene
  light's (`Light::shadow_opacity`), is how dark its shadow is on surfaces
  and in the fog: 1 by default, at most 0.001 draws none.
- `baked_lighting`: `false` turns baked lighting off.
- `atmosphere`: the volumetric fog and mist, off by default as Godot's
  (`volumetric_fog_enabled` and `fog_enabled`); `true` turns them on where
  the setting allows them; `fog` (the medium: density, height and falloff,
  albedo, anisotropy, the share of ambient light it scatters (none by
  default, as Godot's), the volume's length, and how much of it the sky
  takes (all by default, as Godot's)) and `mist` (its colours, opacity,
  billboard size and drift) shape them;
  SGL3D spaces the volume's slices and weights its history. Fog volumes are
  scene content (`Scene::update_fog_volumes`).
- `environment`: the scene's environment that lights the frame and draws its
  sky; `None` is black. `diffuse_environment` turns and scales its diffuse
  light, `backdrop` is its panorama or a colour.
- `reflection_environment`: turns and scales its specular light where no
  baked probe reflects.
- A material's `emits_into_gi` (`asset::Material`, `SurfaceMaterial`;
  `true` by default): `false` keeps the light it gives off itself (its
  emission, an unlit material's whole colour) out of global illumination,
  for a fixture a scene light stands for. Content, not a setting.
- A material's shader and what its functions read: `SurfaceMaterial::shader`
  (`MaterialShader`: the shader and its `displacement_bound`, none by
  default, set through `Scene::set_material`), its parameter block (`Scene::set_shader_parameters`, zeros by
  default), an instance's shader data (`Scene::set_instance_shader_data`,
  zero by default) and a mesh's per-vertex data
  (`PreparedModel::with_shader_data`). Content, not settings: a shader
  changes what a surface is, never how SGL3D renders it.

With the `diagnostics` feature, `Settings::diagnostics` holds investigation
switches (layers off, the frame probe, the tone-target capture, the
dynamic GI observer). It is not serialized and is never shown to players.
