//! The effective configuration: what runs, as the renderer resolves it from
//! the settings, the frame and the device (`renderer::effective`). Stages
//! read only this and the frame's authored values.
use super::pipelines::LayerConstants;
use crate::settings::{
    AmbientOcclusionQuality, Antialiasing, FogQuality, Fsr2Quality, ReflectionMethod,
    ShadowQuality, SmaaQuality,
};

/// The filter the camera's surfaces take their shadows with
/// (`shadow_filter` in shadow_sampling.wgsl).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ShadowFilter {
    /// One hardware 2×2 comparison tap.
    Hardware,
    /// Castaño's fixed kernel.
    #[default]
    Gaussian,
    /// Jimenez's spiral, turned each frame for temporal antialiasing to
    /// resolve.
    Temporal,
}

/// The size-affecting choices `Renderer::resize` resolves: what the stages'
/// targets are made for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sizing {
    /// FSR2's quality, while FSR2 is the antialiasing chosen for the preset.
    pub fsr2: Option<Fsr2Quality>,
    /// Bloom's targets start full size (the High preset), else 1×1; bloom
    /// sizes them each frame for whether it runs.
    pub bloom_targets: bool,
}

/// The screen-space reflection method that runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScreenSpace {
    pub method: ReflectionMethod,
    /// Rays traced at half resolution.
    pub half_resolution: bool,
    /// The perceptual roughness at and above which the method traces no
    /// lobe, and the width of the fade below it over which composition
    /// takes its result in (`specular_trace_fade` in specular_lobes.wgsl).
    pub cutoff: f32,
    pub fade: f32,
}

/// Ambient occlusion that runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AmbientOcclusion {
    pub quality: AmbientOcclusionQuality,
    /// The frame's search radius in world metres, which XeGTAO's pass
    /// clamps to the radii it takes.
    pub radius: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Effective {
    /// The antialiasing in effect: the settings' choice for the preset, TAA
    /// where FSR2 is chosen but not running.
    pub antialiasing: Antialiasing,
    pub taa: bool,
    pub fsr2: bool,
    /// FSR2's RCAS sharpness in 0..=1, while it sharpens.
    pub fsr2_sharpness: Option<f32>,
    /// DiligentFX's post-effect context runs: for TAA, and for Crystal SSR.
    pub post_fx: bool,
    /// The shadow maps' sizes.
    pub shadow_quality: ShadowQuality,
    /// The camera's shadow filter: the quality's, for the antialiasing.
    pub shadow_filter: ShadowFilter,
    pub ambient_occlusion: Option<AmbientOcclusion>,
    pub screen_space: Option<ScreenSpace>,
    /// World-space rays fill the screen-space method's misses (only with a
    /// method).
    pub world_space: bool,
    /// The scene keeps its acceleration structures for hardware ray
    /// tracing, built on the frames that trace rays: the device has ray
    /// queries and `Settings::hardware_ray_tracing` is on.
    pub hardware_ray_tracing: bool,
    /// The receiver pass runs: the scene holds a blended receiver of
    /// screen-space reflections, and a screen-space method, TAA, FSR2 or
    /// motion blur reads the surface it draws.
    pub receivers: bool,
    /// The G-buffer and lighting are one pass.
    pub fused: bool,
    /// The scene's point and spot lights render (a diagnostics layer).
    pub local_lights: bool,
    /// Mist draws (the frame's atmosphere).
    pub atmosphere: bool,
    /// The volumetric fog runs, at this quality, and every draw fogs from
    /// its volume: the frame's atmosphere, with a medium (a density or fog
    /// volumes) the volume can hold, seen by a `perspective` camera.
    pub fog: Option<FogQuality>,
    /// The fog filters its froxels before it integrates them, while it runs.
    pub fog_filter: bool,
    /// The most rays a probe of the dynamic GI volume traces, while it
    /// runs: the scene holds a volume and `Settings::dynamic_gi` is not Off.
    pub dynamic_gi: Option<u32>,
    pub bloom: bool,
    /// The share of each pixel's motion that motion blur spreads it over:
    /// the authored shutter scaled by the setting, while positive.
    pub motion_blur: Option<f32>,
    pub heat: bool,
    /// Additive effects draw (a diagnostics layer).
    pub effects: bool,
    /// The camera culls against its view (a diagnostics layer).
    pub culling: bool,
    /// SMAA may run (a diagnostics layer).
    pub smaa: bool,
    /// SMAA's preset, where it runs.
    pub smaa_quality: SmaaQuality,
    /// Material samplers' `anisotropy_clamp`.
    pub anisotropy: u16,
    /// The geometry pipelines' constants (diagnostics layers).
    pub layers: LayerConstants,
    /// Source completion adds environment and probe specular (a
    /// diagnostics layer, compiled into its pipelines).
    pub source_environment: bool,
    /// The numerical frame probe observes this frame.
    #[cfg(feature = "diagnostics")]
    pub frame_probe: bool,
    /// The camera's instance visibility: observed, or the hidden instances
    /// skipped.
    #[cfg(feature = "diagnostics")]
    pub instance_visibility: crate::settings::InstanceVisibility,
    /// The dynamic GI stage observes this frame, while it runs.
    #[cfg(feature = "diagnostics")]
    pub dynamic_gi_observation: bool,
    /// Tone mapping writes a target before presentation, for diagnostics.
    pub capture_tone_target: bool,
}
