//! The effective configuration: what runs, as the renderer resolves it from
//! the settings, the frame and the device (`renderer::effective`). Stages
//! read only this and the frame's authored values.
use super::pipelines::LayerConstants;
use crate::settings::{
    AmbientOcclusionQuality, Antialiasing, FogQuality, Fsr2Quality, ReflectionMethod,
    ShadowQuality, SmaaQuality, WorldSpaceReflections,
};
use crate::shading::RayQueryForm;

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

/// The hardware path as the settings and the device resolve it (the
/// architecture's Hardware ray tracing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HardwareRayTracing {
    /// `Settings::hardware_ray_tracing` is off.
    Off,
    /// It is on, and the device has no ray queries
    /// (`graphics_device::ray_tracing_features`).
    Unsupported,
    /// It is on, and the device traces rays in this form.
    On(RayQueryForm),
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
    /// What world-space rays reach where they fill the screen-space
    /// method's misses: Off without a method.
    pub world_space: WorldSpaceReflections,
    /// The hardware path: while it is on, the scene keeps its acceleration
    /// structures, built on the frames that trace rays, whose rays then
    /// trace through them.
    pub hardware_ray_tracing: HardwareRayTracing,
    /// Ray-traced shadows: the setting is on and hardware ray tracing is
    /// in effect, so the frame traces rays. Once the frame knows whether
    /// its rays trace in hardware and its slots hold a light, it is whether
    /// the ray-traced shadow stage runs (`renderer::effective::traced_shadows`),
    /// and the opaque stage then takes its two-pass form.
    pub ray_traced_shadows: bool,
    /// The receiver pass runs: the scene holds a blended receiver of
    /// screen-space reflections, and a screen-space method, TAA, FSR2 or
    /// motion blur reads the surface it draws.
    pub receivers: bool,
    /// The G-buffer and lighting are one pass: the device has the
    /// attachments, unless occlusion culling or the ray-traced shadow stage
    /// runs between them.
    pub fused: bool,
    /// The camera culls occlusion in two phases, its opaque stage in its
    /// two-pass form: `Settings::occlusion_culling` on a device with the
    /// pyramid's storage textures, while the camera culls against its view.
    pub occlusion_culling: bool,
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
    /// The dynamic GI stage observes this frame, while it runs.
    #[cfg(feature = "diagnostics")]
    pub dynamic_gi_observation: bool,
    /// Tone mapping writes a target before presentation, for diagnostics.
    pub capture_tone_target: bool,
}
