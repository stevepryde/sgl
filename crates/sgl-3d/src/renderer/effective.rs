//! The settings, the frame and the device resolved into the effective
//! configuration (`view::effective`): once per frame, and the size-affecting
//! choices at `View::resize`.
use crate::FrameInput;
use crate::scene::RayData;
use crate::settings::{
    AmbientOcclusionQuality, Antialiasing, ContactShadows, DynamicGiQuality, FogQuality,
    FsrVersion, ReflectionMethod, RenderPreset, SceneResolution, ScreenSpaceIndirect,
    ScreenSpaceReflections, Settings, ShadowQuality, Sharpening, Upscaling, WorldSpaceReflections,
};
use crate::shading::RayQueryForm;
use crate::shading::bind::BindingTier;
use crate::stages::reflections::velvet;
use crate::view::effective::CompositeDraws;
use crate::view::effective::{
    AmbientOcclusion, Effective, HardwareRayTracing, ScreenIndirect, ScreenSpace, ShadowFilter,
    Sizing,
};
use crate::view::pipelines::LayerConstants;

/// `effective` for a frame whose rays trace in hardware where `hardware`
/// and whose shadowed lights are `lights`: the ray-traced shadow stage runs
/// where the setting asks for it, the rays trace in hardware and a slot
/// holds a light; the contact shadow stage runs where the setting asks for
/// it, a directional light holds the frame's cascades and the ray-traced
/// shadow stage does not run, whose rays resolve contact already. The
/// opaque stage takes its two-pass form while either runs, the stage
/// between its parts.
pub(super) fn shadow_stages(
    effective: Effective,
    hardware: bool,
    lights: &crate::stages::shadows::traced::slots::SlotLights,
) -> Effective {
    let ray_traced_shadows = effective.ray_traced_shadows && hardware && !lights.is_empty();
    let contact_shadows = effective
        .contact_shadows
        .filter(|_| lights.directional.is_some() && !ray_traced_shadows);
    Effective {
        ray_traced_shadows,
        contact_shadows,
        fused: effective.fused && !ray_traced_shadows && contact_shadows.is_none(),
        ..effective
    }
}

fn bloom(settings: &Settings) -> bool {
    settings.bloom.enabled(settings.low()) && !settings.diagnostics_in_effect().disable.bloom
}

/// The size-affecting choices of `settings`.
pub(super) fn sizing(settings: &Settings) -> Sizing {
    Sizing {
        fsr: settings
            .antialiasing
            .resolve(settings.low())
            .fsr()
            .map(|version| (version, settings.fsr_quality)),
        bloom_targets: bloom(settings),
    }
}

/// The scene size for an output of `size` at `device_scale` physical
/// pixels per logical pixel, under the preset and scene resolution.
pub(super) fn scene_size(
    size: [u32; 2],
    preset: RenderPreset,
    resolution: SceneResolution,
    device_scale: f32,
) -> [u32; 2] {
    let scale = match resolution {
        SceneResolution::Preset => {
            let cap = if preset == RenderPreset::Low {
                1.
            } else {
                1.75
            };
            (cap / device_scale.max(1.)).min(1.)
        }
        SceneResolution::Hd => (1280. / size[0].max(1) as f32)
            .min(720. / size[1].max(1) as f32)
            .min(1.),
        SceneResolution::FullHd => (1920. / size[0].max(1) as f32)
            .min(1080. / size[1].max(1) as f32)
            .min(1.),
        SceneResolution::Full => 1.,
        SceneResolution::ThreeQuarter => 0.75,
        SceneResolution::Half => 0.5,
    };
    size.map(|x| ((x as f32 * scale).floor() as u32).max(1))
}

/// The antialiasing that runs for `settings`: their choice resolved for the
/// preset, with TAA where FSR is chosen but not `fsr_running`. The saved
/// choice is unchanged.
pub(super) fn antialiasing(settings: &Settings, fsr_running: Option<FsrVersion>) -> Antialiasing {
    match settings.antialiasing.resolve(settings.low()) {
        choice if choice.fsr().is_some() && choice.fsr() != fsr_running => Antialiasing::Taa,
        other => other,
    }
}

/// The volumetric fog and mist run: the setting, the frame's atmosphere and
/// the diagnostics layer.
pub(super) fn atmosphere(settings: &Settings, input: &FrameInput) -> bool {
    settings.atmosphere && input.atmosphere && !settings.diagnostics_in_effect().disable.atmosphere
}

