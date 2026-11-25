//! The 2D convex kernel: GJK distance and the shape cast.
//!
//! Ported from `Box2D` v3.2.0 (commit
//! `178495ba35695aefe9c321e569d2277f06db3b90`), `src/distance.c`
//! (`b2MakeProxy`, `b2MakeOffsetProxy`, `b2ComputeShapeDistance` with
//! `b2SolveSimplex2`, `b2SolveSimplex3`, `b2FindSupport` and
//! `b2ComputeWitnessPoints`, and `b2CastShape`), with the types of
//! `include/box2d/collision.h` and the vector functions of
//! `include/box2d/math_functions.h`. Copyright 2022 Erin Catto, MIT:
//! `LICENSE-box2d.txt` beside this crate's manifest. Departures and
//! algorithm notes are in `PROVENANCE.md`.
//!
//! The simplex is built in the Minkowski difference A − B: each vertex is
//! `w = wA − wB`, and the search walks toward the origin. The source's
//! header comment gives `wB − wA`; its code, followed here, does not.

use sgl_core::math::Vec2;

use super::{
    Cast, ClosestPoints, DistanceState, MIN_DIRECTION_LENGTH_SQUARED, cast_by_advancement,
    max_float,
};

mod toi;

// Unused until the mover of #921 calls it (the kernel is wired in by #919).
#[allow(unused_imports)]
pub(crate) use toi::{
    MAX_PUSH_BACK_ITERATIONS, MAX_ROOT_ITERATIONS, MAX_TOI_ITERATIONS, Sweep, Toi, ToiState,
    time_of_impact,
};

/// The most points a 2D proxy holds (`B2_MAX_POLYGON_VERTICES`).
pub(crate) const MAX_POINTS: usize = 8;

/// The most support points one distance query takes (`maxIterations` in
/// `b2ComputeShapeDistance`). Two eight-point polygons settle in fewer than
/// ten; the cap is reached only when rounding makes the search cycle
/// without repeating a vertex.
pub(crate) const MAX_GJK_ITERATIONS: u32 = 20;

/// A 2D convex shape: up to [`MAX_POINTS`] points and a radius.
pub(crate) type Proxy = super::Proxy<Vec2, MAX_POINTS>;

/// A rotation as the cosine and sine of its angle (`b2Rot`). The caller
/// supplies both: the kernel calls no trigonometric function.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rot {
    /// Cosine of the angle.
    pub c: f32,
    /// Sine of the angle.
    pub s: f32,
}

impl Rot {
    /// No rotation.
    pub(crate) const IDENTITY: Self = Self { c: 1.0, s: 0.0 };
}

impl Proxy {
    /// The hull of `points`, each rotated by `rotation` and then moved by
    /// `position`, grown by `radius` (`b2MakeOffsetProxy`).
    pub(crate) fn offset(points: &[Vec2], radius: f32, position: Vec2, rotation: Rot) -> Self {
        Self::mapped(points, radius, |point| {
            transform_point(position, rotation, point)
        })
    }
}

fn dot(a: Vec2, b: Vec2) -> f32 {
    a.x * b.x + a.y * b.y
}

