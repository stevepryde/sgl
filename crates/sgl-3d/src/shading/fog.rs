//! Rust mirror of fog.wgsl's fog volume record, which the scene packs and
//! the fog stage reads.
use crate::content::transient::FogVolume;

/// `FogVolumeRecord` in fog.wgsl: a box's frame, size and medium.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct FogVolumeRecord {
    pub local_from_world: [[f32; 4]; 4],
    pub half_size: [f32; 3],
    pub density: f32,
    pub albedo: [f32; 3],
    pub edge_fade: f32,
    pub center: [f32; 3],
    pub radius_squared: f32,
}

impl FogVolumeRecord {
    pub fn new(volume: &FogVolume) -> Self {
        let world_from_local = world_from_local(volume);
        let half_size = volume.size * 0.5;
        Self {
            local_from_world: world_from_local.inverse().to_cols_array_2d(),
            half_size: half_size.to_array(),
            density: volume.density,
            albedo: volume.albedo,
            edge_fade: volume.edge_fade,
            center: volume.center.to_array(),
            radius_squared: half_size.length_squared(),
        }
    }
}

/// The world positions of `volume`'s box's eight corners.
pub(crate) fn corners(volume: &FogVolume) -> [glam::Vec3; 8] {
    let world_from_local = world_from_local(volume);
    let half_size = volume.size * 0.5;
    std::array::from_fn(|corner| {
        let sign = glam::Vec3::new(
            if corner & 1 == 0 { -1. } else { 1. },
            if corner & 2 == 0 { -1. } else { 1. },
            if corner & 4 == 0 { -1. } else { 1. },
        );
        world_from_local.transform_point3(half_size * sign)
    })
}

/// `volume`'s box's frame in the world.
fn world_from_local(volume: &FogVolume) -> glam::Mat4 {
    glam::Mat4::from_rotation_translation(volume.rotation.normalize(), volume.center)
}

/// The layouts this module mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    use crate::shading::layout_tests::mirror;
    [mirror!(
        "volumetric_fog",
        "FogVolumeRecord",
        FogVolumeRecord,
        [
            local_from_world,
            half_size,
            density,
            albedo,
            edge_fade,
            center,
            radius_squared
        ]
    )]
}