/// How far from the eye the froxel volume reaches: `length` of view depth
/// along the frame's farthest corner ray, past which `Fog::start` leaves the
/// frame's medium out of every froxel.
fn fog_reach(length: f32, projection: glam::Mat4) -> f32 {
    let [scale_x, scale_y, offset_x, offset_y] = crate::shading::fog::froxel_projection(projection);
    let x = (1. + offset_x.abs()) / scale_x;
    let y = (1. + offset_y.abs()) / scale_y;
    length * (1. + x * x + y * y).sqrt()
}

/// The volumetric fog's quality while it runs: with the frame's atmosphere,
/// a medium (a positive density starting within the volume's reach, or the
/// scene's `fog_volumes`), a volume of
/// positive, finite length, and a `perspective` camera (`perspective`),
/// whose clip w is view depth, which the froxels' slices and reprojection
/// take. Without a medium there is nothing to fog, and the frame is as it
/// would be with the fog run.
fn fog(
    settings: &Settings,
    input: &FrameInput,
    fog_volumes: bool,
    perspective: bool,
) -> Option<FogQuality> {
    let fog = input.fog;
    let reach = fog_reach(fog.length, input.camera.projection);
    let medium = (fog.density > 0. && fog.effective_start() < reach) || fog_volumes;
    let volume = fog.length.is_finite() && fog.length > 0.;
    (atmosphere(settings, input) && medium && volume && perspective).then_some(settings.fog_quality)
}

/// Whether the air's aerial perspective is in effect: under a procedural
/// sky whose `aerial_perspective` is above 0, seen by a `perspective` camera,
/// the fog's own requirement. It is independent of `FrameInput::atmosphere`
/// and `Settings::atmosphere`, which are the fog's and the mist's.
fn aerial_perspective(input: &FrameInput, perspective: bool) -> bool {
    let crate::FrameEnvironment::Sky(sky) = &input.environment else {
        return false;
    };
    perspective && sky.resolved().aerial_perspective > 0.
}

/// Motion blur's shutter: the frame's authored one scaled by the setting,
/// while that is positive and finite. It also needs `perspective`'s
/// projection, whose depth it linearises.
fn motion_blur(settings: &Settings, input: &FrameInput) -> Option<f32> {
    let shutter = input.motion_blur.shutter_angle * settings.motion_blur.shutter_scale();
    (shutter.is_finite() && shutter > 0.).then_some(shutter)
}

/// Post's sharpness while the setting sharpens a frame of `antialiasing`,
/// the antialiasing in effect: within AMD's 0..=1, NaN as the least.
/// `Preset` sharpens at High, and at Low only where FSR is the
/// antialiasing in effect or `Settings::upscaling` is on.
fn sharpness(settings: &Settings, antialiasing: Antialiasing) -> Option<f32> {
    let sharpening = match settings.sharpening {
        Sharpening::Off => false,
        Sharpening::On => true,
        Sharpening::Preset => {
            !settings.low() || antialiasing.fsr().is_some() || settings.upscaling == Upscaling::On
        }
    };
    let sharpness = settings.sharpness;
    sharpening.then(|| {
        if sharpness.is_nan() {
            0.
        } else {
            sharpness.clamp(0., 1.)
        }
    })
}

/// The effective configuration of a first frame from a `perspective` camera
/// with `FrameInput::new`'s values and a scene with no fog volumes or
/// receivers: what `Renderer::new` builds stages for, so that such a frame
/// finds their pipelines built. `device` is as for `resolve`.
pub(super) fn first_frame(settings: &Settings, device: Device) -> Effective {
    let camera = crate::Camera {
        view: glam::Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: glam::Vec3::ZERO,
    };
    resolve(
        settings,
        &FrameInput::new(camera),
        SceneContent::default(),
        device,
    )
}

/// What the renderer's device runs that the effective configuration
/// follows.
#[derive(Clone, Copy)]
pub(super) struct Device {
    /// FSR's context runs on it.
    pub fsr_running: Option<FsrVersion>,
    /// It has the fused pass's attachments.
    pub fused_supported: bool,
    /// It binds the depth pyramid's storage textures.
    pub occlusion_supported: bool,
    /// It traces rays in hardware, in this form
    /// (`scene::rays::acceleration::supported`, `DeviceRayForm::form`).
    pub ray_queries: Option<RayQueryForm>,
    /// Its binding tier, `Extended` alone binding the dynamic GI volume's
    /// probes.
    pub tier: BindingTier,
}

/// `Settings::dynamic_gi` on a device of `tier`: the setting on the
/// Extended binding tier, else `Off`. The saved choice is unchanged.
pub(super) fn dynamic_gi(
    settings: &Settings,
    tier: BindingTier,
    ray_data: RayData,
) -> DynamicGiQuality {
    if ray_data == RayData::Disabled {
        return DynamicGiQuality::Off;
    }
    match tier {
        BindingTier::Extended => settings.dynamic_gi,
        BindingTier::Basic => DynamicGiQuality::Off,
    }
}

