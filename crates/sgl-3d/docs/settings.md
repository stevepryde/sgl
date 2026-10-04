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
| Scene resolution | `Settings::scene_resolution` | `Preset`, `Hd`, `FullHd`, `Full`, `ThreeQuarter`, `Half` | Low: one pixel per logical pixel. High: up to 1.75, never above the output | `Hd` and `FullHd` fit within 1280×720 and 1920×1080. Pass the window's scale factor to `Renderer::new` and `resize`. The game's UI keeps the output size. |
| Bloom | `Settings::bloom` | `Preset`, `Off`, `On` | Low: Off. High: On | |
| Ambient occlusion | `Settings::ambient_occlusion` | `Off`, `Low`, `Medium`, `High`, `Ultra` | | 4, 8, 18 and 54 samples per pixel at scene resolution. Needs a `perspective` camera. |
| Screen-space reflections | `Settings::screen_space_reflections` | `Off`, `Half`, `Full` | | The resolution rays are traced at. Needs a `perspective` camera. |
| Reflection method | `Settings::reflection_method` | `Crystal`, `Velvet` | | Crystal is sharp and reflects only glossy surfaces (perceptual roughness below 0.2). Velvet blurs with roughness and reaches rougher surfaces (below 0.7), so it traces more of the screen. |
| World-space reflections | `Settings::world_space_reflections` | `false`, `true` | | Shows moving objects that screen-space reflections cannot see, with rays through a software BVH at half resolution. Needs screen-space reflections. |
| Atmosphere | `Settings::atmosphere` | `true`, `false` | | The volumetric fog and mist, while the game turns `FrameInput::atmosphere` on (off by default, as Godot's fog). |
| Fog quality | `Settings::fog_quality` | `High`, `Low` | | The volumetric fog's froxels: 64 slices, and Low 64 across the frame's mean side (Godot's default), High 128. Higher resolves sharper shafts and shadow edges in the fog at more cost. Needs a `perspective` camera. |
| Fog filter | `Settings::fog_filter` | `true`, `false` | | Blurs each slice of the fog's froxels across the frame before integration (Godot's `use_filter`, on by default): smoother fog with softer shafts and shadow edges in it, for two passes over the froxels. |
| Heat shimmer | `Settings::heat_distortion` | `false`, `true` | | Needs geometry from `Scene::update_heat_distortion`. |
| Motion blur | `Settings::motion_blur` | `Off`, `Reduced`, `Full` | | A comfort choice: Full blurs over `FrameInput::motion_blur`'s shutter, Reduced over half of it. Needs a `perspective` camera. |
| Frame rate | the game's loop, from `settings::FrameRate` | `Display`, `Fps60` to `Fps240` | | Not part of `Settings`: SGL3D does not pace frames. `FrameRate::limit` gives the cap. |

## When changes apply

The preset, scene resolution, antialiasing changes to or from FSR2 and FSR2
quality size the targets, so they apply at the next `Renderer::resize`, which
the game calls every frame. The rest apply on the next rendered frame.
`Renderer::antialiasing_in_effect(&settings)` reports what actually runs and
`fsr2_error()` why FSR2 did not; show the effective choice without rewriting
the saved value.

## Frame and scene values

These are the game's authored look or per-frame state, fields of `FrameInput`
([`src/frame_input.rs`](../src/frame_input.rs)), and its content on the scene
types. Under S3D-6 a game says which one or how much (a setting above, a
diagnostics switch below, or a value here), each with a default; how a feature
is done (its algorithm and internal parameters) is SGL3D's.

- `exposure`: fixed stops, or automatic exposure with its histogram range,
  filter, speeds, limits, compensation curve and metering mask.
- `bloom`: intensity, low-frequency boost and its curvature, and high-pass
  frequency.
- `motion_blur`: the shutter angle, the share of each frame's motion that
  blurs (0.5 by default).
- `color_grading`: white balance, hue, post-saturation, the midtone range,
  and each section's saturation, contrast, gamma, gain and lift.
- `ambient_occlusion_radius`: occlusion reach in metres.
- `directional_lights`, `hemisphere_light`: the lights that are not scene
  content. A directional light's `shadow` is its cascades' reach: distance,
  cascade count, first split and pancake size
  (`DirectionalShadow::default()` is Bevy's 150 m, 4 and 10 m with Godot's
  20 m pancake); `None` casts none. Its `fog_energy`, as a scene light's
  (`Light::fog_energy`), scales its light in the volumetric fog: 1 by
  default, at most 0.001 leaves it out.
- `baked_lighting`: `false` turns baked lighting off.
- `atmosphere`: the volumetric fog and mist, off by default as Godot's
  (`volumetric_fog_enabled` and `fog_enabled`); `true` turns them on where
  the setting allows them; `fog` (the medium: density, height and falloff,
  albedo, anisotropy, the share of ambient light it scatters (none by
  default, as Godot's), and the volume's length) and `mist` shape them;
  SGL3D spaces the volume's slices and weights its history. Fog volumes are
  scene content
  (`Scene::update_fog_volumes`).
- `environment`: the scene's environment that lights the frame and draws its
  sky; `None` is black. `diffuse_environment` turns and scales its diffuse
  light, `backdrop` is its panorama or a colour.
- `reflection_environment`: turns and scales its specular light where no
  baked probe reflects.

With the `diagnostics` feature, `Settings::diagnostics` holds investigation
switches (layers off, the frame probe, the tone-target capture). It is not
serialized and is never shown to players.
