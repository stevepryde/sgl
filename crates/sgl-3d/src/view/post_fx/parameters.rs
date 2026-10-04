//! The authored effect parameters of `FrameInput` (`crystal`, `taa`): their
//! defaults and their DiligentFX attributes.
use crate::frame_input::{CrystalParameters, TaaParameters};
use sgl_post_fx::temporal_anti_aliasing::FeatureFlags;
use sgl_post_fx::{ScreenSpaceReflectionAttribs, TemporalAntiAliasingAttribs};

/// DiligentFX's `ScreenSpaceReflectionAttribs` defaults, `RoughnessThreshold`
/// 0.2 included as in AMD's SSSR sample, with the traversal budget of
/// Diligent's own renderer, Hydrogent (`HnPostProcessTaskParams`:
/// `MaxTraversalIntersections` 64).
///
/// Each ray follows its lobe's peak, the mirror direction
/// (`GGXImportanceSampleBias` 1, DFX-20), as Godot's SSR traces: one GGX
/// sample per pixel leaves noise on glossy, normal-mapped receivers that the
/// denoiser cannot remove, and its temporal pass holds it as blotches. The
/// spatial reconstruction still widens reflections with roughness. The
/// temporal pass keeps 0.95 of its clamped history, as Wicked Engine's
/// `ssr_temporalCS` (the pass it derives from) does; at DiligentFX's 1.0 the
/// current frame never enters and stale history smears as the camera moves.
impl Default for CrystalParameters {
    fn default() -> Self {
        let ScreenSpaceReflectionAttribs {
            depth_buffer_thickness,
            roughness_threshold,
            most_detailed_mip,
            spatial_reconstruction_radius,
            temporal_variance_stability_factor,
            bilateral_cleanup_spatial_sigma_factor,
            ..
        } = Default::default();
        Self {
            depth_buffer_thickness,
            roughness_threshold,
            most_detailed_mip,
            max_traversal_intersections: 64,
            ggx_importance_sample_bias: 1.,
            spatial_reconstruction_radius,
            temporal_radiance_stability_factor: 0.95,
            temporal_variance_stability_factor,
            bilateral_cleanup_spatial_sigma_factor,
        }
    }
}

/// Crystal's parameters as DiligentFX's attributes, with the material input
/// `begin` writes: perceptual roughness in channel 0, as Hydrogent supplies
/// it (`HnPostProcessTaskParams`). `ScreenSpaceReflection` sets
/// `AlphaInterpolation` itself.
pub(super) fn ssr_attribs(parameters: &CrystalParameters) -> ScreenSpaceReflectionAttribs {
    let CrystalParameters {
        depth_buffer_thickness,
        roughness_threshold,
        most_detailed_mip,
        max_traversal_intersections,
        ggx_importance_sample_bias,
        spatial_reconstruction_radius,
        temporal_radiance_stability_factor,
        temporal_variance_stability_factor,
        bilateral_cleanup_spatial_sigma_factor,
    } = *parameters;
    ScreenSpaceReflectionAttribs {
        depth_buffer_thickness,
        roughness_threshold,
        most_detailed_mip,
        is_roughness_perceptual: 1,
        roughness_channel: 0,
        max_traversal_intersections,
        ggx_importance_sample_bias,
        spatial_reconstruction_radius,
        temporal_radiance_stability_factor,
        temporal_variance_stability_factor,
        bilateral_cleanup_spatial_sigma_factor,
        ..Default::default()
    }
}