/// `Settings::volume_paths` on a device of `tier`: the setting on the
/// Extended binding tier, which alone binds the volume layers, else off.
/// The saved choice is unchanged.
pub(super) fn volume_paths(settings: &Settings, tier: BindingTier) -> bool {
    settings.volume_paths && tier == BindingTier::Extended
}

/// What a frame's scene holds that its effective configuration follows.
#[derive(Clone, Copy, Default)]
pub(super) struct SceneContent {
    pub ray_data: RayData,
    pub fog_volumes: bool,
    /// A blended receiver of screen-space reflections.
    pub receivers: bool,
    /// A dynamic GI volume.
    pub dynamic_gi_volume: bool,
    /// Blended or transmissive materials.
    pub blended: bool,
    /// An opaque or masked material whose shading may change where its
    /// geometry stands still (`Materials::holds_moving_surfaces`).
    pub moving_surfaces: bool,
    /// Glow, mist and heat distortion to draw.
    pub glow: bool,
    pub mist: bool,
    pub heat: bool,
}

impl SceneContent {
    pub fn of(scene: &crate::Scene) -> Self {
        Self {
            ray_data: scene.ray_data(),
            fog_volumes: !scene.transient.fog_volume_corners.is_empty(),
            receivers: scene.materials.holds_receivers(),
            dynamic_gi_volume: scene.dynamic_gi.is_some(),
            blended: scene.materials.holds_blended() || scene.materials.holds_transmissive(),
            moving_surfaces: scene.materials.holds_moving_surfaces(),
            glow: scene.transient.glow_count > 0,
            mist: !scene.transient.mist_positions.is_empty(),
            heat: scene.transient.heat_count > 0,
        }
    }
}

/// The perceptual roughness at which `method` stops tracing and the width
/// of the fade below it: Crystal's DiligentFX `RoughnessThreshold`, fading
/// over the last 0.05 as Bevy's SSR fades out; Velvet's Godot cutoff and
/// forward-pass fade.
fn trace_cutoff(method: ReflectionMethod) -> (f32, f32) {
    match method {
        ReflectionMethod::Crystal => (
            crate::view::post_fx::ssr_attribs().roughness_threshold,
            0.05,
        ),
        ReflectionMethod::Velvet => (velvet::ROUGHNESS_CUTOFF, velvet::ROUGHNESS_FADE),
    }
}

