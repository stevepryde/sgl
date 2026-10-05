//! The game's rendering settings: one [`Settings`] value the game chooses,
//! stores and passes to the renderer, offering players as many of them as it
//! wants. Preset choices leave explicit overrides intact.
use serde::{Deserialize, Serialize};

/// The quality tier `Preset` choices resolve against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderPreset {
    #[default]
    High,
    Low,
}

/// Preset is TAA on High and SMAA on Low. TAA is DiligentFX's temporal
/// anti-aliasing: the frame renders with sub-pixel jitter and resolves into a
/// history before bloom and tone mapping. FSR2 is AMD's FidelityFX Super
/// Resolution 2, which does the same while upscaling from the
/// [`Fsr2Quality`] render size; TAA replaces it where the device cannot run
/// it ([`Renderer::antialiasing_in_effect`](crate::Renderer::antialiasing_in_effect)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Antialiasing {
    #[default]
    Preset,
    Taa,
    Smaa,
    Off,
    Fsr2,
}
impl Antialiasing {
    /// The chosen method: Taa, Smaa, Off or Fsr2.
    pub fn resolve(self, low: bool) -> Self {
        match self {
            Self::Preset if low => Self::Smaa,
            Self::Preset => Self::Taa,
            other => other,
        }
    }
}

/// FSR2's render size relative to the scene (its output): Native AA renders
/// every scene pixel, the others are the SDK's quality modes
/// (`FfxFsr2QualityMode`), 1.5×, 1.7×, 2× and 3× per dimension. The default is
/// AMD's FSR sample's (`m_ScalePreset`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fsr2Quality {
    NativeAa,
    #[default]
    Quality,
    Balanced,
    Performance,
    UltraPerformance,
}

/// SMAA's quality presets (SMAA 2.8's `SMAA_PRESET_*`): Low and Medium
/// search up to 4 and 8 steps of two pixels each way along an edge, at a
/// colour contrast threshold of 0.15 and 0.1; High up to 16 and adds
/// diagonal and corner detection; Ultra up to 32 at a threshold of 0.05.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmaaQuality {
    Low,
    #[default]
    Medium,
    High,
    Ultra,
}

/// The most anisotropic filtering material textures sample with, as
/// Godot's `anisotropic_filtering_level`: Off filters trilinearly, the
/// others take up to 2, 4, 8 or 16 samples along a surface seen at a
/// grazing angle, sharper there at more texture bandwidth.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnisotropicFiltering {
    Off,
    X2,
    X4,
    #[default]
    X8,
    X16,
}
impl AnisotropicFiltering {
    /// The sampler's `anisotropy_clamp`.
    pub(crate) fn clamp(self) -> u16 {
        match self {
            Self::Off => 1,
            Self::X2 => 2,
            Self::X4 => 4,
            Self::X8 => 8,
            Self::X16 => 16,
        }
    }
}

/// Screen-space reflections, traced by the `ReflectionMethod` and composited
/// over each receiver's probe and sky specular. Off reflects environment and
/// probe specular only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenSpaceReflections {
    #[default]
    Off,
    /// Rays traced at half resolution.
    Half,
    Full,
}

/// How reflections are traced and filtered. Each method returns the same
/// premultiplied radiance and confidence for composition. The package README
/// lists the sources each combines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReflectionMethod {
    /// Hierarchical tracing along each lobe's peak, reconstructed by a
    /// spatial, temporal and bilateral denoiser, below perceptual roughness
    /// 0.2.
    #[default]
    Crystal,
    /// Hierarchical mirror rays, accumulated over frames, then blurred through
    /// a mip chain by a cone that widens with roughness and ray length, below
    /// perceptual roughness 0.7.
    Velvet,
}

/// What world-space rays reach where the screen-space method misses, at
/// half resolution up to 1000 m from the reflecting surface. Off reflects
/// what the method and the probes and sky give.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorldSpaceReflections {
    #[default]
    Off,
    /// Moving objects, which baked probes cannot hold: a static surface in
    /// front of one leaves the probes and sky in charge.
    Moving,
    /// Everything, static surfaces included, which then reflect as they
    /// stand rather than as their probe recorded them, as Wicked Engine's
    /// ray-traced reflections trace the whole scene. Meant for hardware ray
    /// tracing; without it the software BVH walk costs more than `Moving`'s.
    All,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Bloom {
    #[default]
    Preset,
    Off,
    On,
}
impl Bloom {
    pub fn enabled(self, low: bool) -> bool {
        match self {
            Self::Preset => !low,
            Self::Off => false,
            Self::On => true,
        }
    }
}

