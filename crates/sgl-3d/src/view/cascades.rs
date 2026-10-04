//! A directional light's shadow cascades: the camera's view depth split into
//! slices, each covered by one orthographic shadow view.
//!
//! Ports Bevy 9d12036 `crates/bevy_light/src/cascade.rs`
//! (`calculate_cascade_bounds`, the near bounds of
//! `build_directional_light_cascades`, and `calculate_cascade`), MIT OR
//! Apache-2.0 (`src/LICENSE-bevy.txt`). A cascade keeps a constant diameter,
//! the larger of its slice's body and far-plane diagonals rounded up to whole
//! metres, and a centre snapped to whole texels in light space, so the
//! shadow of a still scene does not shimmer while the camera moves or turns.
//! Changes: the slice corners unproject the camera's projection matrix,
//! which also covers off-centre and orthographic cameras (Bevy takes them
//! from its projection types); the first cascade starts at the camera's near
//! plane, as Godot's `_light_instance_setup_directional_shadow` starts it
//! (Bevy's `minimum_distance`); the light's rotation is built from its
//! direction; a probe capture's cascades are cubes about its centre,
//! what its six faces see out to each far bound; and each cascade's near
//! plane lies the shadow's `pancake_size` toward the light beyond the
//! slice's bounds, as Godot b130438's pancake puts it beyond the slice's
//! bounding sphere (`servers/rendering/renderer_scene_cull.cpp`
//! `_light_instance_setup_directional_shadow`: `z_max = z_vec.dot(center) +
//! radius + pancake_size`), MIT (`src/LICENSE-godot.txt`), so a caster within
//! that margin keeps its own depth and only one beyond it is clamped to the
//! near plane.
use crate::content::lighting::DirectionalShadow;
use glam::camera;
use glam::{Mat4, Vec3, Vec4};

/// The most cascades a shadow has.
pub(crate) const MAX_SHADOW_CASCADES: usize = 4;
/// Bevy's default `overlap_proportion`: each cascade starts this share of
/// the previous cascade's far bound before that bound, and shading blends the
/// two across the overlap. directional_shadow.wgsl's SHADOW_CASCADE_OVERLAP
/// is its twin.
pub(crate) const SHADOW_CASCADE_OVERLAP: f32 = 0.2;

/// One cascade's shadow view.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Cascade {
    /// Reversed-Z orthographic view-projection.
    pub clip_from_world: Mat4,
    /// World metres per shadow-map texel.
    pub texel_size: f32,
    /// The camera's view depth where the cascade ends; a probe capture's
    /// cascade ends at this distance from its centre along each axis.
    pub far_bound: f32,
}

/// A shadow's cascades, nearest first.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Cascades {
    list: [Cascade; MAX_SHADOW_CASCADES],
    count: usize,
}

impl Cascades {
    pub fn as_slice(&self) -> &[Cascade] {
        &self.list[..self.count]
    }

    fn from_bounds(bounds: &[f32], mut fit: impl FnMut(usize, f32) -> Cascade) -> Self {
        let mut cascades = Self::default();
        for (index, &far_bound) in bounds.iter().enumerate() {
            cascades.list[index] = fit(index, far_bound);
        }
        cascades.count = bounds.len();
        cascades
    }

    /// `shadow`'s cascades of a light shining along `direction`, seen by a
    /// camera with `view` and `projection`, in maps of `map_size` texels.
    /// None when the shadow reaches no farther than the camera's near plane.
    pub fn camera(
        view: Mat4,
        projection: Mat4,
        direction: Vec3,
        shadow: &DirectionalShadow,
        map_size: u32,
    ) -> Option<Self> {
        let near = camera_near(projection);
        let bounds = cascade_bounds(shadow, near)?;
        let world_from_light = world_from_light(direction);
        let light_from_camera = world_from_light.transpose() * view.inverse();
        let overlap_factor = 1. - SHADOW_CASCADE_OVERLAP;
        let pancake = pancake_size(shadow);
        Some(Self::from_bounds(bounds.as_slice(), |index, far_bound| {
            let near_bound = if index == 0 {
                near
            } else {
                overlap_factor * bounds.as_slice()[index - 1]
            };
            let corners = slice_corners(projection, near_bound, far_bound);
            calculate_cascade(
                corners,
                map_size as f32,
                world_from_light,
                light_from_camera,
                far_bound,
                pancake,
            )
        }))
    }