/// The effective configuration of a frame of a scene holding `content` on
/// a device that runs what `device` says.
pub(super) fn resolve(
    settings: &Settings,
    input: &FrameInput,
    content: SceneContent,
    device: Device,
) -> Effective {
    let Device {
        fsr_running,
        fused_supported,
        occlusion_supported,
        ray_queries,
        tier,
    } = device;
    let low = settings.low();
    let diagnostics = settings.diagnostics_in_effect();
    let disable = diagnostics.disable;
    let p = input.camera.projection.to_cols_array_2d();
    // A perspective projection: clip w is view depth (`perspective`'s, or
    // any other whose w row is (0, 0, -1, 0)), as the fog's slices, aerial
    // perspective and contact shadows' thickness take it.
    let perspective = p[3][3] == 0. && p[2][3] == -1.;
    // XeGTAO's depth unpack/reconstruction is for centered reversed-Z
    // perspective projections, not orthographic cameras.
    let ambient_occlusion = (settings.ambient_occlusion != AmbientOcclusionQuality::Off
        && perspective
        && p[2][0] == 0.
        && p[2][1] == 0.
        && p[2][2] >= 0.
        && p[3][2] > 0.)
        .then_some(AmbientOcclusion {
            quality: settings.ambient_occlusion,
            radius: input.ambient_occlusion_radius,
        });
    // Screen-space indirect light reconstructs its rays and reprojects as
    // XeGTAO does, under the same cameras.
    let screen_indirect = (settings.screen_space_indirect != ScreenSpaceIndirect::Off
        && perspective
        && p[2][0] == 0.
        && p[2][1] == 0.
        && p[2][2] >= 0.
        && p[3][2] > 0.)
        .then(|| {
            let radius =
                crate::stages::opaque::screen_indirect::radius(input.screen_space_indirect_radius);
            ScreenIndirect {
                level: settings.screen_space_indirect,
                radius,
                pyramid_radius: ambient_occlusion.map_or(radius, |occlusion| occlusion.radius),
            }
        });
    // DiligentFX's and FSR's camera inputs are for `perspective`'s
    // infinite reversed-Z projection; with other cameras SSR is off and
    // SMAA replaces TAA, and FSR's render-size frame is presented as it
    // is.
    let post_fx_camera = perspective && p[2][2] == 0. && p[3][2] > 0.;
    let antialiasing = antialiasing(settings, fsr_running);
    let taa = antialiasing == Antialiasing::Taa && !disable.taa && post_fx_camera;
    let fsr = antialiasing.fsr().is_some() && !disable.fsr && post_fx_camera;
    let screen_space = match settings.screen_space_reflections {
        _ if !post_fx_camera => None,
        ScreenSpaceReflections::Off => None,
        ScreenSpaceReflections::Half => Some(true),
        ScreenSpaceReflections::Full => Some(false),
    }
    .map(|half_resolution| {
        let (cutoff, fade) = trace_cutoff(settings.reflection_method);
        ScreenSpace {
            method: settings.reflection_method,
            half_resolution,
            cutoff,
            fade,
        }
    });
    // DiligentFX's SSR shares TAA's post-effect context.
    let crystal = screen_space.is_some_and(|ssr| ssr.method == ReflectionMethod::Crystal);
    // The camera's shadow filter: Godot's hard tap at Low; at High, Bevy's
    // spiral while temporal antialiasing resolves it, widened to each
    // light's penumbra with soft shadows, else its Gaussian.
    let shadow_filter = match settings.shadow_quality {
        ShadowQuality::Low => ShadowFilter::Hardware,
        ShadowQuality::High if (taa || fsr) && settings.soft_shadows.enabled(low) => {
            ShadowFilter::Soft
        }
        ShadowQuality::High if taa || fsr => ShadowFilter::Temporal,
        ShadowQuality::High => ShadowFilter::Gaussian,
    };
    let motion_blur = motion_blur(settings, input).filter(|_| post_fx_camera);
    // What draws onto the composite, each of which marks the temporal
    // masks while the frame has them (Frame item 9). TAA reads the masks
    // only in a frame one of them can mark; FSR's dispatch binds them in
    // every frame.
    let composite = CompositeDraws {
        blended: content.blended,
        moving_surfaces: content.moving_surfaces,
        glow: content.glow && !disable.effects,
        mist: content.mist && atmosphere(settings, input),
        heat: content.heat && settings.heat_distortion,
    };
    let temporal_masks = fsr || taa && composite.any();
    let hardware_ray_tracing = match (settings.hardware_ray_tracing, content.ray_data, ray_queries)
    {
        (false, _, _) => HardwareRayTracing::Off,
        (true, RayData::Disabled, _) => HardwareRayTracing::DisabledByScene,
        (true, _, None) => HardwareRayTracing::Unsupported,
        (true, _, Some(form)) => HardwareRayTracing::On(form),
    };
    // Ray-traced shadows trace through the hardware path alone (the
    // architecture's Ray-traced shadows); elsewhere the maps shadow, their
    // path without hardware ray tracing (D-30). The
    // frame narrows it to whether the stage runs (`shadow_stages`).
    let ray_traced_shadows =
        settings.ray_traced_shadows && matches!(hardware_ray_tracing, HardwareRayTracing::On(_));
    let occlusion_culling = settings.occlusion_culling && occlusion_supported && !disable.culling;
    // The thickness test takes a share of perspective depth; the frame
    // narrows it to whether the stage runs (`shadow_stages`).
    let contact_shadows =
        Some(settings.contact_shadows).filter(|&level| level != ContactShadows::Off && perspective);
    Effective {
        antialiasing,
        taa,
        fsr,
        temporal_masks,
        sharpness: sharpness(settings, antialiasing),
        post_fx: taa || crystal,
        shadow_quality: settings.shadow_quality,
        shadow_filter,
        ambient_occlusion,
        screen_indirect,
        screen_space,
        world_space: world_space(settings, content.ray_data, screen_space.is_some()),
        hardware_ray_tracing,
        ray_traced_shadows,
        ray_traced_shadow_quality: settings.ray_traced_shadow_quality.resolve(low),
        contact_shadows,
        receivers: content.receivers
            && (screen_space.is_some() || taa || fsr || motion_blur.is_some()),
        // Occlusion culling's late phase falls between the G-buffer passes,
        // so the opaque stage takes its two-pass form.
        fused: fused_supported && !disable.fused_opaque && !occlusion_culling,
        occlusion_culling,
        local_lights: !disable.local_lights,
        fog: fog(settings, input, content.fog_volumes, perspective),
        fog_filter: settings.fog_filter,
        aerial_perspective: aerial_perspective(input, perspective),
        dynamic_gi: dynamic_gi(settings, tier, content.ray_data)
            .rays()
            .filter(|_| content.dynamic_gi_volume),
        volume_paths: volume_paths(settings, tier),
        bloom: bloom(settings),
        motion_blur,
        composite,
        culling: !disable.culling,
        smaa: !disable.smaa,
        smaa_quality: settings.smaa_quality,
        tone_mapping: settings.tone_mapping,
        upscaling: settings.upscaling,
        anisotropy: settings.anisotropic_filtering.clamp(),
        layers: LayerConstants::new(&disable),
        source_environment: !disable.source_environment,
        #[cfg(feature = "diagnostics")]
        frame_probe: diagnostics.frame_probe,
        #[cfg(feature = "diagnostics")]
        dynamic_gi_observation: diagnostics.dynamic_gi,
        capture_tone_target: diagnostics.frame_probe || diagnostics.capture_tone_target,
    }
}