/// Motion blur of the frame along each pixel's motion, the camera's and
/// moving instances' alike, a comfort choice: Full blurs over the shutter
/// the frame authors (`FrameInput::motion_blur`), Reduced over half of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MotionBlur {
    #[default]
    Off,
    Reduced,
    Full,
}
impl MotionBlur {
    /// The share of the authored shutter that blurs.
    pub(crate) fn shutter_scale(self) -> f32 {
        match self {
            Self::Off => 0.,
            Self::Reduced => 0.5,
            Self::Full => 1.,
        }
    }
}

/// Current-frame diffuse visibility. The consumer persists this explicit choice.
/// All enabled tiers use full scene resolution and upstream XeGTAO sample counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AmbientOcclusionQuality {
    #[default]
    Off,
    Low,
    Medium,
    High,
    Ultra,
}

/// The resolution of the volumetric fog's froxel volume: Godot's
/// `volumetric_fog/volume_size` across the frame's mean side, with its
/// default `volume_depth` of 64 slices. Higher resolves sharper light shafts
/// and shadow edges in the fog at a higher cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FogQuality {
    /// Godot's default: 64 froxels across, about 260,000 at 16:9.
    Low,
    /// 128 across, about a million at 16:9, as many as Unreal's and
    /// Frostbite's froxel grids hold at 1080p.
    #[default]
    High,
}

impl FogQuality {
    /// Froxels across the frame's mean side, and depth slices.
    pub(crate) fn volume(self) -> (u32, u32) {
        match self {
            Self::Low => (64, 64),
            Self::High => (128, 64),
        }
    }
}

/// Dynamic diffuse GI from the scene's volume of probes
/// (`Scene::set_dynamic_gi_volume`), Wicked Engine's DDGI: the most rays
/// each probe traces a frame. A probe whose light has settled traces a few
/// whatever the tier; one whose light changes traces up to the most, and so
/// settles faster and with less noise at High. Off traces none, and
/// surfaces take their indirect diffuse light as without a volume. Without
/// a volume nothing runs at any tier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DynamicGiQuality {
    Off,
    /// Up to 128 rays a probe.
    Low,
    /// Up to 256 rays a probe, Wicked's default.
    #[default]
    High,
}

impl DynamicGiQuality {
    /// The most rays a probe traces a frame, while it runs.
    pub(crate) fn rays(self) -> Option<u32> {
        let most = crate::shading::dynamic_gi::MOST_RAYS;
        match self {
            Self::Off => None,
            Self::Low => Some(most / 2),
            Self::High => Some(most),
        }
    }
}

/// The shadows' map sizes and the filter the camera's surfaces take them
/// with, as Godot's desktop and mobile project settings set them
/// (b130438: `directional_shadow/size`, `positional_shadow/atlas_size` and
/// both `soft_shadow_filter_quality` settings, and their `.mobile`
/// overrides). Probe captures and world-space ray hits keep Castaño's
/// kernel, and the fog its one tap, at either.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShadowQuality {
    /// Godot's mobile defaults: 1024-texel cascades (its 2048 directional
    /// atlas holds four), a 2048-texel local-light atlas, and one hardware
    /// 2×2 comparison tap (its `Hard` filter).
    Low,
    /// Godot's desktop defaults: 2048-texel cascades (its 4096 atlas holds
    /// four; Bevy's `DirectionalLightShadowMap` default), a 4096-texel
    /// local-light atlas, and Bevy's soft filters: Jimenez's spiral, turned
    /// each frame, while TAA or FSR2 resolves it, else Castaño's kernel.
    #[default]
    High,
}

impl ShadowQuality {
    /// Each directional shadow cascade's size in texels: a power of two, as
    /// the cascade fit's texel snapping needs.
    pub(crate) fn cascade_size(self) -> u32 {
        match self {
            Self::Low => 1024,
            Self::High => 2048,
        }
    }

    /// The local-light shadow atlas's width and height in texels.
    pub(crate) fn atlas_size(self) -> u32 {
        match self {
            Self::Low => 2048,
            Self::High => 4096,
        }
    }
}

