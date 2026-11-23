//! A planar reflector's description (the architecture's Scene content,
//! Planar reflector).
use glam::Vec3;

/// A plane that mirrors the scene: a mirror's, a polished floor's, still
/// water's. An opaque or masked material names it
/// (`SurfaceMaterial::planar_reflector`) and its surfaces, which lie in the
/// plane, then show the scene mirrored in it, moving objects and what lies
/// outside the viewer's frustum included. Each view that shows one renders
/// the scene again from the mirrored eye, at the level
/// `Settings::planar_reflections` chooses, so a reflector costs a view only
/// in the frames it is in that view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlanarReflector {
    /// A point on the plane, in the scene's render frame.
    pub origin: Vec3,
    /// The plane's normal, toward the side that reflects; any nonzero
    /// length. A viewer behind the plane sees no reflection.
    pub normal: Vec3,
}

impl PlanarReflector {
    /// The reflector through `origin` that reflects toward `normal`.
    pub fn new(origin: Vec3, normal: Vec3) -> Self {
        Self { origin, normal }
    }

    /// The plane as (unit normal, d), `dot(normal, p) + d` a point's signed
    /// distance from it, positive on the reflecting side; none where the
    /// origin is not finite or the normal has no direction.
    pub(crate) fn plane(&self) -> Option<glam::Vec4> {
        let normal = self.normal.try_normalize()?;
        (self.origin.is_finite() && normal.is_finite())
            .then(|| normal.extend(-normal.dot(self.origin)))
    }
}