/// World-space reflections need both screen-space receivers and retained
/// ray data; the requested choice itself is never rewritten.
pub(super) fn world_space(
    settings: &Settings,
    ray_data: RayData,
    screen_space: bool,
) -> WorldSpaceReflections {
    if ray_data == RayData::Enabled && screen_space {
        settings.world_space_reflections
    } else {
        WorldSpaceReflections::Off
    }
}

#[cfg(test)]
mod ray_data_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Scene policy must gate the actual effective stage configuration, even
    // on a hypothetical device with every ray feature. SSR, volume layers,
    // ambient and shadow maps remain independently available.
    #[wasm_bindgen_test(unsupported = test)]
    fn disabled_scene_gates_rays_without_changing_requested_settings() {
        let settings = Settings {
            screen_space_reflections: ScreenSpaceReflections::Full,
            world_space_reflections: WorldSpaceReflections::Moving,
            hardware_ray_tracing: true,
            ray_traced_shadows: true,
            dynamic_gi: DynamicGiQuality::High,
            volume_paths: true,
            ..Settings::default()
        };
        let requested = settings;
        let camera = crate::Camera {
            view: glam::Mat4::IDENTITY,
            projection: crate::perspective(1., 1., 0.1),
            eye: glam::Vec3::ZERO,
        };
        let input = FrameInput::new(camera);
        let device = Device {
            fsr_running: None,
            fused_supported: true,
            occlusion_supported: true,
            ray_queries: Some(RayQueryForm::Baseline),
            tier: BindingTier::Extended,
        };
        let content = SceneContent {
            dynamic_gi_volume: true,
            ..SceneContent::default()
        };
        let enabled = resolve(&settings, &input, content, device);
        assert!(enabled.dynamic_gi.is_some());
        assert_eq!(enabled.world_space, WorldSpaceReflections::Moving);
        assert!(enabled.ray_traced_shadows);
        let disabled = resolve(
            &settings,
            &input,
            SceneContent {
                ray_data: RayData::Disabled,
                ..content
            },
            device,
        );
        assert!(disabled.dynamic_gi.is_none());
        assert_eq!(disabled.world_space, WorldSpaceReflections::Off);
        assert_eq!(
            disabled.hardware_ray_tracing,
            HardwareRayTracing::DisabledByScene
        );
        assert!(!disabled.ray_traced_shadows);
        assert_eq!(enabled.screen_space, disabled.screen_space);
        assert!(disabled.volume_paths);
        assert_eq!(enabled.shadow_quality, disabled.shadow_quality);
        assert_eq!(enabled.ambient_occlusion, disabled.ambient_occlusion);
        assert_eq!(settings, requested);
    }

    // A preceding scene's error is not a capability of this one. Synthetic
    // hardware support reaches the no-consumer On branch on Metal/WebGPU too;
    // that branch must not attempt a hardware build or retain NO_RAY_DATA.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn enabled_scene_without_ray_consumers_clears_previous_disabled_reason() {
        use crate::view::history::{CameraFrame, CameraHistory};
        use crate::{Scene, SceneOptions, TestRenderer, test_support};
        let Some((device, queue)) = test_support::device() else {
            return;
        };
        let settings = Settings {
            hardware_ray_tracing: true,
            dynamic_gi: DynamicGiQuality::Off,
            ..Settings::default()
        };
        let mut renderer = TestRenderer::for_test(&device, &queue, [16; 2], &settings);
        let input = FrameInput::new(crate::Camera {
            view: glam::Mat4::IDENTITY,
            projection: crate::perspective(1., 1., 0.1),
            eye: glam::Vec3::ZERO,
        });
        let synthetic = Device {
            fsr_running: None,
            fused_supported: true,
            occlusion_supported: true,
            ray_queries: Some(RayQueryForm::Baseline),
            tier: renderer.binding_tier(),
        };
        for mode in [RayData::Disabled, RayData::Enabled] {
            let mut scene = Scene::with_options(&device, &queue, SceneOptions { ray_data: mode });
            let effective = resolve(&settings, &input, SceneContent::of(&scene), synthetic);
            let history = CameraHistory::default().begin(
                CameraFrame {
                    view: input.camera.view,
                    projection: input.camera.projection,
                    jitter: [0.; 2],
                },
                true,
                glam::DVec3::ZERO,
                0.,
                input.visibility_mask,
            );
            let before = crate::counters::snapshot();
            let traced = crate::stages::prepare::Prepare::traces(&scene, &effective);
            renderer
                .shared
                .prepare
                .run_scene(&device, &queue, &mut scene, &effective, &input, traced);
            crate::stages::prepare::Prepare::run_view(
                &device,
                &queue,
                &scene,
                &input,
                history,
                &effective,
                None,
                [16; 2],
                &mut renderer.view.views,
                &renderer.view.bindings.frame,
            );
            assert_eq!(
                renderer.ray_tracing_error().is_some(),
                mode == RayData::Disabled
            );
            if mode == RayData::Enabled {
                assert_eq!(
                    effective.hardware_ray_tracing,
                    HardwareRayTracing::On(RayQueryForm::Baseline)
                );
                let delta = crate::counters::snapshot().since(&before);
                assert_eq!(delta.blas_builds + delta.tlas_builds, 0);
            }
        }
    }
}

