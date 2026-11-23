//! Rust mirrors of light_records.wgsl: a scene light's record and a light's
//! shadow in the local-light shadow atlas.
use crate::content::light::{Light, LightCookie, LightShape, SpotProfile};
use glam::Vec3;

/// One scene light as shading reads it (`Light` in light_records.wgsl), at
/// its identity's index in the scene's light buffer.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct LightRecord {
    pub position: [f32; 3],
    pub inverse_square_range: f32,
    /// Linear RGB times intensity: per steradian for a point or spot light,
    /// a rectangle's luminance (its intensity over its area).
    pub color: [f32; 3],
    pub specular: f32,
    /// A spot light's unit direction, or a rectangle's unit normal; zero for
    /// a point light.
    pub direction: [f32; 3],
    pub range: f32,
    pub spot_scale: f32,
    pub spot_offset: f32,
    /// `LIGHT_PUNCTUAL` or `LIGHT_RECT`.
    pub shape: u32,
    /// The scale of its light in the volumetric fog.
    pub fog_energy: f32,
    /// A rectangle's half width along its width's axis; zero for a point or
    /// spot light.
    pub half_width: [f32; 3],
    /// Half a rectangle's height, along its width's axis crossed with its
    /// normal; zero for a point or spot light.
    pub half_height: f32,
    /// How dark its shadow is (`Light::shadow_opacity`).
    pub shadow_opacity: f32,
    /// `LIGHT_*` bits.
    pub flags: u32,
    /// A point or spot light's radius in metres, which sizes its highlights
    /// and its rays' ends (`LightShape`); zero for a rectangle.
    pub radius: f32,
    /// WGSL aligns `cookie_rect` to 16 bytes.
    pub padding: f32,
    /// A spot's cookie's place in the decal atlas, in atlas UV (offset in
    /// xy, size in zw); all zero for a light without one.
    pub cookie_rect: [f32; 4],
    /// A cookied spot's unit right axis: its direction crossed with the
    /// cookie's up (`LightCookie::up`); zero without a cookie.
    pub cookie_right: [f32; 3],
    /// A cookied spot's outer cone's cotangent, which scales its projector
    /// square to the cone; zero without a cookie.
    pub cookie_cotangent: f32,
    /// A profiled spot's grid in the decal atlas (`light_profile.wgsl`):
    /// the atlas UV of its first sample's centre in xy, and the atlas UV
    /// per radian of vertical angle in z and of horizontal angle in w (zero
    /// for one row); all zero for a light without a profile.
    pub profile_lookup: [f32; 4],
    /// A profiled spot's unit direction of its profile's 0° horizontal
    /// angle, across its direction (`SpotProfile::zero_azimuth`); zero
    /// without a profile.
    pub profile_zero: [f32; 3],
    /// WGSL rounds the record to 16 bytes.
    pub profile_padding: f32,
}

/// Where a light's cookie and profile lie in the decal atlas its record is
/// read with: each one's place in atlas UV (offset in xy, size in zw), and
/// the profile's grid size in samples (`LightProfile::size`). What the
/// light lacks is ignored.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct AtlasPlaces {
    pub cookie: [f32; 4],
    pub profile: [f32; 4],
    pub profile_size: [u32; 2],
}

/// `LightRecord::shape`: a point or spot light.
pub(crate) const LIGHT_PUNCTUAL: u32 = 0;
/// `LightRecord::shape`: a rectangle.
pub(crate) const LIGHT_RECT: u32 = 1;
/// `LightRecord::flags`: the light casts a shadow (`Light::casts_shadow`).
pub(crate) const LIGHT_CASTS_SHADOW: u32 = 1;
/// The shadow opacity at or below which a light's shadow is not looked up
/// (`SHADOW_OPACITY_CUTOFF` in shadow_sampling.wgsl, Godot's).
pub(crate) const SHADOW_OPACITY_CUTOFF: f32 = 0.001;

