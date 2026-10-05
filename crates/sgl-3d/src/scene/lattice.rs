//! Where a position lies on a volume's lattice: the one rule by which the
//! dynamic GI volume and the irradiance volume tell a scroll from another
//! placement, and the irradiance volume places a region it is written.

use glam::{DVec3, I64Vec3, Vec3};

/// How far, in steps, a position may lie from the lattice and still name a
/// place on it, beyond the rounding of its `f32` magnitude.
const TOLERANCE: f64 = 1e-3;

/// The whole steps of `step` from a lattice's `origin` to `at`, both in the
/// frame the scene was created in, where the game gave `at` as `given` in
/// its render frame; `None` where `at` lies off the lattice beyond its
/// tolerance.
pub(crate) fn steps(origin: DVec3, step: Vec3, at: DVec3, given: Vec3) -> Option<I64Vec3> {
    let step = step.as_dvec3();
    let steps = (at - origin) / step;
    let nearest = steps.round();
    let rounding = f64::from(given.abs().max_element() * f32::EPSILON) / step;
    let tolerance = rounding + TOLERANCE;
    (steps - nearest)
        .abs()
        .cmple(tolerance)
        .all()
        .then(|| nearest.as_i64vec3())
}