#[cfg(all(test, feature = "diagnostics"))]
mod temporal_mask_tests {
    use super::*;
    use crate::settings::{Antialiasing, FsrVersion};
    use wasm_bindgen_test::wasm_bindgen_test;

    // Plausible defects: TAA frames allocate the temporal masks though
    // nothing draws into them, or not when one writer alone draws; a writer
    // counted although its own draw is off (glow with effects disabled, mist
    // without the frame's atmosphere, heat with the setting off); FSR frames
    // left without the masks their dispatch binds. The oracle is Frame item
    // 8's rule as a table: the masks are held for every FSR frame, and for a
    // TAA frame exactly when blended or moving-shading materials, or glow,
    // mist or heat that draws, can mark them.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_temporal_masks_are_held_exactly_while_a_writer_can_mark_them() {
        let camera = crate::Camera {
            view: glam::Mat4::IDENTITY,
            projection: crate::perspective(1., 1., 0.1),
            eye: glam::Vec3::ZERO,
        };
        let mut input = FrameInput::new(camera);
        input.atmosphere = true;
        let device = Device {
            fsr_running: Some(FsrVersion::Fsr2),
            fused_supported: true,
            occlusion_supported: true,
            ray_queries: None,
            tier: BindingTier::Extended,
        };
        let taa = Settings {
            antialiasing: Antialiasing::Taa,
            atmosphere: true,
            heat_distortion: true,
            ..Settings::default()
        };
        let none = SceneContent::default();
        let alone = [
            SceneContent {
                blended: true,
                ..none
            },
            SceneContent {
                moving_surfaces: true,
                ..none
            },
            SceneContent { glow: true, ..none },
            SceneContent { mist: true, ..none },
            SceneContent { heat: true, ..none },
        ];
        let held = |settings: &Settings, content| {
            resolve(settings, &input, content, device).temporal_masks
        };
        assert!(!held(&taa, none), "a TAA frame nothing can mark");
        for content in alone {
            assert!(held(&taa, content), "one writer alone");
        }
        let mut no_effects = taa;
        no_effects.diagnostics.disable.effects = true;
        assert!(!held(&no_effects, alone[2]), "glow with effects disabled");
        let no_atmosphere = Settings {
            atmosphere: false,
            ..taa
        };
        assert!(!held(&no_atmosphere, alone[3]), "mist without atmosphere");
        let no_heat = Settings {
            heat_distortion: false,
            ..taa
        };
        assert!(!held(&no_heat, alone[4]), "heat with the setting off");
        let fsr = Settings {
            antialiasing: Antialiasing::Fsr2,
            ..taa
        };
        assert!(held(&fsr, none), "an FSR frame, whose dispatch binds them");
        let off = Settings {
            antialiasing: Antialiasing::Off,
            ..taa
        };
        for content in alone {
            assert!(!held(&off, content), "no temporal antialiasing");
        }
    }
}

#[cfg(test)]
mod sharpening_tests {
    use super::*;
    use crate::settings::{Antialiasing as A, Sharpening as S};
    use wasm_bindgen_test::wasm_bindgen_test;

