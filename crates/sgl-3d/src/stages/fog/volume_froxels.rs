//! Which froxels each of the scene's fog volumes may reach in a frame, so
//! the injection sums a volume only there: Godot's per-volume froxel bounds
//! (b130438 `servers/rendering/renderer_rd/environment/fog.cpp`
//! `volumetric_fog_update` and `_point_get_position_in_froxel_volume`, MIT,
//! `src/LICENSE-godot.txt`), over which Godot dispatches each volume's
//! kernel. Changes: SGL3D's froxels, placed by the camera's projection;
//! bounds that hold every froxel the box may reach, where Godot's take the
//! froxel before each corner's and leave out the last; and the whole frame
//! wherever the box reaches the camera's plane, where Godot takes it when
//! the camera lies within the near plane's reach of the box.
use super::FroxelVolumeUniform;
use glam::{Mat4, UVec3, Vec2, Vec3};

/// `FogVolumeFroxels` in fog.wgsl: the froxels, `first` to `last`
/// inclusive, that the scene's fog volume `volume` reaches.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct FogVolumeFroxels {
    first: [u32; 3],
    volume: u32,
    last: [u32; 3],
    padding: u32,
}

/// The most fog volumes a frame's fog sums (`FOG_MOST_VOLUMES`, AR-12): the
/// first that reach it, in the scene's order. Fog volumes are authored
/// boxes, a handful in a view; each froxel loops over those reaching the
/// frame, and the cap bounds that loop far above any scene's.
pub(super) const MOST_VOLUMES: usize = 1024;

/// The share of a froxel by which a volume's bounds grow on every side, so
/// rounding between them and the injection's froxel positions never leaves
/// out a froxel the volume reaches.
const MARGIN: f32 = 0.01;

/// The froxels each of the scene's fog volumes reaches this frame, in the
/// buffer the injection binds.
pub(super) struct ReachedFroxels {
    pub(super) buffer: wgpu::Buffer,
}

impl ReachedFroxels {
    pub(super) fn new(device: &wgpu::Device) -> Self {
        Self {
            buffer: buffer(device, 0),
        }
    }

    /// Writes the froxels of `froxel_volume`, seen through
    /// `view_from_world`, that each of the boxes with world corners
    /// `corners` reaches, replacing the buffer when it is too small, and
    /// returns how many boxes reach any.
    pub(super) fn write(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        corners: &[[Vec3; 8]],
        view_from_world: Mat4,
        froxel_volume: &FroxelVolumeUniform,
    ) -> u32 {
        let reached = reached(corners, view_from_world, froxel_volume);
        if size_of_val(reached.as_slice()) as u64 > self.buffer.size() {
            self.buffer = buffer(device, reached.len());
        }
        if !reached.is_empty() {
            crate::counters::write_buffer(queue, &self.buffer, 0, bytemuck::cast_slice(&reached));
        }
        reached.len() as u32
    }
}

/// The froxels of `froxel_volume`, seen through `view_from_world`, that
/// each of the boxes with world corners `corners` reaches, in their order,
/// for at most the first `MOST_VOLUMES` that reach any; none for a box
/// behind the camera, beyond the volume, or wholly in front of the camera
/// and beside the frame.
fn reached(
    corners: &[[Vec3; 8]],
    view_from_world: Mat4,
    froxel_volume: &FroxelVolumeUniform,
) -> Vec<FogVolumeFroxels> {
    corners
        .iter()
        .enumerate()
        .filter_map(|(volume, corners)| {
            let [first, last] = bounds(corners, view_from_world, froxel_volume)?;
            Some(FogVolumeFroxels {
                first,
                volume: volume as u32,
                last,
                padding: 0,
            })
        })
        .take(MOST_VOLUMES)
        .collect()
}

/// The first and last froxels a box with world corners `corners` reaches:
/// its corners' froxels while it lies in front of the camera, where its
/// projection lies within theirs, and otherwise the whole frame up to its
/// farthest corner's slice.
fn bounds(
    corners: &[Vec3; 8],
    view_from_world: Mat4,
    froxel_volume: &FroxelVolumeUniform,
) -> Option<[[u32; 3]; 2]> {
    let points = corners.map(|corner| view_from_world.transform_point3(corner));
    let depths = points.map(|point| -point.z);
    let nearest = depths.into_iter().fold(f32::INFINITY, f32::min);
    let farthest = depths.into_iter().fold(f32::NEG_INFINITY, f32::max);
    if farthest <= 0. || nearest >= froxel_volume.length {
        return None;
    }
    // The slice coordinate of a view depth: shading/fog.wgsl's
    // fog_volume_coordinate, which inverts fog_slice_depth.
    let slice = |depth: f32| {
        (depth / froxel_volume.length)
            .clamp(0., 1.)
            .powf(froxel_volume.detail_spread.recip())
    };
    let (low, high) = if nearest > 0. {
        let [scale_x, scale_y, offset_x, offset_y] = froxel_volume.projection;
        let scale = Vec2::new(scale_x, scale_y);
        let offset = Vec2::new(offset_x, offset_y);
        points.iter().zip(depths).fold(
            (Vec3::INFINITY, Vec3::NEG_INFINITY),
            |(low, high), (point, depth)| {
                // The point's frame position as unit coordinates:
                // stages/fog.wgsl's froxel_view_position and froxel_ndc
                // inverted.
                let ndc = point.truncate() * scale / depth - offset;
                let unit = Vec3::new((ndc.x + 1.) * 0.5, (1. - ndc.y) * 0.5, slice(depth));
                (low.min(unit), high.max(unit))
            },
        )
    } else {
        (Vec3::ZERO, Vec3::new(1., 1., slice(farthest)))
    };
    let size = UVec3::from(froxel_volume.size).as_vec3();
    let first = (low * size - MARGIN).floor();
    let last = (high * size + MARGIN).floor();
    if first.cmpge(size).any() || last.cmplt(Vec3::ZERO).any() {
        return None;
    }
    let froxel = |cell: Vec3| cell.clamp(Vec3::ZERO, size - 1.).as_uvec3().to_array();
    Some([froxel(first), froxel(last)])
}