/// Presentation cadence. Display follows the surface's refresh-paced FIFO;
/// explicit limits cap rendering without changing the simulation tick rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameRate {
    #[default]
    Display,
    Fps60,
    Fps90,
    Fps120,
    Fps144,
    Fps240,
}
impl FrameRate {
    pub fn limit(self) -> Option<u32> {
        match self {
            Self::Display => None,
            Self::Fps60 => Some(60),
            Self::Fps90 => Some(90),
            Self::Fps120 => Some(120),
            Self::Fps144 => Some(144),
            Self::Fps240 => Some(240),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneResolution {
    /// At most 1.75 (High) or 1 (Low) scene pixels per logical pixel.
    #[default]
    Preset,
    /// Fit within 1280×720 without upscaling.
    Hd,
    /// Fit within 1920×1080 without upscaling.
    FullHd,
    Full,
    ThreeQuarter,
    Half,
}

/// Every rendering setting the renderer resolves into its effective
/// configuration each frame. Size-affecting choices (preset, scene
/// resolution, FSR2 and its quality) take effect at `Renderer::resize`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub preset: RenderPreset,
    pub scene_resolution: SceneResolution,
    pub antialiasing: Antialiasing,
    pub fsr2_quality: Fsr2Quality,
    /// FSR2's robust contrast-adaptive sharpening (RCAS) of its output.
    pub fsr2_sharpening: bool,
    /// How much FSR2 sharpens while `fsr2_sharpening` is on, 0..=1 as AMD's
    /// `sharpness`: 0 is the least, 1 the most. Others are clamped, NaN to 0.
    pub fsr2_sharpness: f32,
    pub smaa_quality: SmaaQuality,
    pub anisotropic_filtering: AnisotropicFiltering,
    pub bloom: Bloom,
    /// The shadow maps' sizes and the camera's shadow filter.
    pub shadow_quality: ShadowQuality,
    /// Diffuse sky and fill visibility, and the specular occlusion of
    /// environment and probe reflections (not screen-space hits).
    pub ambient_occlusion: AmbientOcclusionQuality,
    pub screen_space_reflections: ScreenSpaceReflections,
    pub reflection_method: ReflectionMethod,
    /// Traces what screen-space reflections miss through the scene's ray
    /// buffers; only effective with screen-space reflections.
    pub world_space_reflections: WorldSpaceReflections,
    /// Hardware ray tracing, off by default and in no preset: a game opts
    /// in by turning it on and requesting the device's feature
    /// (`graphics_device::ray_tracing_features`). Where the device has it, on
    /// frames that trace rays (world-space reflections, the dynamic GI
    /// volume), the scene builds acceleration structures over its geometry
    /// and keeps them (`Renderer::ray_tracing_stats`), and the rays trace
    /// them; off frees them.
    pub hardware_ray_tracing: bool,
    /// The volumetric fog and mist, while the frame turns its atmosphere on
    /// (`FrameInput::atmosphere`, off by default); this allows them, and is
    /// on by default.
    pub atmosphere: bool,
    /// The volumetric fog's resolution.
    pub fog_quality: FogQuality,
    /// The scene's dynamic diffuse GI volume's quality, or Off.
    pub dynamic_gi: DynamicGiQuality,
    /// Godot's Gaussian filter across each slice of the volumetric fog's
    /// froxels, x then y, before it integrates them (Godot's
    /// `volumetric_fog/use_filter`): smoother fog, softer shafts and shadow
    /// edges in it, at the cost of two passes over the froxels.
    pub fog_filter: bool,
    /// Camera-path heat shimmer.
    pub heat_distortion: bool,
    pub motion_blur: MotionBlur,
    /// Layer isolation and frame observations for debug and test tooling;
    /// never saved.
    #[cfg(feature = "diagnostics")]
    #[serde(skip)]
    pub diagnostics: Diagnostics,
}

impl Default for Settings {
    /// High, with atmosphere allowed and the fog filter on, heat
    /// distortion, world-space reflections, hardware ray tracing and motion
    /// blur off, FSR2 sharpening on at AMD's FSR sample's 0.8
    /// (`m_RCASSharpen`, `m_Sharpness`), and every other choice at its
    /// default.
    fn default() -> Self {
        Self {
            preset: RenderPreset::High,
            scene_resolution: SceneResolution::default(),
            antialiasing: Antialiasing::default(),
            fsr2_quality: Fsr2Quality::default(),
            fsr2_sharpening: true,
            fsr2_sharpness: 0.8,
            smaa_quality: SmaaQuality::default(),
            anisotropic_filtering: AnisotropicFiltering::default(),
            bloom: Bloom::default(),
            shadow_quality: ShadowQuality::default(),
            ambient_occlusion: AmbientOcclusionQuality::default(),
            screen_space_reflections: ScreenSpaceReflections::default(),
            reflection_method: ReflectionMethod::default(),
            world_space_reflections: WorldSpaceReflections::Off,
            hardware_ray_tracing: false,
            atmosphere: true,
            fog_quality: FogQuality::default(),
            dynamic_gi: DynamicGiQuality::default(),
            fog_filter: true,
            heat_distortion: false,
            motion_blur: MotionBlur::Off,
            #[cfg(feature = "diagnostics")]
            diagnostics: Diagnostics::default(),
        }
    }
}

impl Settings {
    pub(crate) fn low(&self) -> bool {
        self.preset == RenderPreset::Low
    }

    /// The diagnostics configuration in effect: every layer on and no
    /// observation without the `diagnostics` feature.
    pub(crate) fn diagnostics_in_effect(&self) -> Diagnostics {
        #[cfg(feature = "diagnostics")]
        {
            self.diagnostics
        }
        #[cfg(not(feature = "diagnostics"))]
        {
            Diagnostics::default()
        }
    }
}

#[cfg(not(feature = "diagnostics"))]
pub(crate) use diagnostics::{Diagnostics, DisabledLayers};
#[cfg(feature = "diagnostics")]
pub use diagnostics::{Diagnostics, DisabledLayers};

mod diagnostics {
    /// Debug and test tooling's renderer configuration (feature
    /// `diagnostics`): layers switched off to isolate the others, and frame
    /// observations. The default switches nothing off and observes nothing.
    /// A game reads the observations through `Renderer::diagnostic_target`
    /// and `Renderer::take_frame_probe_reports`.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Diagnostics {
        pub disable: DisabledLayers,
        /// The numerical frame probe counts black, negative and non-finite
        /// pixels at the lit scene (opaque colour before ambient occlusion,
        /// which reflection source completion applies), the reflection
        /// composite, the completed scene and the tone-mapped scene of every
        /// frame, with the primary raster's coverage. It captures the
        /// tone-mapped target too.
        pub frame_probe: bool,
        /// Tone mapping writes the tone-mapped target
        /// (`DiagnosticTarget::ToneMapped`), which is then presented, instead
        /// of tone mapping straight to the output. The output is identical.
        pub capture_tone_target: bool,
        /// Each frame that runs the dynamic GI volume counts its probes,
        /// its rays and the BVH walks they and their visibility rays make,
        /// read back without blocking: `Renderer::take_dynamic_gi_reports`.
        /// The observed frame's trace writes each ray's costs, which a pass
        /// sums, so observe separately from timing it.
        pub dynamic_gi: bool,
    }

    /// Layers switched off, each named after what it removes. The first five
    /// are pipeline constants: changing them rebuilds the geometry or source
    /// completion pipelines on the next frame. The rest apply per frame.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct DisabledLayers {
        /// Material normal maps of raster geometry.
        pub normal_maps: bool,
        /// Material bump maps of raster geometry.
        pub bump_maps: bool,
        /// Baked lighting of raster geometry: lightmaps, irradiance
        /// atlases, instances' baked irradiance and the irradiance volume.
        pub baked_lighting: bool,
        /// Material emission of moving instances in raster geometry.
        pub instance_emission: bool,
        /// Environment and probe specular added by source completion and
        /// screen-space composition.
        pub source_environment: bool,
        /// The scene's point, spot and rectangle lights, as if it had none.
        pub local_lights: bool,
        /// TAA: SMAA stands in for it.
        pub taa: bool,
        /// FSR2: its render-size frame is presented as it is.
        pub fsr2: bool,
        /// The volumetric fog and mist.
        pub atmosphere: bool,
        pub bloom: bool,
        pub smaa: bool,
        /// The fused G-buffer and lighting pass: the two run as separate
        /// passes, as on devices without its attachments.
        pub fused_opaque: bool,
        /// The camera's view culling: every LOD-selected draw is submitted,
        /// its GPU cull accepting every candidate and section.
        pub culling: bool,
        /// Additive effects (glow).
        pub effects: bool,
    }
}
