//! A scene's point, spot and rectangle lights, as the game describes them to
//! `Scene::add_light` and `Scene::set_light`. Metres, radians and linear RGB.
use super::identity::{DecalImageId, LightProfileId};
use glam::Vec3;

/// Where a light shines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LightShape {
    /// Every direction alike. `radius` (metres, nonnegative) is the size
    /// of the sphere it shines from: its specular highlights spread over the
    /// sphere's reflection (Karis's representative point, as Bevy's point
    /// lights), and a ray-traced shadow's rays, and the dynamic GI volume's
    /// visibility rays, end just short of a point of it, so a larger light
    /// casts a broader highlight and a softer shadow and 0 a point's, as
    /// Wicked Engine's `LightComponent::radius`
    /// (`LightShape::DEFAULT_RADIUS`). Diffuse light and the shadow maps
    /// treat the light as a point.
    Point { radius: f32 },
    /// A cone around `direction` (toward where it shines; any nonzero
    /// length): full intensity within `inner_angle` of it, none beyond
    /// `outer_angle`, and a smooth falloff between, as Filament's and Bevy's
    /// spot lights. `0 <= inner_angle <= outer_angle < π/2`. `radius` is as
    /// a point light's.
    Spot {
        direction: Vec3,
        inner_angle: f32,
        outer_angle: f32,
        radius: f32,
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

impl LightShape {
    /// Wicked Engine's default light radius, in metres
    /// (`LightComponent::radius`).
    pub const DEFAULT_RADIUS: f32 = 0.025;
}

/// An image a spot light projects (a cookie or gobo: Godot's spot
/// `light_projector`, Wicked Engine's light mask). It fills the light's
/// projector square, which spans twice the outer cone angle about the
/// spot's direction, so the cone's circle is inscribed in it, with the
/// image's top toward `up`. The light's radiance is multiplied by the
/// image's linear colour times its alpha (the image sampled as sRGB colour)
/// wherever the light reaches: on surfaces, at reflections' and the dynamic
/// GI volume's ray hits, and in the volumetric fog, whether or not the
/// light casts a shadow. A white opaque image leaves the light as it was; a
/// black or transparent one stops it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightCookie {
    /// The image, from `Scene::add_decal_image`: it shares the decal atlas,
    /// and cannot be removed while a light projects it.
    pub image: DecalImageId,
    /// The direction of the image's top: any vector not parallel to the
    /// spot's direction, whose part across the direction is used, as
    /// `LightShape::Rect`'s `width_axis`. The image's right is the spot's
    /// direction crossed with it.
    pub up: Vec3,
}

/// A measured profile a spot light shines with (`LightProfile`, from
/// `Scene::add_light_profile`), turned about the spot's direction: the
/// profile's nadir (vertical angle 0°) is the spot's direction and its 0°
/// horizontal angle lies toward `zero_azimuth`, its horizontal angles
/// increasing counterclockwise seen looking along the direction (LM-63's
/// type C, from above), so its 90° lies toward `zero_azimuth` crossed with
/// the direction. The spot's intensity toward each direction inside its
/// cone is multiplied by the profile's there, whose brightest direction is
/// 1: `Light::intensity` is the candela in that direction
/// (`LightProfile::peak_candela` for the file's own). Its cone still fades
/// it and bounds it, and a cookie multiplies it too, wherever the light
/// reaches: on surfaces, at reflections' and the dynamic GI volume's ray
/// hits, and in the volumetric fog.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpotProfile {
    /// The profile, from `Scene::add_light_profile`: it cannot be removed
    /// while a light shines with it.
    pub profile: LightProfileId,
    /// The direction of the profile's 0° horizontal angle (along a
    /// luminaire's length): any vector not parallel to the spot's
    /// direction, whose part across the direction is used, as
    /// `LightCookie::up`.
    pub zero_azimuth: Vec3,
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
    /// How dark its shadow is, 0..=1 (Godot's `shadow_opacity`), on surfaces
    /// and in the volumetric fog alike: 1 hides all of its light behind its
    /// casters, 0.4 lets 0.6 of it through, and at most 0.001 (Godot's
    /// cutoff) draws no shadow and skips the lookup.
    pub shadow_opacity: f32,
    /// The image a spot light projects (`LightCookie`); a point or
    /// rectangle light takes none. A `baked` light's pattern must be in the
    /// game's bake too, which holds that light.
    pub cookie: Option<LightCookie>,
    /// The measured profile a spot light shines with (`SpotProfile`); a
    /// point or rectangle light takes none. A `baked` light's profile must
    /// be in the game's bake too, which holds that light.
    pub profile: Option<SpotProfile>,
}

impl Default for Light {
    /// Godot's `Light3D` defaults, with Wicked Engine's radius: a white
    /// point light of 2.5 cm (`LightShape::DEFAULT_RADIUS`) at the origin, of
    /// π candela (its light energy of 1, which its renderer scales by π),
    /// reaching 5 metres, live, physical specular (its `light_specular` of
    /// 0.5, which its renderer doubles), fog energy 1, no shadow, at
    /// opacity 1 when cast, and no cookie or profile. Set what differs and take the
    /// rest with `..Default::default()`.
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            shape: LightShape::Point {
                radius: LightShape::DEFAULT_RADIUS,
            },
            color: [1.; 3],
            intensity: std::f32::consts::PI,
            range: 5.,
            baked: false,
            specular: 1.,
            fog_energy: 1.,
            casts_shadow: false,
            shadow_opacity: 1.,
            cookie: None,
            profile: None,
        }
    }
}
