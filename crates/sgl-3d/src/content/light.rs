//! A scene's point, spot and rectangle lights, as the game describes them to
//! `Scene::add_light` and `Scene::set_light`. Metres, radians and linear RGB.
use glam::Vec3;

/// Where a light shines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LightShape {
    /// Every direction alike.
    Point,
    /// A cone around `direction` (toward where it shines; any nonzero
    /// length): full intensity within `inner_angle` of it, none beyond
    /// `outer_angle`, and a smooth falloff between, as Filament's and Bevy's
    /// spot lights. `0 <= inner_angle <= outer_angle < π/2`.
    Spot {
        direction: Vec3,
        inner_angle: f32,
        outer_angle: f32,
    },
    /// A one-sided rectangle centred on the light's position, facing
    /// `direction` (toward where it shines; any nonzero length), `width`
    /// metres along `width_axis` and `height` metres across both, as Bevy's
    /// `RectLight`. It shines as a Lambertian emitter of even luminance over
    /// the half-space in front of it, integrated over its area by linearly
    /// transformed cosines (Heitz et al. 2016), so it lights nearby surfaces
    /// softly and stretches their highlights. `width_axis` is any vector
    /// not parallel to `direction`; its part across `direction` is the
    /// width's axis. `width` and `height` are positive.
    Rect {
        direction: Vec3,
        width_axis: Vec3,
        width: f32,
        height: f32,
    },
}

/// A point, spot or rectangle light: scene content, added with
/// `Scene::add_light`. Its light falls off with the inverse square of
/// distance (a rectangle's, of its distance to each point of its face) and
/// fades smoothly to nothing at `range` from its position (Filament's
/// punctual lights, Bevy's rectangle lights).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    pub position: Vec3,
    pub shape: LightShape,
    /// Linear RGB, nonnegative.
    pub color: [f32; 3],
    /// Luminous intensity per steradian (candela), nonnegative; zero turns
    /// the light off. A rectangle's is along its normal: its luminance times
    /// its area. Bevy's `RectLight` takes π times this, its flux in lumens.
    pub intensity: f32,
    /// Metres, positive: nothing farther away is lit.
    pub range: f32,
    /// The game's bake already holds this light: it lights only receivers
    /// without baked lighting, as Godot's `BAKE_STATIC` lights skip only
    /// lightmapped meshes. Those are the moving instances, and static
    /// receivers with no baked map: baked lighting is off
    /// (`FrameInput::baked_lighting`), or their material is not lightmapped
    /// and either no irradiance atlas is installed or their lightmap UV is
    /// unassigned ((0,0) or negative). A chart's black outside its cropped
    /// bounds is part of its bake. Static receivers with a baked map keep
    /// it, and a baked light's moving casters do not shadow them. A light
    /// that must is live (not baked) and left out of the game's bake.
    pub baked: bool,
    /// Scales its specular highlights, nonnegative: 1 is physical, 0 a
    /// diffuse-only light (Godot's `light_specular`), for a fixture already
    /// visible as an emitter in reflections and probes.
    pub specular: f32,
    /// Scales the light it scatters in the volumetric fog, nonnegative
    /// (Godot's `light_volumetric_fog_energy`): 1 is physical, 2 doubles
    /// it, and at most 0.001 (Godot's cutoff) leaves the light out of the
    /// fog, which then skips its attenuation and shadow lookup. Surfaces
    /// take the light alike whatever its value, and its shadow is still
    /// drawn for them. A `baked` light lights the fog too.
    pub fog_energy: f32,
    /// Casts a shadow, in the local-light shadow atlas, while its range
    /// reaches the camera's view and the atlas has room for it: the lights
    /// that cover most of the screen take the room first, and the rest are
    /// lit without a shadow. A rectangle has a point shadow from its centre
    /// over the half-space it lights, as Godot shadows its area lights.
    pub casts_shadow: bool,
}

impl Default for Light {
    /// Godot's `Light3D` defaults: a white point light at the origin, of
    /// π candela (its light energy of 1, which its renderer scales by π),
    /// reaching 5 metres, live, physical specular (its `light_specular` of
    /// 0.5, which its renderer doubles), fog energy 1 and no shadow. Set
    /// what differs and take the rest with `..Default::default()`.
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            shape: LightShape::Point,
            color: [1.; 3],
            intensity: std::f32::consts::PI,
            range: 5.,
            baked: false,
            specular: 1.,
            fog_energy: 1.,
            casts_shadow: false,
        }
    }
}