/// A buffer for `volumes` fog volumes' froxels.
fn buffer(device: &wgpu::Device, volumes: usize) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some("fog volume froxels"),
            size: (volumes.max(1) * size_of::<FogVolumeFroxels>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
    )
}

/// The layouts this module mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    use crate::shading::layout_tests::mirror;
    [mirror!(
        "volumetric_fog",
        "FogVolumeFroxels",
        FogVolumeFroxels,
        [first, volume, last]
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Quat;

    // Defect: the culling drops a box that may reach a froxel or keeps one
    // that cannot, or a box across the camera's plane takes bounds from its
    // corners' projection, which no longer bounds it. The geometry decides:
    // behind the camera, beyond the volume, or wholly in front of the camera
    // and beside the frame, a box reaches no froxel; across the camera's
    // plane it may reach every froxel of the frame up to the slice of its
    // farthest point; in view, it reaches the froxel of its centre.
    #[test]
    fn only_boxes_that_may_reach_a_froxel_are_bounded() {
        let size = crate::stages::fog::froxels(crate::settings::FogQuality::High, [160, 90], 2048);
        let projection = crate::perspective(1., 160. / 90., 0.1);
        let (length, detail_spread) = (20., 2.);
        let froxel_volume = FroxelVolumeUniform {
            projection: [
                projection.x_axis.x,
                projection.y_axis.y,
                projection.z_axis.x,
                projection.z_axis.y,
            ],
            size,
            length,
            detail_spread,
            ..Default::default()
        };
        let turn = Quat::from_rotation_y(0.7);
        let world_from_camera = Mat4::from_rotation_translation(turn, Vec3::new(12., 1.5, -40.));
        // A box about `center` relative to the camera, square to its view.
        let corners = |center: Vec3, size: Vec3| {
            crate::shading::fog::corners(&crate::FogVolume {
                center: world_from_camera.transform_point3(center),
                rotation: turn,
                size,
                density: 1.,
                albedo: [1.; 3],
                edge_fade: 0.,
            })
        };
        let in_view = Vec3::new(0.5, 0.2, -8.);
        let boxes = [
            corners(Vec3::new(0., 0., 6.), Vec3::splat(3.)),
            corners(Vec3::new(-30., 0., -8.), Vec3::splat(2.)),
            corners(Vec3::new(0., 0., -30.), Vec3::splat(4.)),
            // From 6 m behind the camera to 6 m in front of it.
            corners(Vec3::new(1.5, -1., 0.), Vec3::new(1., 1., 12.)),
            corners(in_view, Vec3::splat(2.)),
        ];
        let reached = reached(&boxes, world_from_camera.inverse(), &froxel_volume);
        assert_eq!(
            reached
                .iter()
                .map(|froxels| froxels.volume)
                .collect::<Vec<_>>(),
            [3, 4],
            "{reached:?}"
        );
        // Slice `slice` begins this deep, as the volume spaces its slices.
        let depth = |slice: u32| length * (slice as f32 / size[2] as f32).powf(detail_spread);
        let across = reached[0];
        assert_eq!(across.first, [0; 3], "{across:?}");
        assert_eq!(across.last[..2], [size[0] - 1, size[1] - 1], "{across:?}");
        assert!(
            depth(across.last[2]) <= 6. && 6. < depth(across.last[2] + 1),
            "{across:?} ends at the wrong slice"
        );
        // The froxel of the in-view box's centre, through the projection.
        let ndc = projection.project_point3(in_view);
        let froxel = |unit: f32, side: u32| (unit * side as f32) as u32;
        let centre = [
            froxel((ndc.x + 1.) * 0.5, size[0]),
            froxel((1. - ndc.y) * 0.5, size[1]),
            (0..size[2]).rfind(|&slice| depth(slice) <= 8.).unwrap(),
        ];
        let view = reached[1];
        assert!(
            (0..3).all(|axis| view.first[axis] <= centre[axis] && centre[axis] <= view.last[axis]),
            "{view:?} leaves out its centre's froxel {centre:?}"
        );
        assert!(
            view.first[0] > 0 && view.last[0] < size[0] - 1,
            "{view:?} spans the frame"
        );
    }
}
