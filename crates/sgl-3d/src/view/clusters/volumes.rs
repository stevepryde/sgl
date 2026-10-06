//! Culling a light or decal by its range: a sphere against a view's clip
//! volume without a far plane, as the camera's cluster assignment culls
//! them, or against a box. Bevy 9d12036's `HalfSpace`,
//! `ViewFrustum::from_clip_from_world_no_far` and
//! `Frustum::intersects_sphere`, which `assign` ports with its assignment,
//! MIT OR Apache-2.0 (`src/LICENSE-bevy.txt`).
use crate::content::decal::Decal;
use crate::content::light::Light;
use glam::{Mat4, Vec3, Vec4, Vec4Swizzles};

/// The sphere that bounds `decal`'s box.
pub(super) fn decal_sphere(decal: &Decal) -> Sphere {
    Sphere {
        center: decal.position,
        radius: 0.5 * decal.size.length(),
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Sphere {
    pub center: Vec3,
    pub radius: f32,
}

/// Bevy's `HalfSpace`: a plane through `normal_d`, normalised; a point `p`
/// is inside where `normal · p + d > 0`.
#[derive(Clone, Copy)]
pub(super) struct HalfSpace(pub Vec4);

impl HalfSpace {
    pub fn new(normal_d: Vec4) -> Self {
        Self(normal_d * normal_d.xyz().length_recip())
    }
    pub fn normal(&self) -> Vec3 {
        self.0.xyz()
    }
    pub fn d(&self) -> f32 {
        self.0.w
    }
}

/// The view's clip volume without a far plane (Bevy's
/// `ViewFrustum::from_clip_from_world_no_far`): left, right, bottom, top and
/// the reversed-Z near plane.
pub(super) fn frustum(clip_from_world: Mat4) -> [HalfSpace; 5] {
    let row = |i| clip_from_world.row(i);
    let row3 = row(3);
    [
        HalfSpace::new(row3 + row(0)),
        HalfSpace::new(row3 - row(0)),
        HalfSpace::new(row3 + row(1)),
        HalfSpace::new(row3 - row(1)),
        HalfSpace::new(row3 - row(2)),
    ]
}

/// `Frustum::intersects_sphere`.
pub(super) fn intersects_sphere(frustum: &[HalfSpace; 5], sphere: &Sphere) -> bool {
    let center = sphere.center.extend(1.);
    frustum
        .iter()
        .all(|half_space| half_space.0.dot(center) + sphere.radius > 0.)
}

/// A view's clip volume without a far plane, for culling lights by their
/// range, as the camera's assignment culls them and Wicked Engine culls the
/// frame's light list.
pub(crate) struct ViewVolume([HalfSpace; 5]);

impl ViewVolume {
    pub fn new(clip_from_world: Mat4) -> Self {
        Self(frustum(clip_from_world))
    }

    /// Whether `light`'s range reaches into the volume.
    pub fn reaches(&self, light: &Light) -> bool {
        intersects_sphere(
            &self.0,
            &Sphere {
                center: light.position,
                radius: light.range,
            },
        )
    }

    /// Whether the sphere about `decal`'s box reaches into the volume.
    pub fn reaches_decal(&self, decal: &Decal) -> bool {
        intersects_sphere(&self.0, &decal_sphere(decal))
    }
}

/// A box, for culling lights by their range as the dynamic GI volume's list
/// culls them against its extent: a sphere reaches it where its centre lies
/// within its radius of the box's nearest point.
pub(crate) struct BoxVolume {
    pub min: Vec3,
    pub max: Vec3,
}

impl BoxVolume {
    fn reaches_sphere(&self, sphere: &Sphere) -> bool {
        let nearest = sphere.center.clamp(self.min, self.max);
        nearest.distance_squared(sphere.center) <= sphere.radius * sphere.radius
    }

    /// Whether `light`'s range reaches into the box.
    pub fn reaches(&self, light: &Light) -> bool {
        self.reaches_sphere(&Sphere {
            center: light.position,
            radius: light.range,
        })
    }

    /// Whether the sphere about `decal`'s box reaches into the box.
    pub fn reaches_decal(&self, decal: &Decal) -> bool {
        self.reaches_sphere(&decal_sphere(decal))
    }
}
