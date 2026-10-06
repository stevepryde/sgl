//! What a frame's passes take from the scene and the camera beside the
//! volume's placement: the moving instances' bounds about its probes, the
//! rotation of every probe's rays and the camera's frustum.
use super::buffers::{BoundsUniform, MOST_MOVING_BOUNDS};
use crate::Scene;
use crate::content::dynamic_gi::DynamicGiVolume;
use glam::{Mat3, Mat4, Vec3, Vec4};

/// The world bounds of `scene`'s drawn moving instances that reach into the
/// cells of `placement`'s probes, the nearest `eye` first, at most
/// `MOST_MOVING_BOUNDS`: the probes about them trace as active ones do.
pub(super) fn moving_bounds(
    scene: &Scene,
    placement: &DynamicGiVolume,
    eye: Vec3,
) -> Vec<BoundsUniform> {
    let extent = [
        placement.origin - placement.spacing,
        placement.end() + placement.spacing,
    ];
    let mut bounds: Vec<(f32, [Vec3; 2])> = scene
        .instances
        .slots
        .iter()
        .filter(|(_, instance)| {
            instance.mobility == crate::Mobility::Moving && instance.state.visible
        })
        .map(|(_, instance)| {
            let model = scene.drawn_model(instance.state.model);
            crate::scene::static_edits::posed_bounds(instance.bounds(model), instance.state.pose)
        })
        .filter(|[min, max]| min.cmple(extent[1]).all() && max.cmpge(extent[0]).all())
        .map(|[min, max]| ((min + max).distance_squared(eye * 2.), [min, max]))
        .collect();
    bounds.sort_by(|a, b| a.0.total_cmp(&b.0));
    bounds
        .into_iter()
        .take(MOST_MOVING_BOUNDS as usize)
        .map(|(_, [min, max])| BoundsUniform {
            min: min.to_array(),
            padding_min: 0.,
            max: max.to_array(),
            padding_max: 0.,
        })
        .collect()
}

/// Thomas Wang's hash, as Wicked's RNG seeds through it.
fn hash(seed: u32) -> u32 {
    let mut seed = (seed ^ 61) ^ (seed >> 16);
    seed = seed.wrapping_mul(9);
    seed ^= seed >> 4;
    seed = seed.wrapping_mul(0x27d4_eb2d);
    seed ^ (seed >> 15)
}

/// Frame `frame`'s rotation of every probe's rays: a random angle about a
/// random axis, as `wiRenderer::DDGI` draws its `g_xTransform` each frame
/// (df44c3d wiRenderer.cpp 12520–12528).
pub(super) fn rotation(frame: u32) -> Mat3 {
    let random = |index: u32| hash(frame.wrapping_mul(4).wrapping_add(index)) as f32 / 4294967296.;
    let angle = random(0) * std::f32::consts::TAU;
    let axis = Vec3::new(random(1), random(2), random(3)) * 2. - 1.;
    Mat3::from_axis_angle(axis.try_normalize().unwrap_or(Vec3::Y), angle)
}

/// The planes of `clip_from_world`'s clip volume, each normalised with the
/// inside where `dot(xyz, p) + w >= 0`; a plane at infinity, such as
/// `perspective`'s far plane, takes everything.
pub(super) fn frustum(clip_from_world: Mat4) -> [[f32; 4]; 6] {
    let row = |index| clip_from_world.row(index);
    [
        row(3) + row(0),
        row(3) - row(0),
        row(3) + row(1),
        row(3) - row(1),
        row(2),
        row(3) - row(2),
    ]
    .map(|plane: Vec4| {
        let length = plane.truncate().length();
        if length > 1e-12 && length.is_finite() {
            (plane / length).to_array()
        } else {
            [0., 0., 0., 1.]
        }
    })
}
