//! The settings, the frame and the device resolved into the effective
//! configuration (`view::effective`): once per frame, and the size-affecting
//! choices at `Renderer::resize`.
use crate::FrameInput;
use crate::scene::RayData;
use crate::settings::{
    AmbientOcclusionQuality, Antialiasing, DynamicGiQuality, FogQuality, FsrVersion,
    ReflectionMethod, RenderPreset, SceneResolution, ScreenSpaceReflections, Settings,
    ShadowQuality, WorldSpaceReflections,
};
use crate::shading::RayQueryForm;
use crate::shading::bind::BindingTier;
use crate::stages::reflections::velvet;
use crate::view::effective::{
    AmbientOcclusion, Effective, HardwareRayTracing, ScreenSpace, ShadowFilter, Sizing,
};
use crate::view::pipelines::LayerConstants;

/// `effective` for a frame whose rays trace in hardware where `hardware`
/// and whose slots may hold `lights`: the ray-traced shadow stage runs
/// where the setting asks for it, the rays trace in hardware and a slot
/// holds a light, and the opaque stage then takes its two-pass form, the
/// stage between its parts.
pub(super) fn traced_shadows(
    effective: Effective,
    hardware: bool,
    lights: &crate::stages::shadows::traced::slots::SlotLights,
) -> Effective {
    let ray_traced_shadows = effective.ray_traced_shadows && hardware && !lights.is_empty();
    Effective {
        ray_traced_shadows,
        fused: effective.fused && !ray_traced_shadows,
        ..effective
    }
}

/// The size-affecting choices of `settings`.
pub(super) fn sizing(settings: &Settings) -> Sizing {
    Sizing {
        fsr: settings
            .antialiasing
            .resolve(settings.low())
            .fsr()
            .map(|version| (version, settings.fsr_quality)),
        bloom_targets: settings.preset == RenderPreset::High,
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

/// The volumetric fog's quality while it runs: with the frame's atmosphere,
/// a medium (a positive density, or the scene's `fog_volumes`), a volume of
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
    let medium = fog.density > 0. || fog_volumes;
    let volume = fog.length.is_finite() && fog.length > 0.;
    (atmosphere(settings, input) && medium && volume && perspective).then_some(settings.fog_quality)
}

/// Motion blur's shutter: the frame's authored one scaled by the setting,
/// while that is positive and finite. It also needs `perspective`'s
/// projection, whose depth it linearises.
fn motion_blur(settings: &Settings, input: &FrameInput) -> Option<f32> {
    let shutter = input.motion_blur.shutter_angle * settings.motion_blur.shutter_scale();
    (shutter.is_finite() && shutter > 0.).then_some(shutter)
}

/// FSR's sharpness while `fsr` runs and the setting sharpens: within
/// AMD's 0..=1, NaN as the least.
fn fsr_sharpness(settings: &Settings, fsr: bool) -> Option<f32> {
    let sharpness = settings.fsr_sharpness;
    (fsr && settings.fsr_sharpening).then(|| {
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
}

impl SceneContent {
    pub fn of(scene: &crate::Scene) -> Self {
        Self {
            ray_data: scene.ray_data(),
            fog_volumes: !scene.transient.fog_volume_corners.is_empty(),
            receivers: scene.materials.holds_receivers(),
            dynamic_gi_volume: scene.dynamic_gi.is_some(),
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
    // XeGTAO's depth unpack/reconstruction is for centered reversed-Z
    // perspective projections, not orthographic cameras.
    let ambient_occlusion = (settings.ambient_occlusion != AmbientOcclusionQuality::Off
        && p[3][3] == 0.
        && p[2][3] == -1.
        && p[2][0] == 0.
        && p[2][1] == 0.
        && p[2][2] >= 0.
        && p[3][2] > 0.)
        .then_some(AmbientOcclusion {
            quality: settings.ambient_occlusion,
            radius: input.ambient_occlusion_radius,
        });
    // DiligentFX's and FSR's camera inputs are for `perspective`'s
    // infinite reversed-Z projection; with other cameras SSR is off and
    // SMAA replaces TAA, and FSR's render-size frame is presented as it
    // is.
    let post_fx_camera = p[3][3] == 0. && p[2][3] == -1. && p[2][2] == 0. && p[3][2] > 0.;
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
    // spiral while temporal antialiasing resolves it, else its Gaussian.
    let shadow_filter = match settings.shadow_quality {
        ShadowQuality::Low => ShadowFilter::Hardware,
        ShadowQuality::High if taa || fsr => ShadowFilter::Temporal,
        ShadowQuality::High => ShadowFilter::Gaussian,
    };
    let motion_blur = motion_blur(settings, input).filter(|_| post_fx_camera);
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
    // frame narrows it to whether the stage runs (`traced_shadows`).
    let ray_traced_shadows =
        settings.ray_traced_shadows && matches!(hardware_ray_tracing, HardwareRayTracing::On(_));
    let occlusion_culling = settings.occlusion_culling && occlusion_supported && !disable.culling;
    Effective {
        antialiasing,
        taa,
        fsr,
        fsr_sharpness: fsr_sharpness(settings, fsr),
        post_fx: taa || crystal,
        shadow_quality: settings.shadow_quality,
        shadow_filter,
        ambient_occlusion,
        screen_space,
        world_space: world_space(settings, content.ray_data, screen_space.is_some()),
        hardware_ray_tracing,
        ray_traced_shadows,
        ray_traced_shadow_quality: settings.ray_traced_shadow_quality.resolve(low),
        receivers: content.receivers
            && (screen_space.is_some() || taa || fsr || motion_blur.is_some()),
        // Occlusion culling's late phase falls between the G-buffer passes,
        // so the opaque stage takes its two-pass form.
        fused: fused_supported && !disable.fused_opaque && !occlusion_culling,
        occlusion_culling,
        local_lights: !disable.local_lights,
        atmosphere: atmosphere(settings, input),
        fog: fog(
            settings,
            input,
            content.fog_volumes,
            p[3][3] == 0. && p[2][3] == -1.,
        ),
        fog_filter: settings.fog_filter,
        dynamic_gi: dynamic_gi(settings, tier, content.ray_data)
            .rays()
            .filter(|_| content.dynamic_gi_volume),
        volume_paths: volume_paths(settings, tier),
        bloom: settings.bloom.enabled(low) && !disable.bloom,
        motion_blur,
        heat: settings.heat_distortion,
        effects: !disable.effects,
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
        use crate::{Renderer, Scene, SceneOptions, test_support};
        let Some((device, queue)) = test_support::device() else {
            return;
        };
        let settings = Settings {
            hardware_ray_tracing: true,
            dynamic_gi: DynamicGiQuality::Off,
            ..Settings::default()
        };
        let mut renderer = Renderer::for_test(&device, &queue, [16; 2], &settings);
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
            );
            let before = crate::counters::snapshot();
            renderer.prepare.run(
                &device,
                &queue,
                &mut scene,
                &input,
                history,
                &effective,
                None,
                [16; 2],
                &mut renderer.views,
                &renderer.bindings.frame,
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
