//! Rust mirror of fog.wgsl's fog volume record, which the scene packs and
//! the fog stage reads, and the spread of the froxel volume's slices.
use crate::content::transient::FogVolume;

/// How the froxel volume spaces its depth slices, as fog.wgsl's
/// `fog_slice_depth` takes it: above 1, more of them lie near the camera.
/// Godot's default `volumetric_fog_detail_spread` (b130438
/// `scene/resources/environment.h`, MIT). The frame's uniform carries it to
/// draws, and the fog stage's to its passes and its fog volumes' froxel
/// bounds.
pub(crate) const DETAIL_SPREAD: f32 = 2.;

/// `projection`'s x and y scales and x and y offsets, as fog.wgsl's
/// `fog_froxel_view_position` takes them: the froxel columns' rays, which
/// the fog's froxels and the sky's aerial volume both follow.
pub(crate) fn froxel_projection(projection: glam::Mat4) -> [f32; 4] {
    [
        projection.x_axis.x,
        projection.y_axis.y,
        projection.z_axis.x,
        projection.z_axis.y,
    ]
}

/// `FogLayout` in fog.wgsl: the frame's one fog volume's slices, which
/// `Frame`, the fog stage's integration and source completion hold.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct FogLayout {
    /// One over the fog's length (0 while the fog does not run) and over
    /// its detail spread.
    pub inverse_length: f32,
    pub inverse_detail_spread: f32,
    /// How far the aerial slices reach, metres of view depth.
    pub aerial_reach: f32,
    pub fog_slices: u32,
    pub aerial_slices: u32,
}

impl FogLayout {
    /// The volume of a frame whose fog runs with `fog` slices over `length`
    /// metres (none while it does not), with the aerial volume's slices to
    /// its reach while aerial perspective is in effect (`aerial`).
    pub fn new(fog: Option<(u32, f32)>, aerial: bool) -> Self {
        Self {
            inverse_length: fog.map_or(0., |(_, length)| length.recip()),
            inverse_detail_spread: DETAIL_SPREAD.recip(),
            aerial_reach: super::sky::AERIAL_REACH,
            fog_slices: fog.map_or(0, |(slices, _)| slices),
            aerial_slices: if aerial { super::sky::AERIAL_SLICES } else { 0 },
        }
    }

    /// The volume's slices: the fog's, then the aerial slices and the fog
    /// alone.
    pub fn slices(&self) -> u32 {
        self.fog_slices
            + if self.aerial_slices > 0 {
                self.aerial_slices + 1
            } else {
                0
            }
    }
}

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
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 2] {
    use crate::shading::layout_tests::mirror;
    [
        mirror!(
            "volumetric_fog",
            "FogLayout",
            FogLayout,
            [
                inverse_length,
                inverse_detail_spread,
                aerial_reach,
                fog_slices,
                aerial_slices
            ]
        ),
        mirror!(
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
        ),
    ]
}