    // Defects: `Preset` sharpens Low's unreconstructed frames (SMAA, TAA,
    // no antialiasing), or not its FSR or spatially upscaled ones; it
    // follows the FSR the settings request rather than the antialiasing in
    // effect on a device without FSR; Off or On is overridden by the
    // preset; the sharpness leaves AMD's 0..=1. The oracle is the settings
    // guide's Sharpening row as a table.
    #[wasm_bindgen_test(unsupported = test)]
    fn sharpening_follows_the_settings_guide() {
        let camera = crate::Camera {
            view: glam::Mat4::IDENTITY,
            projection: crate::perspective(1., 1., 0.1),
            eye: glam::Vec3::ZERO,
        };
        let input = FrameInput::new(camera);
        let device = |fsr_running| Device {
            fsr_running,
            fused_supported: true,
            occlusion_supported: true,
            ray_queries: None,
            tier: BindingTier::Extended,
        };
        let fsr2 = Some(FsrVersion::Fsr2);
        let fsr3 = Some(FsrVersion::Fsr3);
        let (high, low) = (RenderPreset::High, RenderPreset::Low);
        let (off, on) = (Upscaling::Off, Upscaling::On);
        // Preset, requested antialiasing, the FSR the device runs,
        // upscaling, sharpening, and whether the frame is sharpened.
        let cases = [
            (high, A::Preset, None, off, S::Preset, true),
            (high, A::Smaa, None, off, S::Preset, true),
            (high, A::Off, None, off, S::Preset, true),
            (high, A::Fsr2, fsr2, off, S::Preset, true),
            (high, A::Fsr2, None, off, S::Preset, true),
            (low, A::Preset, None, off, S::Preset, false),
            (low, A::Taa, None, off, S::Preset, false),
            (low, A::Off, None, off, S::Preset, false),
            (low, A::Smaa, None, on, S::Preset, true),
            (low, A::Taa, None, on, S::Preset, true),
            (low, A::Fsr2, fsr2, off, S::Preset, true),
            (low, A::Fsr3, fsr3, off, S::Preset, true),
            // FSR requested on a device without it: TAA is in effect.
            (low, A::Fsr2, None, off, S::Preset, false),
            (low, A::Fsr3, fsr2, off, S::Preset, false),
            (low, A::Fsr2, None, on, S::Preset, true),
            (high, A::Taa, None, off, S::Off, false),
            (high, A::Fsr2, fsr2, on, S::Off, false),
            (low, A::Fsr2, fsr2, off, S::Off, false),
            (low, A::Smaa, None, off, S::On, true),
            (low, A::Off, None, off, S::On, true),
            (high, A::Taa, None, off, S::On, true),
        ];
        for (preset, antialiasing, fsr_running, upscaling, sharpening, sharpened) in cases {
            let settings = Settings {
                preset,
                antialiasing,
                upscaling,
                sharpening,
                ..Settings::default()
            };
            let sharpness = resolve(
                &settings,
                &input,
                SceneContent::default(),
                device(fsr_running),
            )
            .sharpness;
            assert_eq!(
                sharpness,
                sharpened.then_some(0.8),
                "{preset:?}, {antialiasing:?} on a device running {fsr_running:?}, \
                 upscaling {upscaling:?}, sharpening {sharpening:?}"
            );
        }
        for (sharpness, effective) in [(0., 0.), (1., 1.), (1.5, 1.), (-0.5, 0.), (f32::NAN, 0.)] {
            let settings = Settings {
                sharpening: Sharpening::On,
                sharpness,
                ..Settings::default()
            };
            let resolved = resolve(&settings, &input, SceneContent::default(), device(None));
            assert_eq!(resolved.sharpness, Some(effective), "sharpness {sharpness}");
        }
    }
}

#[cfg(test)]
mod contact_shadow_tests {
    use super::*;
    use crate::stages::shadows::traced::slots::{SlotLight, SlotLights};
    use wasm_bindgen_test::wasm_bindgen_test;

