//! The effective configuration: what runs, as the renderer resolves it from
//! the settings, the frame and the device (`renderer::effective`). Stages
//! read only this and the frame's authored values.
use super::pipelines::LayerConstants;
use crate::settings::{
    AmbientOcclusionQuality, Antialiasing, FogQuality, FsrQuality, FsrVersion,
    RayTracedShadowQuality, ReflectionMethod, ShadowQuality, SmaaQuality, WorldSpaceReflections,
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

/// The size-affecting choices `View::resize` resolves: what the stages'
/// targets are made for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sizing {
    /// FSR's quality, while FSR is the antialiasing chosen for the preset.
    pub fsr: Option<(FsrVersion, FsrQuality)>,
    /// Bloom's targets start full size (the High preset), else 1×1; bloom
    /// sizes them each frame for whether it runs.
    pub bloom_targets: bool,
}

/// The hardware path as the settings and the device resolve it (the
/// architecture's Hardware ray tracing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HardwareRayTracing {
    /// Requested, but the scene permanently omitted ray-query data.
    DisabledByScene,
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

/// What the frame draws onto the composite (Frame item 8), each of which
/// marks the temporal masks while the frame has them: the one condition
/// each draw and the masks' allocation read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CompositeDraws {
    /// The scene holds blended or transmissive materials, which the camera's
    /// blended list draws where it holds them.
    pub blended: bool,
    /// The scene holds an opaque or masked material whose shading moves
    /// (`Materials::holds_moving_surfaces`), which the temporal composition
    /// marks in the frames it changes (`Scene::shading_changes`).
    pub moving_surfaces: bool,
    /// Glow is drawn: the scene holds some and effects draw (a diagnostics
    /// layer).
    pub glow: bool,
    /// Mist is drawn: the scene holds some, in the frame's atmosphere.
    pub mist: bool,
    /// Heat distortion is drawn: the scene holds some and
    /// `Settings::heat_distortion` is on.
    pub heat: bool,
}

impl CompositeDraws {
    /// Whether anything draws that marks the masks.
    pub fn any(self) -> bool {
        self.blended || self.moving_surfaces || self.glow || self.mist || self.heat
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Effective {
    /// The antialiasing in effect: the settings' choice for the preset, TAA
    /// where FSR is chosen but not running.
    pub antialiasing: Antialiasing,
    pub taa: bool,
    pub fsr: bool,
    /// The temporal masks are drawn and read: while FSR runs, and while TAA
    /// runs and the frame can mark them (Frame item 8).
    pub temporal_masks: bool,
    /// FSR's RCAS sharpness in 0..=1, while it sharpens.
    pub fsr_sharpness: Option<f32>,
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
    /// How the ray-traced shadow stage denoises, while it runs: the
    /// setting resolved for the preset, Low or High.
    pub ray_traced_shadow_quality: RayTracedShadowQuality,
    /// The receiver pass runs: the scene holds a blended receiver of
    /// screen-space reflections, and a screen-space method, TAA, FSR or
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
    /// The volumetric fog runs, at this quality, and every draw fogs from
    /// its volume: the frame's atmosphere, with a medium (a density or fog
    /// volumes) the volume can hold, seen by a `perspective` camera.
    pub fog: Option<FogQuality>,
    /// The fog filters its froxels before it integrates them, while it runs.
    pub fog_filter: bool,
    /// The most rays a probe of the dynamic GI volume traces, while it
    /// runs: the scene holds a volume and `Settings::dynamic_gi` is not Off.
    pub dynamic_gi: Option<u32>,
    /// The transparent stage draws the volume layers where the camera's
    /// blended list holds a material whose shader reads its volume path:
    /// `Settings::volume_paths` on a device of the Extended binding tier.
    pub volume_paths: bool,
    pub bloom: bool,
    /// The share of each pixel's motion that motion blur spreads it over:
    /// the authored shutter scaled by the setting, while positive.
    pub motion_blur: Option<f32>,
    /// What draws onto the composite and marks the temporal masks.
    pub composite: CompositeDraws,
    /// The camera culls against its view (a diagnostics layer).
    pub culling: bool,
    /// SMAA may run (a diagnostics layer).
    pub smaa: bool,
    /// SMAA's preset, where it runs.
    pub smaa_quality: SmaaQuality,
    pub tone_mapping: crate::settings::ToneMapping,
    pub upscaling: crate::settings::Upscaling,
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
