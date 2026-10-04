//! The frame's lights and authored look, as the game describes them in
//! `FrameInput`: directional lights, the hemisphere fill, the environment's
//! lighting and backdrop, and the mist. Metres, radians
//! about +Y and linear RGB. Point and spot lights are scene content
//! (`Light`); SGL3D packs all of these into its frame data itself.
use glam::Vec3;

/// A light at infinity, such as the sun or the moon (Bevy's
/// `DirectionalLight`, Godot's `DirectionalLight3D`, Filament's directional
/// light).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionalLight {
    /// Where the light shines, from the light toward what it lights; any
    /// nonzero length. A zero or non-finite direction is no light.
    pub direction: Vec3,
    /// Linear RGB, nonnegative.
    pub color: [f32; 3],
    /// Illuminance at normal incidence (lux), on the scale of scene lights'
    /// candela; zero turns the light off.
    pub illuminance: f32,
    /// The light's shadow, `None` for none. One directional light has a
    /// shadow: the first that is on and has one.
    pub shadow: Option<DirectionalShadow>,
    /// Scales the light it scatters in the volumetric fog, nonnegative
    /// (Godot's `light_volumetric_fog_energy`): 1 is physical, 2 doubles
    /// it, and at most 0.001 (Godot's cutoff) leaves the light out of the
    /// fog, which then skips its attenuation and shadow lookup. Surfaces
    /// take the light alike whatever its value, and its shadow is still
    /// drawn for them. A negative or non-finite value leaves it out too.
    pub fog_energy: f32,
}

impl Default for DirectionalLight {
    /// Godot's `DirectionalLight3D` defaults: white, shining along -Z (as
    /// Godot's and Bevy's untransformed lights do), of π lux (its light
    /// energy of 1, which its renderer scales by π), with no shadow and fog
    /// energy 1. Set what differs and take the rest with
    /// `..Default::default()`.
    fn default() -> Self {
        Self {
            direction: Vec3::NEG_Z,
            color: [1.; 3],
            illuminance: std::f32::consts::PI,
            shadow: None,
            fog_energy: 1.,
        }
    }
}

/// A directional light's cascaded shadow, which SGL3D splits and fits from
/// the camera (Godot's `DirectionalLight3D` splits, Bevy's cascade fit).
/// The camera's view depth from its near plane to `distance` is split into
/// `cascades`, each a shadow map of the same size covering a farther and
/// larger part of the view, so texels near the camera are small. SGL3D
/// places the splits as Godot does by default: each cascade but the last
/// ends 0.1, 0.2 and 0.5 of the way from the near plane to `distance`, and
/// the last at `distance`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionalShadow {
    /// The farthest view depth that is shadowed, in metres (Godot's
    /// `directional_shadow_max_distance`, Bevy's `maximum_distance`), at
    /// least 1 mm beyond the camera's near plane, as Godot keeps it, and at
    /// most 8192 m, the top of Godot's `directional_shadow_max_distance`
    /// range (NaN as 0). `DirectionalLight::shadow` as `None` casts
    /// no shadow.
    pub distance: f32,
    /// How many cascades split `distance`, 1 to 4; others are clamped.
    pub cascades: u32,
}

impl DirectionalShadow {
    /// Bevy's `CascadeShadowConfigBuilder` defaults (150 m and 4 cascades),
    /// as `Default::default()` returns them, for a `const` to build from:
    ///
    /// ```
    /// use sgl_3d::DirectionalShadow;
    /// const COURSE_SHADOW: DirectionalShadow = DirectionalShadow {
    ///     distance: 200.,
    ///     ..DirectionalShadow::DEFAULT
    /// };
    /// ```
    pub const DEFAULT: Self = Self {
        distance: 150.,
        cascades: 4,
    };
}

impl Default for DirectionalShadow {
    /// [`DirectionalShadow::DEFAULT`]. Set what differs and take the rest
    /// with `..Default::default()`.
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl DirectionalLight {
    /// The light reaches anything: positive illuminance and a direction
    /// that normalises. A light that is off is packed as no light and casts
    /// no shadow.
    pub(crate) fn is_on(&self) -> bool {
        self.illuminance > 0. && self.direction.try_normalize().is_some()
    }
}

/// A sky and ground fill (Three.js's `HemisphereLight`): a surface facing
/// straight up receives `sky_color` and one facing straight down
/// `ground_color`, blended by the normal's height, times `intensity`, as
/// irradiance on the directional lights' scale. It lights diffuse surfaces
/// only.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HemisphereLight {
    /// Linear RGB, nonnegative.
    pub sky_color: [f32; 3],
    /// Linear RGB, nonnegative.
    pub ground_color: [f32; 3],
    /// Nonnegative; zero turns the fill off.
    pub intensity: f32,
}

/// The frame environment's map turned about +Y and scaled where it lights
/// one lobe: `FrameInput::diffuse_environment` or `reflection_environment`.
/// Bevy's `EnvironmentMapLight` and Filament's `IndirectLight` carry the same
/// rotation and intensity but scale diffuse and specular together.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvironmentLight {
    /// Radians about +Y.
    pub yaw: f32,
    /// Nonnegative scale of the map's radiance.
    pub intensity: f32,
}

impl Default for EnvironmentLight {
    /// The map unturned at its own radiance (yaw 0, intensity 1), as
    /// Three.js r185's `Scene.environmentRotation` and
    /// `environmentIntensity`.
    fn default() -> Self {
        Self {
            yaw: 0.,
            intensity: 1.,
        }
    }
}

/// What the camera sees where no surface is (Godot's background mode,
/// Filament's `Skybox`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Backdrop {
    /// The frame environment's panorama (Bevy's `Skybox`), turned `yaw`
    /// radians about +Y, its radiance scaled by `brightness`.
    Environment { yaw: f32, brightness: f32 },
    /// One linear RGB colour.
    Color([f32; 3]),
}

/// The look of the mist billboards at the scene's mist positions
/// (`Scene::update_mist`), shaded by animated noise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mist {
    /// Linear RGB where the noise is thin.
    pub thin_color: [f32; 3],
    /// Linear RGB where the noise is dense.
    pub dense_color: [f32; 3],
    /// The densest noise's opacity, 0..=1; zero hides the mist.
    pub opacity: f32,
    /// Each billboard's width in metres.
    pub width: f32,
    /// Each billboard's height in metres.
    pub height: f32,
    /// How fast and which way the noise moves across each billboard, in
    /// billboard widths per second rightward and heights per second upward
    /// on screen.
    pub drift: [f32; 2],
}

impl Default for Mist {
    /// Hidden: black, opacity 0, on 1 m square billboards, drifting slowly
    /// up and to the left. Set the colours and opacity to show it.
    fn default() -> Self {
        Self {
            thin_color: [0.; 3],
            dense_color: [0.; 3],
            opacity: 0.,
            width: 1.,
            height: 1.,
            // SGL3D's own, from Hyperdrive: its noise, 7 by 3 cells across
            // a billboard (mist.wgsl), moved -0.025 and 0.035 cells a second.
            drift: [-0.025 / 7., 0.035 / 3.],
        }
    }
}
