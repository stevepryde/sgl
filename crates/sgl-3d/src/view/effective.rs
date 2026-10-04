//! The effective configuration: what runs, as the renderer resolves it from
//! the settings, the frame and the device (`renderer::effective`). Stages
//! read only this and the frame's authored values.
use super::pipelines::LayerConstants;
use crate::settings::{
    AmbientOcclusionQuality, Antialiasing, FogQuality, Fsr2Quality, ReflectionMethod,
};

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
}

/// Ambient occlusion that runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AmbientOcclusion {
    pub quality: AmbientOcclusionQuality,
    /// XeGTAO's search radius in world metres, positive and finite.
    pub radius: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Effective {
    /// The antialiasing in effect: the settings' choice for the preset, TAA
    /// where FSR2 is chosen but not running.
    pub antialiasing: Antialiasing,
    pub taa: bool,
    pub fsr2: bool,
    /// DiligentFX's post-effect context runs: for TAA, and for Crystal SSR.
    pub post_fx: bool,
    pub ambient_occlusion: Option<AmbientOcclusion>,
    pub screen_space: Option<ScreenSpace>,
    /// World-space rays fill the screen-space method's misses (only with a
    /// method).
    pub world_space: bool,
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
    /// The geometry pipelines' constants (diagnostics layers).
    pub layers: LayerConstants,
    /// Source completion adds environment and probe specular (a
    /// diagnostics layer, compiled into its pipelines).
    pub source_environment: bool,
    /// The numerical frame probe observes this frame.
    #[cfg(feature = "diagnostics")]
    pub frame_probe: bool,
    /// Tone mapping writes a target before presentation, for diagnostics.
    pub capture_tone_target: bool,
}
