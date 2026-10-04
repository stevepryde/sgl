# SGL3D settings

The rendering choices a game can offer players. They are the fields of
`settings::Settings` ([`src/settings.rs`](../src/settings.rs)), one serde value:
store it in the game's own settings record and pass it to `Renderer::new`,
`resize` and `render`. `Settings::default()` is High with every choice at its
default. The game owns the menu, the save file and its presets. Offer a
setting only where it is a real trade-off for the player.

## Presets

`Settings::preset` (`RenderPreset`: `High`, the default, or `Low`) is SGL3D's
quality tier.

A setting whose saved value is `Preset` follows the tier when it is used; its
saved value stays `Preset`. An explicit value is never changed by the tier.
Settings without a `Preset` value have no tier default, so the game's own
presets (for example Low, High and Ultra) must set them. A game preset should
replace every choice with a complete bundle.

## Player settings

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
| Atmosphere | `Settings::atmosphere` | `true`, `false` | | The volumetric fog and mist, while `FrameInput::atmosphere` is on. |
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
`fsr2_error()` why FSR2 did not; show players the effective choice without
rewriting their saved one.

## Not player settings

These are the game's authored look or per-frame state, fields of `FrameInput`
([`src/frame_input.rs`](../src/frame_input.rs)). Keep them out of settings
menus. S3D-6 requires every behaviour that changes the image to be a player
setting above, a diagnostics switch below, or a value here or on the scene
type it belongs to, with a default so a game sets only what it changes.

- `exposure`: fixed stops, or automatic exposure with its histogram range,
  filter, speeds, limits, compensation curve and metering mask.
- `bloom`: intensity, low-frequency boost and its curvature, and high-pass
  frequency.
- `motion_blur`: the shutter angle, the share of each frame's motion that
  blurs (0.5 by default).
- `color_grading`: white balance, hue, post-saturation, the midtone range,
  and each section's saturation, contrast, gamma, gain and lift.
- `ambient_occlusion_radius`: occlusion reach in metres.
- `crystal`: Crystal's tracing and denoising parameters.
- `directional_lights`, `hemisphere_light`: the lights that are not scene
  content. A directional light's `shadow` is its cascades' reach: distance,
  cascade count and first split; `None` casts none. Its `fog_energy`, as a
  scene light's (`Light::fog_energy`), scales its light in the volumetric
  fog: 1 by default, 0 leaves it out.
- `baked_lighting`: `false` turns baked lighting off.
- `atmosphere`: `false` turns the volumetric fog and mist off whatever the
  setting; `fog` (the medium: density, height and falloff, albedo,
  anisotropy, the share of ambient light it scatters, the volume's length
  and detail spread, and how much of the last frame's volume it keeps) and
  `mist` shape them. Fog volumes are scene content
  (`Scene::update_fog_volumes`).
- `environment`: the scene's environment that lights the frame and draws its
  sky; `None` is black. `diffuse_environment` turns and scales its diffuse
  light, `backdrop` is its panorama or a colour.
- `reflection_environment`: turns and scales its specular light where no
  baked probe reflects.

With the `diagnostics` feature, `Settings::diagnostics` holds investigation
switches (layers off, the frame probe, the tone-target capture). It is not
serialized and is never a player setting.
