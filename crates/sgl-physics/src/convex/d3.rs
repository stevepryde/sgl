//! The 3D convex kernel: GJK distance and the shape cast.
//!
//! Ported from `Box3D` v0.1.0 (commit
//! `8441b4a06d6d09dcfb0b0f704df4d847d1437b92`), `src/distance.c`
//! (`b3GetProxySupport`, `b3ShapeDistance` with `b3SolveSimplex2`,
//! `b3SolveSimplex3`, `b3SolveSimplex4`, the `b3BarycentricCoords`
//! functions and `b3ComputeWitnessPoints`, and `b3ShapeCast`), with the
//! types of `include/box3d/types.h` and `include/box3d/collision.h` and the
//! vector functions of `include/box3d/math_functions.h` and
//! `src/math_internal.h`. Copyright 2026 Erin Catto, MIT; the source credits
//! Dirk Gregorius with portions of the file. `LICENSE-box3d.txt` is beside
//! this crate's manifest. Departures and algorithm notes are in
//! `PROVENANCE.md`.
//!
//! The simplex is built in the Minkowski difference B − A: each vertex is
//! `w = wB − wA`, the opposite of the 2D source, and the search walks
//! toward the origin.

use sgl_core::math::Vec3;

use super::{
    Cast, ClosestPoints, DistanceState, MIN_DIRECTION_LENGTH_SQUARED, cast_by_advancement, is_unit,
    max_float,
};

mod simplex;

use simplex::{Simplex, SimplexVertex};

/// The most points a 3D proxy holds. The source's proxy points at a caller's
/// array of up to `B3_MAX_SHAPE_CAST_POINTS` (64); this one owns its points,
/// and sixteen is the largest hull the crate offers.
pub(crate) const MAX_POINTS: usize = 16;

/// The most support points one distance query takes
/// (`B3_MAX_GJK_ITERATIONS`). Two boxes settle in fewer than ten; the cap is
/// reached only when rounding makes the search cycle without repeating a
/// vertex or losing ground.
pub(crate) const MAX_GJK_ITERATIONS: u32 = 32;

/// A 3D convex shape: up to [`MAX_POINTS`] points and a radius.
pub(crate) type Proxy = super::Proxy<Vec3, MAX_POINTS>;

/// A rotation as the three columns of its matrix (`b3Matrix3`): where it
/// sends the x, y and z axes. The caller supplies it, as a matrix or as a
/// unit quaternion: the kernel calls no trigonometric function.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rot {
    /// Where the rotation sends the x axis.
    pub cx: Vec3,
    /// Where the rotation sends the y axis.
    pub cy: Vec3,
    /// Where the rotation sends the z axis.
    pub cz: Vec3,
}

impl Rot {
    /// No rotation.
    pub(crate) const IDENTITY: Self = Self {
        cx: Vec3::X,
        cy: Vec3::Y,
        cz: Vec3::Z,
    };

    /// The rotation of the unit quaternion `x i + y j + z k + w`
    /// (`b3MakeMatrixFromQuat`).
    pub(crate) fn from_quat(x: f32, y: f32, z: f32, w: f32) -> Self {
        let xx = x * x;
        let yy = y * y;
        let zz = z * z;
        let xy = x * y;
        let xz = x * z;
        let xw = x * w;
        let yz = y * z;
        let yw = y * w;
        let zw = z * w;

        Self {
            cx: Vec3::new(1.0 - 2.0 * (yy + zz), 2.0 * (xy + zw), 2.0 * (xz - yw)),
            cy: Vec3::new(2.0 * (xy - zw), 1.0 - 2.0 * (xx + zz), 2.0 * (yz + xw)),
            cz: Vec3::new(2.0 * (xz + yw), 2.0 * (yz - xw), 1.0 - 2.0 * (xx + yy)),
        }
    }