impl LightRecord {
    /// `light`'s record, valid as `Scene::add_light` accepts it. A spot's
    /// cone is Bevy's spot scale and offset (`prepare_lights`), which
    /// Filament's `getAngleAttenuation` takes; a point light's and a
    /// rectangle's leave it whole. A rectangle's luminance is its intensity
    /// over its area, as Bevy's `extract_lights` divides its flux by its
    /// area and π. `places` are its cookie's and profile's in the decal
    /// atlas the record is read with.
    pub fn new(light: &Light, places: AtlasPlaces) -> Self {
        let cookie = cookie(light, places.cookie);
        let mut color = light.color.map(|channel| channel * light.intensity);
        let (shape, direction, half_width, half_height, spot_scale, spot_offset) = match light.shape
        {
            LightShape::Point { .. } => (LIGHT_PUNCTUAL, [0.; 3], [0.; 3], 0., 0., 1.),
            LightShape::Spot {
                direction,
                inner_angle,
                outer_angle,
                ..
            } => {
                let cos_outer = outer_angle.cos();
                let scale = 1. / (inner_angle.cos() - cos_outer).max(1e-4);
                (
                    LIGHT_PUNCTUAL,
                    direction.normalize().to_array(),
                    [0.; 3],
                    0.,
                    scale,
                    -cos_outer * scale,
                )
            }
            LightShape::Rect {
                direction,
                width_axis,
                width,
                height,
            } => {
                let normal = direction.normalize();
                let across = width_axis.reject_from_normalized(normal).normalize();
                color = color.map(|channel| channel / (width * height));
                (
                    LIGHT_RECT,
                    normal.to_array(),
                    (across * (0.5 * width)).to_array(),
                    0.5 * height,
                    0.,
                    1.,
                )
            }
        };
        Self {
            position: light.position.to_array(),
            inverse_square_range: 1. / (light.range * light.range),
            color,
            specular: light.specular,
            direction,
            range: light.range,
            spot_scale,
            spot_offset,
            shape,
            fog_energy: light.fog_energy,
            half_width,
            half_height,
            shadow_opacity: light.shadow_opacity,
            flags: if light.casts_shadow {
                LIGHT_CASTS_SHADOW
            } else {
                0
            },
            radius: match light.shape {
                LightShape::Point { radius } | LightShape::Spot { radius, .. } => radius,
                LightShape::Rect { .. } => 0.,
            },
            padding: 0.,
            cookie_rect: cookie.cookie_rect,
            cookie_right: cookie.cookie_right,
            cookie_cotangent: cookie.cookie_cotangent,
            ..profile(light, places)
        }
    }
}

/// A cookie's frame on a light of `shape`: its unit right axis and its
/// outer cone's cotangent, which fit its projector square to the cone
/// (`light_cookie.wgsl`); none unless the light is a spot whose outer
/// angle and the part of the cookie's `up` across its direction give
/// finite ones. The right axis is the spot's direction crossed with up, as
/// Bevy 9d12036 projects a spot's texture through the light's transform
/// (`extract_spot_light_textures`, crates/bevy_pbr/src/decal/clustered.rs
/// 346–372), whose +X is its up crossed with its back.
pub(crate) fn cookie_frame(cookie: &LightCookie, shape: &LightShape) -> Option<(Vec3, f32)> {
    let LightShape::Spot {
        direction,
        outer_angle,
        ..
    } = *shape
    else {
        return None;
    };
    let direction = direction.normalize();
    let right = direction
        .cross(cookie.up.reject_from_normalized(direction))
        .normalize();
    let cotangent = 1. / outer_angle.tan();
    (outer_angle > 0. && right.is_finite() && cotangent.is_finite()).then_some((right, cotangent))
}

/// The record's cookie rows: its image's place in the atlas and its frame
/// (`cookie_frame`), or zero without a cookie.
fn cookie(light: &Light, rect: [f32; 4]) -> LightRecord {
    let zeroed = bytemuck::Zeroable::zeroed();
    let Some((right, cotangent)) = light
        .cookie
        .and_then(|cookie| cookie_frame(&cookie, &light.shape))
    else {
        return zeroed;
    };
    LightRecord {
        cookie_rect: rect,
        cookie_right: right.to_array(),
        cookie_cotangent: cotangent,
        ..zeroed
    }
}

/// A profile's unit 0° horizontal axis on a light of `shape`: the part of
/// its `zero_azimuth` across the spot's direction; none unless the light
/// is a spot and that part is finite and nonzero.
pub(crate) fn profile_zero(profile: &SpotProfile, shape: &LightShape) -> Option<Vec3> {
    let LightShape::Spot { direction, .. } = *shape else {
        return None;
    };
    let zero = profile
        .zero_azimuth
        .reject_from_normalized(direction.normalize())
        .normalize();
    zero.is_finite().then_some(zero)
}

