//! What the caller supplies for one frame: the camera, the authored look and
//! per-frame state. Linear colours and metres. No simulation clock is
//! advanced here; rendering settings are `settings::Settings`, and point and
//! spot lights are scene content (`Scene::add_light`).
mod look;

pub use look::{
    AgxLook, AutoExposure, BloomParameters, ColorGrading, ColorGradingGlobal, ColorGradingSection,
    CompensationCurve, CompensationCurveError, Exposure, LocalExposure, MeteringMask,
    MotionBlurParameters,
};

use crate::content::lighting::{
    Backdrop, DirectionalLight, EnvironmentLight, FrameEnvironment, HemisphereLight, Mist,
};
use glam::camera;
use glam::{Mat4, Vec3};

/// Reversed-Z, infinite-far, right-handed perspective. Device depth is
/// `near / distance`: 1 at the near plane, approaching 0 at infinity.
pub fn perspective(fov_y_radians: f32, aspect: f32, near: f32) -> Mat4 {
    camera::rh::proj::directx::perspective_infinite_reverse(fov_y_radians, aspect, near)
}

/// A render camera. Projection uses reversed-Z device depth (1 near, 0 far),
/// normally from [`perspective`], in Y-up world space.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub view: Mat4,
    pub projection: Mat4,
    pub eye: Vec3,
}

/// The frame's participating medium, which the volumetric fog lights with
/// the frame's directional lights and their cascades, the scene's point, spot
/// and rectangle lights with their shadows, and the frame's ambient light.
/// The fields are Godot's volumetric fog (`Environment` and `FogMaterial`):
/// the medium's density falls off above `height` as Godot's fog material's
/// does, and the camera's froxel volume reaches `length` metres. The scene's
/// fog volumes (`Scene::update_fog_volumes`) add medium where they lie.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fog {
    /// Extinction per metre at and below `height`; zero is no medium, and
    /// with no fog volumes either the fog does not run.
    pub density: f32,
    /// Linear RGB single-scattering albedo: the share of extinction that
    /// scatters light rather than absorbing it.
    pub albedo: [f32; 3],
    /// Henyey–Greenstein asymmetry, -0.9..0.9: positive scatters forward,
    /// so light shafts brighten looking toward their light.
    pub anisotropy: f32,
    /// The share of the frame's ambient light (the hemisphere fill and
    /// environment diffuse) the medium and fog volumes scatter, as Godot's
    /// `volumetric_fog_ambient_inject`. By default 0, as Godot's, so only
    /// lights light the medium; up to 1 lets the sky's light glow in the
    /// fog, less where the sky does not reach, such as inside a tunnel.
    pub ambient: f32,
    /// World Y below which the density is whole.
    pub height: f32,
    /// How fast the density halves above `height`, per metre (Godot's
    /// `height_falloff`: density × 2^(-falloff × metres above)); zero is
    /// uniform.
    pub height_falloff: f32,
    /// The view depth in metres the froxel volume covers; anything farther
    /// takes the fog as far as it, the sky by `sky_affect`.
    pub length: f32,
    /// How far from the camera the medium begins, metres along the view
    /// ray: nearer, it is absent (no extinction, no scattered light), and
    /// beyond, it builds as from the camera, so far-only fog leaves the
    /// foreground untouched. Fog volumes are not affected. 0 (the default)
    /// starts it at the camera; NaN, infinite and negative values are 0. The
    /// volume covers `length` of view depth, which is `length` metres away
    /// at the frame's centre and farther toward its edges: a start at
    /// `length` leaves the medium only toward the edges, and one at or
    /// beyond the corners' distance leaves it out of the volume.
    pub start: f32,
    /// How much of its fog the sky takes, 0..=1 (Godot's
    /// `volumetric_fog_sky_affect`): 1 fogs it as if at `length`, 0 leaves
    /// it clear. Fog volumes in front of the sky are part of its fog.
    pub sky_affect: f32,
}

impl Default for Fog {
    /// No medium, with Godot's defaults: albedo white, anisotropy 0.2, no
    /// ambient light, uniform, a 64 m volume starting at the camera and the
    /// whole fog on the sky.
    fn default() -> Self {
        Self {
            density: 0.,
            albedo: [1.; 3],
            anisotropy: 0.2,
            ambient: 0.,
            height: 0.,
            height_falloff: 0.,
            length: 64.,
            start: 0.,
            sky_affect: 1.,
        }
    }
}

impl Fog {
    /// `start` as the fog takes it: NaN, infinite and negative values are 0.
    pub(crate) fn effective_start(&self) -> f32 {
        if self.start.is_finite() && self.start > 0. {
            self.start
        } else {
            0.
        }
    }
}