fn cross(a: Vec2, b: Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

/// The cross product of a scalar and a vector (`b2CrossSV`): `v` turned a
/// quarter turn counter-clockwise and scaled by `s`.
fn cross_sv(s: f32, v: Vec2) -> Vec2 {
    Vec2::new(-s * v.y, s * v.x)
}

/// `v` as a unit vector, or zero when it is too short (`b2Normalize`).
fn normalize(v: Vec2) -> Vec2 {
    let length_squared = v.x * v.x + v.y * v.y;
    if length_squared > MIN_DIRECTION_LENGTH_SQUARED {
        let s = 1.0 / length_squared.sqrt();
        Vec2::new(s * v.x, s * v.y)
    } else {
        Vec2::ZERO
    }
}

fn rotate(q: Rot, v: Vec2) -> Vec2 {
    Vec2::new(q.c * v.x - q.s * v.y, q.s * v.x + q.c * v.y)
}

fn inv_rotate(q: Rot, v: Vec2) -> Vec2 {
    Vec2::new(q.c * v.x + q.s * v.y, -q.s * v.x + q.c * v.y)
}

/// `point` rotated by `rotation`, then moved by `position`
/// (`b2TransformPoint`).
fn transform_point(position: Vec2, rotation: Rot, point: Vec2) -> Vec2 {
    Vec2::new(
        (rotation.c * point.x - rotation.s * point.y) + position.x,
        (rotation.s * point.x + rotation.c * point.y) + position.y,
    )
}

fn weight2(a1: f32, w1: Vec2, a2: f32, w2: Vec2) -> Vec2 {
    Vec2::new(a1 * w1.x + a2 * w2.x, a1 * w1.y + a2 * w2.y)
}

fn weight3(a1: f32, w1: Vec2, a2: f32, w2: Vec2, a3: f32, w3: Vec2) -> Vec2 {
    Vec2::new(
        a1 * w1.x + a2 * w2.x + a3 * w3.x,
        a1 * w1.y + a2 * w2.y + a3 * w3.y,
    )
}

/// The index of the point furthest along `direction`, the first of equals
/// (`b2FindSupport`).
fn find_support(points: &[Vec2], direction: Vec2) -> usize {
    let mut best_index = 0;
    let mut best_value = dot(points[0], direction);
    for (index, point) in points.iter().enumerate().skip(1) {
        let value = dot(*point, direction);
        if value > best_value {
            best_index = index;
            best_value = value;
        }
    }
    best_index
}

/// One vertex of the simplex (`b2SimplexVertex`).
#[derive(Clone, Copy)]
struct SimplexVertex {
    /// Support point on A.
    w_a: Vec2,
    /// Support point on B.
    w_b: Vec2,
    /// `w_a - w_b`.
    w: Vec2,
    /// Barycentric weight of this vertex in the closest point.
    a: f32,
    index_a: usize,
    index_b: usize,
}

impl SimplexVertex {
    fn new(a: &[Vec2], b: &[Vec2], index_a: usize, index_b: usize) -> Self {
        Self {
            w_a: a[index_a],
            w_b: b[index_b],
            w: a[index_a] - b[index_b],
            a: 1.0,
            index_a,
            index_b,
        }
    }
}

/// One to three vertices of the Minkowski difference (`b2Simplex`).
struct Simplex {
    v: [SimplexVertex; 3],
    count: usize,
}

/// Which points of each proxy the final simplex used: what the source's
/// simplex cache records. The time of impact reads its separating axis
/// from it; nothing is warm started from it.
pub(super) struct Features {
    pub count: usize,
    pub index_a: [usize; 3],
    pub index_b: [usize; 3],
}

impl Features {
    fn of(simplex: &Simplex) -> Self {
        let mut out = Self {
            count: simplex.count,
            index_a: [0; 3],
            index_b: [0; 3],
        };
        for (slot, vertex) in simplex.v[..simplex.count].iter().enumerate() {
            out.index_a[slot] = vertex.index_a;
            out.index_b[slot] = vertex.index_b;
        }
        out
    }
}

/// The closest points on A and B for a solved simplex
/// (`b2ComputeWitnessPoints`). A full simplex holds the origin, so the two
/// are one point.
fn witness_points(s: &Simplex) -> (Vec2, Vec2) {
    let v = &s.v;
    match s.count {
        1 => (v[0].w_a, v[0].w_b),
        2 => (
            weight2(v[0].a, v[0].w_a, v[1].a, v[1].w_a),
            weight2(v[0].a, v[0].w_b, v[1].a, v[1].w_b),
        ),
        _ => {
            let a = weight3(v[0].a, v[0].w_a, v[1].a, v[1].w_a, v[2].a, v[2].w_a);
            (a, a)
        }
    }
}

/// Solves a segment (`b2SolveSimplex2`): keeps the part of it nearest the
/// origin, weighs its vertices, and returns a direction toward the origin.
///
/// The closest point `a1 w1 + a2 w2` (`a1 + a2 = 1`) is where the line to
/// the origin is perpendicular to the segment, which gives, with
/// `e12 = w2 - w1`, `a1 = w2 · e12 / |e12|²` and `a2 = -w1 · e12 / |e12|²`.
/// A weight that would be negative means the origin lies beyond that end.
fn solve_simplex2(s: &mut Simplex) -> Vec2 {
    let w1 = s.v[0].w;
    let w2 = s.v[1].w;
    let e12 = w2 - w1;

    // w1 region
    let d12_2 = -dot(w1, e12);
    if d12_2 <= 0.0 {
        s.v[0].a = 1.0;
        s.count = 1;
        return -w1;
    }

    // w2 region
    let d12_1 = dot(w2, e12);
    if d12_1 <= 0.0 {
        s.v[1].a = 1.0;
        s.count = 1;
        s.v[0] = s.v[1];
        return -w2;
    }

    // Must be in the e12 region.
    let inv_d12 = 1.0 / (d12_1 + d12_2);
    s.v[0].a = d12_1 * inv_d12;
    s.v[1].a = d12_2 * inv_d12;
    s.count = 2;
    cross_sv(cross(w1 + w2, e12), e12)
}

/// Solves a triangle (`b2SolveSimplex3`): the vertex, edge or interior
/// region of it that holds the point nearest the origin, by the signs of
/// the edge and triangle weights. The interior means the origin is inside:
/// the cores overlap, and there is no direction to return.
fn solve_simplex3(s: &mut Simplex) -> Vec2 {
    let w1 = s.v[0].w;
    let w2 = s.v[1].w;
    let w3 = s.v[2].w;

    // Edge12
    let e12 = w2 - w1;
    let w1e12 = dot(w1, e12);
    let w2e12 = dot(w2, e12);
    let d12_1 = w2e12;
    let d12_2 = -w1e12;

    // Edge13
    let e13 = w3 - w1;
    let w1e13 = dot(w1, e13);
    let w3e13 = dot(w3, e13);
    let d13_1 = w3e13;
    let d13_2 = -w1e13;

    // Edge23
    let e23 = w3 - w2;
    let w2e23 = dot(w2, e23);
    let w3e23 = dot(w3, e23);
    let d23_1 = w3e23;
    let d23_2 = -w2e23;

    // Triangle123
    let n123 = cross(e12, e13);

    let d123_1 = n123 * cross(w2, w3);
    let d123_2 = n123 * cross(w3, w1);
    let d123_3 = n123 * cross(w1, w2);

    // w1 region
    if d12_2 <= 0.0 && d13_2 <= 0.0 {
        s.v[0].a = 1.0;
        s.count = 1;
        return -w1;
    }

    // e12
    if d12_1 > 0.0 && d12_2 > 0.0 && d123_3 <= 0.0 {
        let inv_d12 = 1.0 / (d12_1 + d12_2);
        s.v[0].a = d12_1 * inv_d12;
        s.v[1].a = d12_2 * inv_d12;
        s.count = 2;
        return cross_sv(cross(w1 + w2, e12), e12);
    }

    // e13
    if d13_1 > 0.0 && d13_2 > 0.0 && d123_2 <= 0.0 {
        let inv_d13 = 1.0 / (d13_1 + d13_2);
        s.v[0].a = d13_1 * inv_d13;
        s.v[2].a = d13_2 * inv_d13;
        s.count = 2;
        s.v[1] = s.v[2];
        return cross_sv(cross(w1 + w3, e13), e13);
    }

    // w2 region
    if d12_1 <= 0.0 && d23_2 <= 0.0 {
        s.v[1].a = 1.0;
        s.count = 1;
        s.v[0] = s.v[1];
        return -w2;
    }

    // w3 region
    if d13_1 <= 0.0 && d23_1 <= 0.0 {
        s.v[2].a = 1.0;
        s.count = 1;
        s.v[0] = s.v[2];
        return -w3;
    }

    // e23
    if d23_1 > 0.0 && d23_2 > 0.0 && d123_1 <= 0.0 {
        let inv_d23 = 1.0 / (d23_1 + d23_2);
        s.v[1].a = d23_1 * inv_d23;
        s.v[2].a = d23_2 * inv_d23;
        s.count = 2;
        s.v[0] = s.v[2];
        return cross_sv(cross(w2 + w3, e23), e23);
    }

    // Must be in triangle123
    let inv_d123 = 1.0 / (d123_1 + d123_2 + d123_3);
    s.v[0].a = d123_1 * inv_d123;
    s.v[1].a = d123_2 * inv_d123;
    s.v[2].a = d123_3 * inv_d123;
    s.count = 3;

    // No search direction
    Vec2::ZERO
}

/// The GJK distance query (`b2ComputeShapeDistance`), and the features of
/// its final simplex.
///
/// It grows a simplex in the Minkowski difference toward the origin: solve
/// the simplex for its part nearest the origin, look for a support point
/// further that way, and stop when the support point is one the simplex
/// already had. Three vertices that hold the origin mean overlap.
///
/// `max_iterations` is [`MAX_GJK_ITERATIONS`] for every caller but the test
/// of the cap itself.
pub(super) fn gjk(
    a: &Proxy,
    b: &Proxy,
    use_radii: bool,
    max_iterations: u32,
) -> (ClosestPoints<Vec2>, Features) {
    let points_a = a.points();
    let points_b = b.points();

    let mut simplex = Simplex {
        v: [SimplexVertex::new(points_a, points_b, 0, 0); 3],
        count: 1,
    };
    let mut non_unit_normal = Vec2::ZERO;

    // The vertices of the last simplex, to find duplicates and so prevent
    // cycling.
    let mut saved = [(0, 0); 3];

    let mut iterations = 0;
    let mut settled = false;
    while iterations < max_iterations {
        let save_count = simplex.count;
        for (slot, vertex) in saved.iter_mut().zip(&simplex.v[..save_count]) {
            *slot = (vertex.index_a, vertex.index_b);
        }

        let d = match simplex.count {
            1 => -simplex.v[0].w,
            2 => solve_simplex2(&mut simplex),
            _ => solve_simplex3(&mut simplex),
        };

        // Three points hold the origin; a direction too short to normalise
        // means a segment or a triangle probably does. Either way the cores
        // overlap.
        if simplex.count == 3 || dot(d, d) < MIN_DIRECTION_LENGTH_SQUARED {
            let (point_a, point_b) = witness_points(&simplex);
            let overlapped = ClosestPoints {
                state: DistanceState::Overlapped,
                point_a,
                point_b,
                normal: Vec2::ZERO,
                distance: 0.0,
                iterations,
            };
            return (overlapped, Features::of(&simplex));
        }

        non_unit_normal = d;

        // A tentative new vertex: support(A, d) - support(B, -d).
        let index_a = find_support(points_a, d);
        let index_b = find_support(points_b, -d);
        simplex.v[simplex.count] = SimplexVertex::new(points_a, points_b, index_a, index_b);

        // The iteration count is the number of support point calls.
        iterations += 1;

        // A duplicate support point is the main termination criterion.
        if saved[..save_count].contains(&(index_a, index_b)) {
            settled = true;
            break;
        }

        // The new vertex is valid and needed.
        simplex.count += 1;
    }

    // At the cap the last vertex was added and never solved, so its weight
    // means nothing: leave it out and answer from the simplex last solved.
    if !settled {
        simplex.count -= 1;
    }

    let normal = normalize(non_unit_normal);
    let (mut point_a, mut point_b) = witness_points(&simplex);
    let dx = point_b.x - point_a.x;
    let dy = point_b.y - point_a.y;
    let mut distance = (dx * dx + dy * dy).sqrt();

    if use_radii {
        distance = max_float(0.0, distance - a.radius() - b.radius());

        // The points stay on the perimeters even when the shapes overlap,
        // so they move smoothly.
        point_a = Vec2::new(
            point_a.x + a.radius() * normal.x,
            point_a.y + a.radius() * normal.y,
        );
        point_b = Vec2::new(
            point_b.x - b.radius() * normal.x,
            point_b.y - b.radius() * normal.y,
        );
    }

    let found = ClosestPoints {
        state: if settled {
            DistanceState::Separated
        } else {
            DistanceState::IterationCap
        },
        point_a,
        point_b,
        normal,
        distance,
        iterations,
    };
    (found, Features::of(&simplex))
}

/// The closest points between two shapes in one frame, their distance and
/// the unit normal from `a` toward `b` (`b2ComputeShapeDistance`).
///
/// With `use_radii` the points lie on the shapes' surfaces and the distance
/// is between the surfaces, stopping at zero. Without it they lie on the
/// cores and the distance is between the cores, so a caller that wants a
/// signed separation subtracts the radii itself. Cores that overlap give
/// [`DistanceState::Overlapped`], zero distance and no normal either way.
pub(crate) fn distance(a: &Proxy, b: &Proxy, use_radii: bool) -> ClosestPoints<Vec2> {
    gjk(a, b, use_radii, MAX_GJK_ITERATIONS).0
}

/// Where shape `b`, moved by `translation_b`, first meets the fixed shape
/// `a`, both in one frame (`b2CastShape`). See
/// [`cast_by_advancement`] for the parameters.
pub(crate) fn shape_cast(
    a: &Proxy,
    b: &Proxy,
    translation_b: Vec2,
    max_fraction: f32,
    can_encroach: bool,
    slop: f32,
) -> Cast<Vec2> {
    cast_by_advancement(
        a,
        b,
        translation_b,
        max_fraction,
        can_encroach,
        slop,
        |a, b| gjk(a, b, false, MAX_GJK_ITERATIONS).0,
    )
}

#[cfg(test)]
mod tests;