/// DiligentFX's `TemporalAntiAliasingAttribs` defaults, the still-pixel
/// history of Bevy's TAA (DFX-19) included, with Hydrogent's feature flags
/// (`HnPostProcessTaskParams::TAAFeatureFlags`): the Catmull-Rom history
/// filter alone.
impl Default for TaaParameters {
    fn default() -> Self {
        let TemporalAntiAliasingAttribs {
            temporal_stability_factor,
            still_history_factor,
            still_motion_pixels,
            motion_vector_diff_factor,
            min_variance_gamma,
            max_variance_gamma,
            depth_disocclusion_threshold,
            variance_intersection_max_t,
            ..
        } = Default::default();
        Self {
            temporal_stability_factor,
            still_history_factor,
            still_motion_pixels,
            motion_vector_diff_factor,
            min_variance_gamma,
            max_variance_gamma,
            depth_disocclusion_threshold,
            variance_intersection_max_t,
            bicubic_filter: true,
            gaussian_weighting: false,
            ycocg_color_space: false,
        }
    }
}

/// `value` in `0..=max`, NaN as 0. A NaN, or an infinite factor times a
/// zero motion or deviation, would reach the history, which keeps it.
fn clamped(value: f32, max: f32) -> f32 {
    if value.is_nan() {
        0.
    } else {
        value.clamp(0., max)
    }
}

fn share(value: f32) -> f32 {
    clamped(value, 1.)
}

fn nonnegative(value: f32) -> f32 {
    clamped(value, f32::MAX)
}

/// TAA's parameters as DiligentFX's attributes, clamped to their ranges, and
/// whether its history restarts.
pub(super) fn taa_attribs(
    parameters: &TaaParameters,
    reset_accumulation: bool,
) -> TemporalAntiAliasingAttribs {
    let TaaParameters {
        temporal_stability_factor,
        still_history_factor,
        still_motion_pixels,
        motion_vector_diff_factor,
        min_variance_gamma,
        max_variance_gamma,
        depth_disocclusion_threshold,
        variance_intersection_max_t,
        bicubic_filter: _,
        gaussian_weighting: _,
        ycocg_color_space: _,
    } = *parameters;
    TemporalAntiAliasingAttribs {
        temporal_stability_factor: share(temporal_stability_factor),
        reset_accumulation: u32::from(reset_accumulation),
        min_variance_gamma: nonnegative(min_variance_gamma),
        max_variance_gamma: nonnegative(max_variance_gamma),
        motion_vector_diff_factor: nonnegative(motion_vector_diff_factor),
        depth_disocclusion_threshold: share(depth_disocclusion_threshold),
        variance_intersection_max_t: nonnegative(variance_intersection_max_t),
        still_motion_pixels: nonnegative(still_motion_pixels),
        still_history_factor: share(still_history_factor),
        ..Default::default()
    }
}

/// TAA's history filters as DiligentFX's feature flags.
pub(super) fn taa_feature_flags(parameters: &TaaParameters) -> FeatureFlags {
    let flag = |on: bool, flag| if on { flag } else { FeatureFlags::NONE };
    flag(
        parameters.gaussian_weighting,
        FeatureFlags::GAUSSIAN_WEIGHTING,
    ) | flag(parameters.bicubic_filter, FeatureFlags::BICUBIC_FILTER)
        | flag(
            parameters.ycocg_color_space,
            FeatureFlags::YCOCG_COLOR_SPACE,
        )
}

/// The layout `taa_attribs` fills, against `sgl-post-fx`'s WGSL.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "sgl_post_fx_taa",
        "TemporalAntiAliasingAttribs",
        TemporalAntiAliasingAttribs,
        [
            temporal_stability_factor as "TemporalStabilityFactor",
            reset_accumulation as "ResetAccumulation",
            skip_rejection as "SkipRejection",
            padding0 as "Padding0",
            min_variance_gamma as "MinVarianceGamma",
            max_variance_gamma as "MaxVarianceGamma",
            motion_vector_diff_factor as "MotionVectorDiffFactor",
            depth_disocclusion_threshold as "DepthDisocclusionThreshold",
            variance_intersection_max_t as "VarianceIntersectionMaxT",
            still_motion_pixels as "StillMotionPixels",
            still_history_factor as "StillHistoryFactor",
            padding1 as "Padding1",
        ]
    )]
}