/// One frame's camera, authored look and per-frame state.
#[derive(Clone, Copy)]
pub struct FrameInput {
    pub camera: Camera,
    /// The camera jumped (a cut, teleport or discontinuous time): history
    /// restarts. A target-changing resize and another scene restart it
    /// without this.
    pub camera_cut: bool,
    /// Presentation seconds, which place materials' scrolling normal layers
    /// and the mist's drift. Double precision, so sessions hours long keep
    /// their animation smooth.
    pub elapsed_seconds: f64,
    /// Milliseconds since the previous frame: FSR's `frameTimeDelta` and
    /// auto exposure's time step.
    pub frame_time_ms: f32,
    /// Enabled material visibility groups: a material draws while every bit
    /// of its `visibility_group` is set here.
    pub visibility_mask: u32,
    /// What surrounds the frame: one of the scene's environment maps,
    /// which lights the frame, draws its sky and backs its reflections, or
    /// a procedural sky lit by `directional_lights`; with `None`, or a map
    /// the scene no longer has, no environment does.
    pub environment: FrameEnvironment,
    /// Up to two directional lights; `None` is no light. Under a
    /// procedural sky (`FrameEnvironment::Sky`) each is its light above the
    /// air, which reaches the scene through it.
    pub directional_lights: [Option<DirectionalLight>; 2],
    pub hemisphere_light: HemisphereLight,
    /// How `environment` lights surfaces' diffuse lobe. Their specular
    /// lobe takes `reflection_environment`.
    pub diffuse_environment: EnvironmentLight,
    pub backdrop: Backdrop,
    /// Baked (fixed) lighting: lightmaps, irradiance atlases, instances'
    /// baked irradiance and the irradiance volume.
    pub baked_lighting: bool,
    /// The volumetric fog and mist, while `Settings::atmosphere` is on too.
    /// Off by default, as Godot's `volumetric_fog_enabled` and
    /// `fog_enabled`: a game turns it on where it wants fog or mist.
    pub atmosphere: bool,
    pub fog: Fog,
    pub mist: Mist,
    /// How `environment` lights surfaces' specular lobe where no baked
    /// probe does: source completion beyond the probes, probe captures' and
    /// ray hits' environment specular, and the sky probe captures record.
    pub reflection_environment: EnvironmentLight,
    pub exposure: Exposure,
    /// Off (1, 1) by default.
    pub local_exposure: LocalExposure,
    pub bloom: BloomParameters,
    /// While `Settings::motion_blur` is on.
    pub motion_blur: MotionBlurParameters,
    pub color_grading: ColorGrading,
    /// XeGTAO's search radius in world metres, clamped to 0.01..=10000
    /// (NaN to 0.01). `Settings::ambient_occlusion` turns ambient occlusion
    /// off.
    pub ambient_occlusion_radius: f32,
    /// How far screen-space indirect light reaches, in world metres,
    /// clamped to 1..=25 (NaN to 5). `Settings::screen_space_indirect`
    /// turns it on.
    pub screen_space_indirect_radius: f32,
}

impl FrameInput {
    /// `camera` with no lights, fog medium, mist, visibility groups
    /// or environment; the environment's diffuse lighting, reflections and
    /// backdrop unturned at intensity 1; baked lighting on and atmosphere
    /// off; a 60 Hz frame time; a fixed exposure of 0 stops and no local
    /// exposure; and the default bloom, motion blur and colour grading, an
    /// ambient occlusion radius of 0.5 m and a screen-space indirect light
    /// radius of 5 m.
    pub fn new(camera: Camera) -> Self {
        Self {
            camera,
            camera_cut: false,
            elapsed_seconds: 0.,
            frame_time_ms: 1000. / 60.,
            visibility_mask: 0,
            environment: FrameEnvironment::None,
            directional_lights: [None; 2],
            hemisphere_light: HemisphereLight::default(),
            diffuse_environment: EnvironmentLight::default(),
            backdrop: Backdrop::Environment {
                yaw: 0.,
                brightness: 1.,
            },
            baked_lighting: true,
            atmosphere: false,
            fog: Fog::default(),
            mist: Mist::default(),
            reflection_environment: EnvironmentLight::default(),
            exposure: Exposure::default(),
            local_exposure: LocalExposure::default(),
            bloom: BloomParameters::default(),
            motion_blur: MotionBlurParameters::default(),
            color_grading: ColorGrading::default(),
            ambient_occlusion_radius: 0.5,
            screen_space_indirect_radius: 5.,
        }
    }
}
