//! Rust mirror of decal_records.wgsl: a decal's record, at its identity's
//! index in the scene's decal buffer.
use crate::content::decal::Decal;
use glam::{Mat3, Mat4, Vec3};

/// One decal as shading reads it (`Decal` in decal_records.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DecalRecord {
    /// From the world to the box's decal space: x and z across its images
    /// from 0 to 1, y from −1 at its lower face to 1 at its upper.
    pub decal_from_world: [[f32; 4]; 4],
    pub x_axis: [f32; 3],
    pub upper_fade: f32,
    pub y_axis: [f32; 3],
    pub lower_fade: f32,
    pub z_axis: [f32; 3],
    pub normal_fade: f32,
    /// Linear RGB and alpha that multiply its base colour image.
    pub color: [f32; 4],
    /// Each image's place in the decal atlas: offset in xy and size in zw,
    /// in atlas UV; zero for a map it does not have.
    pub base_color_rect: [f32; 4],
    pub normal_rect: [f32; 4],
    pub metallic_roughness_rect: [f32; 4],
    pub base_color_mix: f32,
    pub padding: [f32; 3],
}

/// Where a decal's images are in the decal atlas, in atlas UV.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct DecalRects {
    pub base_color: [f32; 4],
    pub normal: Option<[f32; 4]>,
    pub metallic_roughness: Option<[f32; 4]>,
}

impl DecalRecord {
    /// `decal`'s record, valid as `Scene::add_decal` accepts it, with its
    /// images at `rects`. Its decal space is Godot b130438's
    /// (`TextureStorage::update_decal_buffer`, the inverse of the decal's
    /// transform scaled by its half extents and `uv_xform`): the box's local
    /// X and Z from its −X and −Z faces at 0 to its +X and +Z faces at 1, and
    /// its local Y over its half height.
    pub fn new(decal: &Decal, rects: DecalRects) -> Self {
        let rotation = decal.rotation.normalize();
        let axes = Mat3::from_quat(rotation);
        let local_from_world =
            Mat4::from_quat(rotation.inverse()) * Mat4::from_translation(-decal.position);
        let decal_from_local = Mat4::from_translation(Vec3::new(0.5, 0., 0.5))
            * Mat4::from_scale(Vec3::new(
                1. / decal.size.x,
                2. / decal.size.y,
                1. / decal.size.z,
            ));
        Self {
            decal_from_world: (decal_from_local * local_from_world).to_cols_array_2d(),
            x_axis: axes.x_axis.to_array(),
            upper_fade: decal.upper_fade,
            y_axis: axes.y_axis.to_array(),
            lower_fade: decal.lower_fade,
            z_axis: axes.z_axis.to_array(),
            normal_fade: decal.normal_fade,
            color: decal.color,
            base_color_rect: rects.base_color,
            normal_rect: rects.normal.unwrap_or_default(),
            metallic_roughness_rect: rects.metallic_roughness.unwrap_or_default(),
            base_color_mix: decal.base_color_mix,
            padding: [0.; 3],
        }
    }
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    use crate::shading::layout_tests::mirror;
    [mirror!(
        "geometry",
        "Decal",
        DecalRecord,
        [
            decal_from_world,
            x_axis,
            upper_fade,
            y_axis,
            lower_fade,
            z_axis,
            normal_fade,
            color,
            base_color_rect,
            normal_rect,
            metallic_roughness_rect,
            base_color_mix,
            padding,
        ]
    )]
}