    // Defects: the stage runs with the setting off, under an orthographic
    // camera (whose depth the thickness test does not take), without a
    // directional light with the frame's cascades above the opacity
    // cutoff, or beside the ray-traced shadow stage, whose mask the
    // lighting pass binds in its place; a level other than the setting's;
    // or the opaque stage keeps its fused form, which has no pass between
    // its G-buffer and lighting. The oracle is the Contact shadows design's
    // conditions as a table.
    #[wasm_bindgen_test(unsupported = test)]
    fn contact_shadows_run_as_the_design_says() {
        let camera = |projection| crate::Camera {
            view: glam::Mat4::IDENTITY,
            projection,
            eye: glam::Vec3::ZERO,
        };
        let perspective = crate::perspective(1., 1., 0.1);
        let orthographic =
            glam::camera::rh::proj::directx::orthographic(-5., 5., -5., 5., 100., 0.1);
        let device = Device {
            fsr_running: None,
            fused_supported: true,
            occlusion_supported: true,
            ray_queries: Some(RayQueryForm::Baseline),
            tier: BindingTier::Extended,
        };
        let sun = SlotLights {
            directional: Some(0),
            local: Vec::new(),
        };
        let lamp = SlotLights {
            directional: None,
            local: vec![SlotLight {
                id: <crate::LightId as crate::content::identity::Identity>::issue(0, 1),
                baked: false,
            }],
        };
        // The setting, the projection, the frame's shadowed lights,
        // whether ray-traced shadows are asked for and the rays trace in
        // hardware, and the stage's level where it runs.
        let cases = [
            (
                ContactShadows::Medium,
                perspective,
                &sun,
                false,
                Some(ContactShadows::Medium),
            ),
            (
                ContactShadows::Short,
                perspective,
                &sun,
                false,
                Some(ContactShadows::Short),
            ),
            (
                ContactShadows::Long,
                perspective,
                &sun,
                false,
                Some(ContactShadows::Long),
            ),
            (ContactShadows::Off, perspective, &sun, false, None),
            (ContactShadows::Medium, orthographic, &sun, false, None),
            (ContactShadows::Medium, perspective, &lamp, false, None),
            (
                ContactShadows::Medium,
                perspective,
                &SlotLights::default(),
                false,
                None,
            ),
            (ContactShadows::Medium, perspective, &sun, true, None),
        ];
        for (level, projection, lights, traced, expected) in cases {
            let settings = Settings {
                contact_shadows: level,
                hardware_ray_tracing: traced,
                ray_traced_shadows: traced,
                ..Settings::default()
            };
            let input = FrameInput::new(camera(projection));
            let resolved = shadow_stages(
                resolve(&settings, &input, SceneContent::default(), device),
                traced,
                lights,
            );
            let case = format!("{level:?} {lights:?} traced {traced}");
            assert_eq!(resolved.contact_shadows, expected, "{case}");
            assert_eq!(
                resolved.fused,
                expected.is_none() && !resolved.ray_traced_shadows,
                "{case}"
            );
        }
    }
}

#[cfg(test)]
mod shadow_filter_tests {
    use super::*;
    use crate::settings::SoftShadows;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Plausible defects: soft shadows resolved on where the temporal filter
    // does not run (SMAA, no antialiasing, the Low shadow quality), so eight
    // turned taps go unresolved; the Preset value not following the preset,
    // or an explicit value overridden by it; FSR left out of the temporal
    // condition. The oracle is the settings guide's rule: the soft filter
    // where the setting resolves on (Preset: on at High, off at Low) and
    // the temporal filter runs (High shadows with TAA or FSR running).
    #[wasm_bindgen_test(unsupported = test)]
    fn soft_shadows_resolve_where_the_temporal_filter_runs() {
        let input = FrameInput::new(crate::Camera {
            view: glam::Mat4::IDENTITY,
            projection: crate::perspective(1., 1., 0.1),
            eye: glam::Vec3::ZERO,
        });
        use Antialiasing::{Fsr2, Smaa, Taa};
        use ShadowFilter::{Gaussian, Hardware, Soft, Temporal};
        use SoftShadows as S;
        let (low, high) = (RenderPreset::Low, RenderPreset::High);
        let (low_maps, high_maps) = (ShadowQuality::Low, ShadowQuality::High);
        let rows = [
            (high, S::Preset, Taa, high_maps, Soft),
            (low, S::Preset, Taa, high_maps, Temporal),
            (high, S::Off, Taa, high_maps, Temporal),
            (low, S::On, Taa, high_maps, Soft),
            (high, S::On, Fsr2, high_maps, Soft),
            (high, S::On, Smaa, high_maps, Gaussian),
            (high, S::On, Antialiasing::Off, high_maps, Gaussian),
            (high, S::On, Taa, low_maps, Hardware),
            // The Low preset's own antialiasing is SMAA.
            (low, S::On, Antialiasing::Preset, high_maps, Gaussian),
        ];
        for (preset, soft_shadows, antialiasing, shadow_quality, expected) in rows {
            let settings = Settings {
                preset,
                soft_shadows,
                antialiasing,
                shadow_quality,
                ..Settings::default()
            };
            // FSR runs where it is chosen.
            let device = Device {
                fsr_running: antialiasing.fsr(),
                fused_supported: true,
                occlusion_supported: true,
                ray_queries: None,
                tier: BindingTier::Extended,
            };
            let effective = resolve(&settings, &input, SceneContent::default(), device);
            assert_eq!(
                effective.shadow_filter, expected,
                "{preset:?}, {soft_shadows:?}, {antialiasing:?}, {shadow_quality:?} shadows"
            );
        }
    }
}