    /// `shadow`'s cascades of a light shining along `direction` for a probe
    /// capture at `center`: cascade `i` covers the cube that reaches its far
    /// bound from `center` along each axis, which the capture's six faces see
    /// out to that view depth.
    pub fn capture(
        center: Vec3,
        direction: Vec3,
        shadow: &DirectionalShadow,
        map_size: u32,
    ) -> Option<Self> {
        let bounds = cascade_bounds(shadow, 0.)?;
        let world_from_light = world_from_light(direction);
        let light_from_capture = world_from_light.transpose() * Mat4::from_translation(center);
        let pancake = pancake_size(shadow);
        Some(Self::from_bounds(bounds.as_slice(), |_, far_bound| {
            let f = far_bound;
            let corners = [
                Vec3::new(f, -f, f),
                Vec3::new(f, f, f),
                Vec3::new(-f, f, f),
                Vec3::new(-f, -f, f),
                Vec3::new(f, -f, -f),
                Vec3::new(f, f, -f),
                Vec3::new(-f, f, -f),
                Vec3::new(-f, -f, -f),
            ];
            calculate_cascade(
                corners,
                map_size as f32,
                world_from_light,
                light_from_capture,
                far_bound,
                pancake,
            )
        }))
    }
}

/// Up to four far bounds.
struct Bounds {
    list: [f32; MAX_SHADOW_CASCADES],
    count: usize,
}

impl Bounds {
    fn as_slice(&self) -> &[f32] {
        &self.list[..self.count]
    }
}

/// `shadow`'s far bounds beyond a first cascade starting at `near`: Bevy's
/// `calculate_cascade_bounds`, geometric from `first_split` to `distance`.
/// A `first_split` that is not finite or not beyond `near` gives one
/// cascade over the whole distance, as Bevy's builder refuses it.
fn cascade_bounds(shadow: &DirectionalShadow, near: f32) -> Option<Bounds> {
    let distance = shadow.distance;
    if !distance.is_finite() || distance <= near {
        return None;
    }
    let first = shadow.first_split;
    let split = first.is_finite() && first > near;
    let count = if split {
        shadow.cascades.clamp(1, MAX_SHADOW_CASCADES as u32) as usize
    } else {
        1
    };
    let mut bounds = Bounds {
        list: [distance; MAX_SHADOW_CASCADES],
        count,
    };
    if count > 1 {
        let first = first.min(distance);
        let base = (distance / first).powf(1. / (count - 1) as f32);
        for (index, bound) in bounds.list[..count].iter_mut().enumerate() {
            *bound = first * base.powf(index as f32);
        }
    }
    Some(bounds)
}

/// `shadow`'s pancake size: nonnegative and finite, else 0.
fn pancake_size(shadow: &DirectionalShadow) -> f32 {
    let size = shadow.pancake_size;
    if size.is_finite() { size.max(0.) } else { 0. }
}

/// The view depth of the camera's near plane: device depth 1.
fn camera_near(projection: Mat4) -> f32 {
    let near = projection.inverse() * Vec4::new(0., 0., 1., 1.);
    (-near.z / near.w).max(0.)
}

/// A light's rotation, light space to world, with its forward (-Z) along
/// `direction`: orthogonal with no translation, as Bevy requires for stable
/// cascades. Bevy takes the light entity's rotation; a `DirectionalLight` is
/// a direction alone, so this looks along it with +Y up, or +X within about
/// 8° of vertical, where +Y would make no basis. Only the texel grid's
/// orientation about the light's axis depends on that choice, and it holds
/// still while the direction does.
fn world_from_light(direction: Vec3) -> Mat4 {
    let forward = direction.normalize();
    let up = if forward.dot(Vec3::Y).abs() > 0.99 {
        Vec3::X
    } else {
        Vec3::Y
    };
    camera::rh::view::look_to_mat4(Vec3::ZERO, forward, up).transpose()
}

