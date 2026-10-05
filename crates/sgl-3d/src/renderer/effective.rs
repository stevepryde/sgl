//! The settings, the frame and the device resolved into the effective
//! configuration (`view::effective`): once per frame, and the size-affecting
//! choices at `Renderer::resize`.
use crate::FrameInput;
use crate::settings::{
    AmbientOcclusionQuality, Antialiasing, FogQuality, ReflectionMethod, RenderPreset,
    ScreenSpaceReflections, Settings, ShadowQuality,
};
use crate::stages::reflections::velvet;
use crate::view::effective::{AmbientOcclusion, Effective, ScreenSpace, ShadowFilter, Sizing};
use crate::view::pipelines::LayerConstants;

/// The size-affecting choices of `settings`.
pub(super) fn sizing(settings: &Settings) -> Sizing {
    Sizing {
        fsr2: (settings.antialiasing.resolve(settings.low()) == Antialiasing::Fsr2)
            .then_some(settings.fsr2_quality),
        bloom_targets: settings.preset == RenderPreset::High,
    }
}

/// The antialiasing that runs for `settings`: their choice resolved for the
/// preset, with TAA where FSR2 is chosen but not `fsr2_running`. The saved
/// choice is unchanged.
pub(super) fn antialiasing(settings: &Settings, fsr2_running: bool) -> Antialiasing {
    match settings.antialiasing.resolve(settings.low()) {
        Antialiasing::Fsr2 if !fsr2_running => Antialiasing::Taa,
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

/// FSR2's sharpness while `fsr2` runs and the setting sharpens: within
/// AMD's 0..=1, NaN as the least.
fn fsr2_sharpness(settings: &Settings, fsr2: bool) -> Option<f32> {
    let sharpness = settings.fsr2_sharpness;
    (fsr2 && settings.fsr2_sharpening).then(|| {
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
    /// FSR2's context runs on it.
    pub fsr2_running: bool,
    /// It has the fused pass's attachments.
    pub fused_supported: bool,
    /// It traces rays in hardware (`scene::rays::acceleration::supported`).
    pub ray_queries: bool,
}

/// What a frame's scene holds that its effective configuration follows.
#[derive(Clone, Copy, Default)]
pub(super) struct SceneContent {
    pub fog_volumes: bool,
    /// A blended receiver of screen-space reflections.
    pub receivers: bool,
    /// A dynamic GI volume.
    pub dynamic_gi_volume: bool,
}

impl SceneContent {
    pub fn of(scene: &crate::Scene) -> Self {
        Self {
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
        fsr2_running,
        fused_supported,
        ray_queries,
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
    // DiligentFX's and FSR2's camera inputs are for `perspective`'s
    // infinite reversed-Z projection; with other cameras SSR is off and
    // SMAA replaces TAA, and FSR2's render-size frame is presented as it
    // is.
    let post_fx_camera = p[3][3] == 0. && p[2][3] == -1. && p[2][2] == 0. && p[3][2] > 0.;
    let antialiasing = antialiasing(settings, fsr2_running);
    let taa = antialiasing == Antialiasing::Taa && !disable.taa && post_fx_camera;
    let fsr2 = antialiasing == Antialiasing::Fsr2 && !disable.fsr2 && post_fx_camera;
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
        ShadowQuality::High if taa || fsr2 => ShadowFilter::Temporal,
        ShadowQuality::High => ShadowFilter::Gaussian,
    };
    let motion_blur = motion_blur(settings, input).filter(|_| post_fx_camera);
    Effective {
        antialiasing,
        taa,
        fsr2,
        fsr2_sharpness: fsr2_sharpness(settings, fsr2),
        post_fx: taa || crystal,
        shadow_quality: settings.shadow_quality,
        shadow_filter,
        ambient_occlusion,
        screen_space,
        world_space: settings.world_space_reflections && screen_space.is_some(),
        hardware_ray_tracing: ray_queries && settings.hardware_ray_tracing,
        receivers: content.receivers
            && (screen_space.is_some() || taa || fsr2 || motion_blur.is_some()),
        fused: fused_supported && !disable.fused_opaque,
        local_lights: !disable.local_lights,
        atmosphere: atmosphere(settings, input),
        fog: fog(
            settings,
            input,
            content.fog_volumes,
            p[3][3] == 0. && p[2][3] == -1.,
        ),
        fog_filter: settings.fog_filter,
        dynamic_gi: settings
            .dynamic_gi
            .rays()
            .filter(|_| content.dynamic_gi_volume),
        bloom: settings.bloom.enabled(low) && !disable.bloom,
        motion_blur,
        heat: settings.heat_distortion,
        effects: !disable.effects,
        culling: !disable.culling,
        smaa: !disable.smaa,
        smaa_quality: settings.smaa_quality,
        anisotropy: settings.anisotropic_filtering.clamp(),
        layers: LayerConstants::new(&disable),
        source_environment: !disable.source_environment,
        #[cfg(feature = "diagnostics")]
        frame_probe: diagnostics.frame_probe,
        #[cfg(feature = "diagnostics")]
        instance_visibility: diagnostics.instance_visibility,
        #[cfg(feature = "diagnostics")]
        dynamic_gi_observation: diagnostics.dynamic_gi,
        capture_tone_target: diagnostics.frame_probe || diagnostics.capture_tone_target,
    }
}
