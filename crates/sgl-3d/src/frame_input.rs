//! What the caller supplies for one frame: the camera, the authored look and
//! per-frame state. Linear colours and metres. No simulation clock is
//! advanced here; rendering settings are `settings::Settings`, and point and
//! spot lights are scene content (`Scene::add_light`).
mod look;

pub use look::{
    AutoExposure, BloomParameters, ColorGrading, ColorGradingGlobal, ColorGradingSection,
    CompensationCurve, CompensationCurveError, Exposure, MeteringMask, MotionBlurParameters,
};

use crate::content::identity::EnvironmentId;
use crate::content::lighting::{
    Backdrop, DirectionalLight, EnvironmentLight, HemisphereLight, Mist,
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
    /// The view depth in metres the froxel volume covers; anything farther,
    /// the sky included, takes the fog as far as it.
    pub length: f32,
    /// Above 1, more of the volume's depth slices lie near the camera.
    pub detail_spread: f32,
    /// 0..1: the share of the last frame's volume each froxel keeps where it
    /// reprojects. Higher is smoother; lower trails less behind moving lights.
    pub temporal_reprojection: f32,
}

impl Default for Fog {
    /// No medium, with Godot's defaults: albedo white, anisotropy 0.2, no
    /// ambient light, uniform, a 64 m volume, detail spread 2 and 0.9 of the
    /// reprojected volume kept.
    fn default() -> Self {
        Self {
            density: 0.,
            albedo: [1.; 3],
            anisotropy: 0.2,
            ambient: 0.,
            height: 0.,
            height_falloff: 0.,
            length: 64.,
            detail_spread: 2.,
            temporal_reprojection: 0.9,
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
    /// Presentation seconds, for atmosphere animation.
    pub elapsed_seconds: f32,
    /// Milliseconds since the previous frame: FSR2's `frameTimeDelta` and
    /// auto exposure's time step.
    pub frame_time_ms: f32,
    /// Enabled material visibility groups. Use only the low 24 bits.
    pub visibility_mask: u32,
    /// The scene's environment that lights the frame, draws its sky and
    /// backs its reflections; with none, or one the scene no longer has, no
    /// environment does.
    pub environment: Option<EnvironmentId>,
    /// Up to two directional lights; `None` is no light.
    pub directional_lights: [Option<DirectionalLight>; 2],
    pub hemisphere_light: HemisphereLight,
    /// How `environment` lights surfaces' diffuse lobe. Their specular
    /// lobe takes `reflection_environment`.
    pub diffuse_environment: EnvironmentLight,
    pub backdrop: Backdrop,
    /// Baked (fixed) lighting: lightmaps, irradiance atlases and instances'
    /// baked irradiance.
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
    pub bloom: BloomParameters,
    /// While `Settings::motion_blur` is on.
    pub motion_blur: MotionBlurParameters,
    pub color_grading: ColorGrading,
    /// XeGTAO's search radius in world metres; nonpositive or nonfinite
    /// turns ambient occlusion off.
    pub ambient_occlusion_radius: f32,
}

impl FrameInput {
    /// `camera` with no lights, fog medium, mist, visibility groups
    /// or environment; the environment's diffuse lighting, reflections and
    /// backdrop unturned at intensity 1; baked lighting on and atmosphere
    /// off; a 60 Hz frame time; a fixed exposure of 0 stops; and the default
    /// bloom, motion blur, colour grading and ambient occlusion radius.
    pub fn new(camera: Camera) -> Self {
        Self {
            camera,
            camera_cut: false,
            elapsed_seconds: 0.,
            frame_time_ms: 1000. / 60.,
            visibility_mask: 0,
            environment: None,
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
            bloom: BloomParameters::default(),
            motion_blur: MotionBlurParameters::default(),
            color_grading: ColorGrading::default(),
            ambient_occlusion_radius: 0.5,
        }
    }
}