    /// `v` rotated (`b3MulMV`).
    fn apply(&self, v: Vec3) -> Vec3 {
        Vec3::new(
            self.cx.x * v.x + self.cy.x * v.y + self.cz.x * v.z,
            self.cx.y * v.x + self.cy.y * v.y + self.cz.y * v.z,
            self.cx.z * v.x + self.cy.z * v.y + self.cz.z * v.z,
        )
    }
}

impl Proxy {
    /// The hull of `points`, each rotated by `rotation` and then moved by
    /// `position`, grown by `radius`: the step by which `b3ShapeDistance`
    /// brings shape B into shape A's frame.
    pub(crate) fn offset(points: &[Vec3], radius: f32, position: Vec3, rotation: &Rot) -> Self {
        Self::mapped(points, radius, |point| rotation.apply(point) + position)
    }
}

fn dot(a: Vec3, b: Vec3) -> f32 {
    a.x * b.x + a.y * b.y + a.z * b.z
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    Vec3::new(
        a.y * b.z - a.z * b.y,
        a.z * b.x - a.x * b.z,
        a.x * b.y - a.y * b.x,
    )
}

/// `v` as a unit vector, or zero when it is too short (`b3Normalize`).
fn normalize(v: Vec3) -> Vec3 {
    let length_squared = v.x * v.x + v.y * v.y + v.z * v.z;
    if length_squared > MIN_DIRECTION_LENGTH_SQUARED {
        let s = 1.0 / length_squared.sqrt();
        Vec3::new(s * v.x, s * v.y, s * v.z)
    } else {
        Vec3::ZERO
    }
}

/// The index of the point furthest along `axis`, the first of equals
/// (`b3GetProxySupport`). The points are measured from the first, so the
/// comparison is as precise far from the frame's origin as near it.
fn find_support(points: &[Vec3], axis: Vec3) -> usize {
    let origin = points[0];
    let mut max_index = 0;
    let mut max_projection = 0.0;
    for (index, point) in points.iter().enumerate().skip(1) {
        let projection = dot(axis, *point - origin);
        if projection > max_projection {
            max_index = index;
            max_projection = projection;
        }
    }
    max_index
}

/// The direction from the solved simplex toward the origin: from a point,
/// straight back; from a segment, perpendicular to it in the plane it
/// makes with the origin; from a triangle, its normal on the origin's side.
fn search_direction(s: &Simplex) -> Vec3 {
    let v = &s.v;
    match s.count {
        1 => -v[0].w,
        2 => {
            let a = v[0].w;
            let ab = v[1].w - a;
            cross(cross(ab, -a), ab)
        }
        _ => {
            let a = v[0].w;
            let n = cross(v[1].w - a, v[2].w - a);
            if dot(n, a) < 0.0 { n } else { -n }
        }
    }
}

/// A result for cores that overlap.
fn overlapped(point_a: Vec3, point_b: Vec3, iterations: u32) -> ClosestPoints<Vec3> {
    ClosestPoints {
        state: DistanceState::Overlapped,
        point_a,
        point_b,
        normal: Vec3::ZERO,
        distance: 0.0,
        iterations,
    }
}

/// The closest points between two shapes in one frame, their distance and
/// the unit normal from `a` toward `b` (`b3ShapeDistance`).
///
/// With `use_radii` the points lie on the shapes' surfaces and the distance
/// is between the surfaces, stopping at zero. Without it they lie on the
/// cores and the distance is between the cores, so a caller that wants a
/// signed separation subtracts the radii itself. Cores that overlap give
/// [`DistanceState::Overlapped`], zero distance and no normal either way.
///
/// GJK grows a simplex in the Minkowski difference toward the origin: solve
/// the simplex for its part nearest the origin, look for a support point
/// further that way, and stop when the support point is one the simplex
/// already had or the distance stops shrinking. Four vertices that hold the
/// origin mean overlap.
pub(crate) fn distance(a: &Proxy, b: &Proxy, use_radii: bool) -> ClosestPoints<Vec3> {
    gjk(a, b, use_radii, MAX_GJK_ITERATIONS)
}

