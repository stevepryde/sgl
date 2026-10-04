//! The settings, the frame and the device resolved into the effective
//! configuration (`view::effective`): once per frame, and the size-affecting
//! choices at `Renderer::resize`.
use crate::FrameInput;
use crate::settings::{
    AmbientOcclusionQuality, Antialiasing, FogQuality, ReflectionMethod, RenderPreset,
    ScreenSpaceReflections, Settings,
};
use crate::view::effective::{AmbientOcclusion, Effective, ScreenSpace, Sizing};
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
/// positive, finite length and detail spread, and a `perspective` camera
/// (`perspective`), whose clip w is view depth, which the froxels' slices
/// and reprojection take. Without a medium there is nothing to fog, and the
/// frame is as it would be with the fog run.
fn fog(
    settings: &Settings,
    input: &FrameInput,
    fog_volumes: bool,
    perspective: bool,
) -> Option<FogQuality> {
    let fog = input.fog;
    let medium = fog.density > 0. || fog_volumes;
    let volume = [fog.length, fog.detail_spread]
        .iter()
        .all(|value| value.is_finite() && *value > 0.);
    (atmosphere(settings, input) && medium && volume && perspective).then_some(settings.fog_quality)
}

/// Motion blur's shutter: the frame's authored one scaled by the setting,
/// while that is positive and finite. It also needs `perspective`'s
/// projection, whose depth it linearises.
fn motion_blur(settings: &Settings, input: &FrameInput) -> Option<f32> {
    let shutter = input.motion_blur.shutter_angle * settings.motion_blur.shutter_scale();
    (shutter.is_finite() && shutter > 0.).then_some(shutter)
}

/// The effective configuration of a frame. `fog_volumes` is whether the
/// scene holds fog volumes, `fsr2_running` whether FSR2's context runs on
/// this device and `fused_supported` whether the device has the fused pass's
/// attachments.
pub(super) fn resolve(
    settings: &Settings,
    input: &FrameInput,
    fog_volumes: bool,
    fsr2_running: bool,
    fused_supported: bool,
) -> Effective {
    let low = settings.low();
    let diagnostics = settings.diagnostics_in_effect();
    let disable = diagnostics.disable;
    let ambient_occlusion_radius = input.ambient_occlusion_radius;
    let p = input.camera.projection.to_cols_array_2d();
    // XeGTAO's depth unpack/reconstruction is for centered reversed-Z
    // perspective projections, not orthographic cameras.
    let ambient_occlusion = (settings.ambient_occlusion != AmbientOcclusionQuality::Off
        && ambient_occlusion_radius.is_finite()
        && ambient_occlusion_radius > 0.
        && p[3][3] == 0.
        && p[2][3] == -1.
        && p[2][0] == 0.
        && p[2][1] == 0.
        && p[2][2] >= 0.
        && p[3][2] > 0.)
        .then_some(AmbientOcclusion {
            quality: settings.ambient_occlusion,
            radius: ambient_occlusion_radius,
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
    .map(|half_resolution| ScreenSpace {
        method: settings.reflection_method,
        half_resolution,
    });
    // DiligentFX's SSR shares TAA's post-effect context.
    let crystal = screen_space.is_some_and(|ssr| ssr.method == ReflectionMethod::Crystal);
    Effective {
        antialiasing,
        taa,
        fsr2,
        post_fx: taa || crystal,
        ambient_occlusion,
        screen_space,
        world_space: settings.world_space_reflections && screen_space.is_some(),
        fused: fused_supported && !disable.fused_opaque,
        local_lights: !disable.local_lights,
        atmosphere: atmosphere(settings, input),
        fog: fog(
            settings,
            input,
            fog_volumes,
            p[3][3] == 0. && p[2][3] == -1.,
        ),
        bloom: settings.bloom.enabled(low) && !disable.bloom,
        motion_blur: motion_blur(settings, input).filter(|_| post_fx_camera),
        heat: settings.heat_distortion,
        effects: !disable.effects,
        culling: !disable.culling,
        smaa: !disable.smaa,
        layers: LayerConstants::new(&disable),
        source_environment: !disable.source_environment,
        #[cfg(feature = "diagnostics")]
        frame_probe: diagnostics.frame_probe,
        capture_tone_target: diagnostics.frame_probe || diagnostics.capture_tone_target,
    }
}