/// The corners of what a camera with `projection` sees between view depths
/// `near` and `far`, in its view space, in `calculate_cascade`'s order (Bevy's
/// `get_frustum_corners`): bottom right, top right, top left and bottom left
/// at `near`, then the same at `far`.
fn slice_corners(projection: Mat4, near: f32, far: f32) -> [Vec3; 8] {
    let inverse = projection.inverse();
    let at = |depth: f32| {
        let clip = projection * Vec4::new(0., 0., -depth, 1.);
        let device_depth = clip.z / clip.w;
        [(1., -1.), (1., 1.), (-1., 1.), (-1., -1.)].map(|(x, y)| {
            let view = inverse * Vec4::new(x, y, device_depth, 1.);
            view.truncate() / view.w
        })
    };
    let [a, b, c, d] = at(near);
    let [e, f, g, h] = at(far);
    [a, b, c, d, e, f, g, h]
}

/// Bevy's `calculate_cascade`: the shadow view of the frustum slice with
/// `frustum_corners` (in the camera's view space, `calculate_cascade`'s
/// order), for maps of `cascade_texture_size` texels, with its near plane
/// `pancake_size` metres toward the light beyond the slice (Godot's
/// pancake).
fn calculate_cascade(
    frustum_corners: [Vec3; 8],
    cascade_texture_size: f32,
    world_from_light: Mat4,
    light_from_camera: Mat4,
    far_bound: f32,
    pancake_size: f32,
) -> Cascade {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for corner_camera_view in frustum_corners {
        let corner_light_view = light_from_camera.transform_point3(corner_camera_view);
        min = min.min(corner_light_view);
        max = max.max(corner_light_view);
    }

    // NOTE: Use the larger of the frustum slice far plane diagonal and body diagonal lengths as this
    //       will be the maximum possible projection size. Use the ceiling to get an integer which is
    //       very important for floating point stability later. It is also important that these are
    //       calculated using the original camera space corner positions for floating point precision
    //       as even though the lengths using corner_light_view above should be the same, precision can
    //       introduce small but significant differences.
    // NOTE: The size remains the same unless the view frustum or cascade configuration is modified.
    let body_diagonal = (frustum_corners[0] - frustum_corners[6]).length_squared();
    let far_plane_diagonal = (frustum_corners[4] - frustum_corners[6]).length_squared();
    let cascade_diameter = body_diagonal.max(far_plane_diagonal).sqrt().ceil();

    // NOTE: If we ensure that cascade_texture_size is a power of 2, then as we made cascade_diameter an
    //       integer, cascade_texel_size is then an integer multiple of a power of 2 and can be
    //       exactly represented in a floating point value.
    let cascade_texel_size = cascade_diameter / cascade_texture_size;
    // SGL3D: the near plane, moved toward the light by Godot's pancake.
    let near_plane = max.z + pancake_size;
    // NOTE: For shadow stability it is very important that the near_plane_center is at integer
    //       multiples of the texel size to be exactly representable in a floating point value.
    let near_plane_center = Vec3::new(
        (0.5 * (min.x + max.x) / cascade_texel_size).floor() * cascade_texel_size,
        (0.5 * (min.y + max.y) / cascade_texel_size).floor() * cascade_texel_size,
        // NOTE: max.z is the near plane for right-handed y-up
        near_plane,
    );

    // It is critical for `cascade_from_world` to be stable. So rather than forming `world_from_cascade`
    // and inverting it, which risks instability due to numerical precision, we directly form
    // `cascade_from_world` as the reference material suggests.
    let world_from_light_transpose = world_from_light.transpose();
    let cascade_from_world = Mat4::from_cols(
        world_from_light_transpose.x_axis,
        world_from_light_transpose.y_axis,
        world_from_light_transpose.z_axis,
        (-near_plane_center).extend(1.0),
    );

    // Right-handed orthographic projection, centered at `near_plane_center`.
    // NOTE: This is different from the reference material, as we use reverse Z.
    let r = (near_plane - min.z).recip();
    let clip_from_cascade = Mat4::from_cols(
        Vec4::new(2.0 / cascade_diameter, 0.0, 0.0, 0.0),
        Vec4::new(0.0, 2.0 / cascade_diameter, 0.0, 0.0),
        Vec4::new(0.0, 0.0, r, 0.0),
        Vec4::new(0.0, 0.0, 1.0, 1.0),
    );

    Cascade {
        clip_from_world: clip_from_cascade * cascade_from_world,
        texel_size: cascade_texel_size,
        far_bound,
    }
}

#[cfg(test)]
mod tests;