/// [`distance`] with its iteration cap as an argument: it is
/// [`MAX_GJK_ITERATIONS`] for every caller but the test of the cap itself.
fn gjk(a: &Proxy, b: &Proxy, use_radii: bool, max_iterations: u32) -> ClosestPoints<Vec3> {
    let points_a = a.points();
    let points_b = b.points();

    let mut simplex = Simplex::new(SimplexVertex::new(points_a, points_b, 0, 0));
    // The last simplex solved and found nearer the origin: the source's
    // backup, taken before each new vertex is added.
    let mut accepted: Option<Simplex> = None;

    let mut distance_squared = f32::MAX;
    let mut normal = Vec3::ZERO;

    let mut iterations = 0;
    let mut cap_reached = true;
    while iterations < max_iterations {
        if !simplex.solve() {
            // No progress: answer from the last simplex.
            cap_reached = false;
            break;
        }

        if simplex.count == 4 {
            // The origin is inside the tetrahedron.
            let (point_a, point_b) = simplex.witness_points();
            return overlapped(point_a, point_b, iterations);
        }

        // The distance must shrink.
        let old_distance_squared = distance_squared;
        let closest_point = simplex.closest_point();
        distance_squared = dot(closest_point, closest_point);
        if distance_squared >= old_distance_squared {
            // No progress: answer from the last simplex.
            cap_reached = false;
            break;
        }

        let direction = search_direction(&simplex);
        if dot(direction, direction) < MIN_DIRECTION_LENGTH_SQUARED {
            // The origin is probably on a segment or a triangle, so the
            // cores overlap.
            let (point_a, point_b) = simplex.witness_points();
            return overlapped(point_a, point_b, iterations);
        }

        normal = -direction;

        // New support points.
        let index_a = find_support(points_a, -direction);
        let index_b = find_support(points_b, direction);

        // Keep this simplex: adding the vertex can fail by cycling.
        accepted = Some(simplex);

        // A duplicate support point is the main termination criterion.
        if simplex.v[..simplex.count]
            .iter()
            .any(|vertex| vertex.index_a == index_a && vertex.index_b == index_b)
        {
            cap_reached = false;
            break;
        }

        simplex.push(SimplexVertex::new(points_a, points_b, index_a, index_b));
        iterations += 1;
    }

    // However the search ended, the working simplex may hold a vertex that
    // was never solved (at the cap it always does), so the answer comes
    // from the last one accepted. With none, the first distance was not a
    // number below `f32::MAX`: the source treats that as overlap.
    let Some(simplex) = accepted else {
        return overlapped(Vec3::ZERO, Vec3::ZERO, iterations);
    };

    let normal = normalize(normal);
    if !is_unit(normal) {
        // Treat as overlap.
        return overlapped(Vec3::ZERO, Vec3::ZERO, iterations);
    }

    let (mut point_a, mut point_b) = simplex.witness_points();
    let between = point_b - point_a;
    let mut distance = dot(between, between).sqrt();

    if use_radii {
        distance = max_float(0.0, distance - a.radius() - b.radius());

        // The points stay on the perimeters even when the shapes overlap,
        // so they move smoothly.
        point_a += normal * a.radius();
        point_b -= normal * b.radius();
    }

    ClosestPoints {
        state: if cap_reached {
            DistanceState::IterationCap
        } else {
            DistanceState::Separated
        },
        point_a,
        point_b,
        normal,
        distance,
        iterations,
    }
}

/// Where shape `b`, moved by `translation_b`, first meets the fixed shape
/// `a`, both in one frame (`b3ShapeCast`). See
/// [`cast_by_advancement`] for the parameters.
pub(crate) fn shape_cast(
    a: &Proxy,
    b: &Proxy,
    translation_b: Vec3,
    max_fraction: f32,
    can_encroach: bool,
    slop: f32,
) -> Cast<Vec3> {
    cast_by_advancement(
        a,
        b,
        translation_b,
        max_fraction,
        can_encroach,
        slop,
        |a, b| distance(a, b, false),
    )
}

#[cfg(test)]
mod tests;
