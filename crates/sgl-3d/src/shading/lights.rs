//! Rust mirrors of light_records.wgsl: a scene light's record and a light's
//! shadow in the local-light shadow atlas.
use crate::content::light::{Light, LightShape};

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
}

/// `LightRecord::shape`: a point or spot light.
pub(crate) const LIGHT_PUNCTUAL: u32 = 0;
/// `LightRecord::shape`: a rectangle.
pub(crate) const LIGHT_RECT: u32 = 1;

impl LightRecord {
    /// `light`'s record, valid as `Scene::add_light` accepts it. A spot's
    /// cone is Bevy's spot scale and offset (`prepare_lights`), which
    /// Filament's `getAngleAttenuation` takes; a point light's and a
    /// rectangle's leave it whole. A rectangle's luminance is its intensity
    /// over its area, as Bevy's `extract_lights` divides its flux by its
    /// area and π.
    pub fn new(light: &Light) -> Self {
        let mut color = light.color.map(|channel| channel * light.intensity);
        let (shape, direction, half_width, half_height, spot_scale, spot_offset) = match light.shape
        {
            LightShape::Point => (LIGHT_PUNCTUAL, [0.; 3], [0.; 3], 0., 0., 1.),
            LightShape::Spot {
                direction,
                inner_angle,
                outer_angle,
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
        }
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
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 5] {
    use crate::shading::layout_tests::Constant;
    use naga::Literal::U32;
    [
        Constant::new("geometry", "LIGHT_PUNCTUAL", U32(LIGHT_PUNCTUAL)),
        Constant::new("geometry", "LIGHT_RECT", U32(LIGHT_RECT)),
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