/// The record's profile rows: where its grid's samples lie in the atlas
/// (`places.profile`, `places.profile_size`), its first one's centre and
/// the atlas UV per radian of each angle, which span 0° to 90° across its
/// columns and 0° to 360° down its rows; and its frame (`profile_zero`).
/// Zero without a profile.
fn profile(light: &Light, places: AtlasPlaces) -> LightRecord {
    let zeroed = bytemuck::Zeroable::zeroed();
    let Some(zero) = light
        .profile
        .and_then(|profile| profile_zero(&profile, &light.shape))
    else {
        return zeroed;
    };
    let [x, y, width, height] = places.profile;
    let [columns, rows] = places.profile_size.map(|side| side as f32);
    let texel = [width / columns, height / rows];
    let across = |samples: f32, span: f32, texel: f32| {
        if samples > 1. {
            texel * (samples - 1.) / span
        } else {
            0.
        }
    };
    LightRecord {
        profile_lookup: [
            x + 0.5 * texel[0],
            y + 0.5 * texel[1],
            across(columns, std::f32::consts::FRAC_PI_2, texel[0]),
            across(rows, std::f32::consts::TAU, texel[1]),
        ],
        profile_zero: zero.to_array(),
        ..zeroed
    }
}

/// `LocalShadow::kind`: the light has no shadow.
pub(crate) const LOCAL_SHADOW_NONE: u32 = 0;
/// `LocalShadow::kind`: six cube faces around the light.
pub(crate) const LOCAL_SHADOW_CUBE: u32 = 1;
/// `LocalShadow::kind`: one spot face.
pub(crate) const LOCAL_SHADOW_SPOT: u32 = 2;

/// A scene light's shadow in the local-light shadow atlas (`LocalShadow` in
/// light_records.wgsl), at its identity's index in group 0's
/// `local_shadows`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct LocalShadowRecord {
    /// A spot face's reversed-Z projection from the world.
    pub clip_from_world: [[f32; 4]; 4],
    /// Each face's top-left corner in atlas UV: face 2i in xy, 2i+1 in zw.
    pub corners: [[f32; 4]; 3],
    /// A face's size in atlas UV.
    pub size: f32,
    /// The near plane of the faces' projections.
    pub near: f32,
    /// World metres per atlas texel at a metre along a face's axis.
    pub texel_scale: f32,
    /// `LOCAL_SHADOW_*`.
    pub kind: u32,
    /// 1 while its static layers hold its static casters.
    pub layers: u32,
    pub padding: [u32; 3],
}

impl LocalShadowRecord {
    /// A light without a shadow.
    pub const NONE: Self = Self {
        clip_from_world: [[0.; 4]; 4],
        corners: [[0.; 4]; 3],
        size: 0.,
        near: 0.,
        texel_scale: 0.,
        kind: LOCAL_SHADOW_NONE,
        layers: 0,
        padding: [0; 3],
    };
}

#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 7] {
    use crate::shading::layout_tests::Constant;
    use naga::Literal::U32;
    [
        Constant::new("geometry", "LIGHT_PUNCTUAL", U32(LIGHT_PUNCTUAL)),
        Constant::new("geometry", "LIGHT_RECT", U32(LIGHT_RECT)),
        Constant::new("geometry", "LIGHT_CASTS_SHADOW", U32(LIGHT_CASTS_SHADOW)),
        Constant::new(
            "geometry",
            "SHADOW_OPACITY_CUTOFF",
            naga::Literal::F32(SHADOW_OPACITY_CUTOFF),
        ),
        Constant::new("geometry", "LOCAL_SHADOW_NONE", U32(LOCAL_SHADOW_NONE)),
        Constant::new("geometry", "LOCAL_SHADOW_CUBE", U32(LOCAL_SHADOW_CUBE)),
        Constant::new("geometry", "LOCAL_SHADOW_SPOT", U32(LOCAL_SHADOW_SPOT)),
    ]
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 2] {
    use crate::shading::layout_tests::mirror;
    [
        mirror!(
            "geometry",
            "Light",
            LightRecord,
            [
                position,
                inverse_square_range,
                color,
                specular,
                direction,
                range,
                spot_scale,
                spot_offset,
                shape,
                fog_energy,
                half_width,
                half_height,
                shadow_opacity,
                flags,
                radius,
                cookie_rect,
                cookie_right,
                cookie_cotangent,
                profile_lookup,
                profile_zero,
            ]
        ),
        mirror!(
            "geometry",
            "LocalShadow",
            LocalShadowRecord,
            [
                clip_from_world,
                corners,
                size,
                near,
                texel_scale,
                kind,
                layers,
                padding
            ]
        ),
    ]
}
