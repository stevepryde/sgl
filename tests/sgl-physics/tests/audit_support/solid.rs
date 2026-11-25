//! Solid geometry in `f64`, written for the audit from the shapes'
//! definitions (physics.md 9, 16 and 19) and never from the crate: the
//! distance from a point to a 3D collider, and how deep a round body (a
//! ball, or a capsule: a segment grown by a radius) reaches into one.

use sgl_core::math::Vec3;
use sgl_physics::{Collider, Shape};

use super::flat;

pub type Q = [f64; 3];

pub fn q(v: Vec3) -> Q {
    [f64::from(v.x), f64::from(v.y), f64::from(v.z)]
}

pub fn sub(a: Q, b: Q) -> Q {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn add(a: Q, b: Q) -> Q {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn scale(a: Q, s: f64) -> Q {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub fn dot(a: Q, b: Q) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: Q, b: Q) -> Q {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn length(a: Q) -> f64 {
    dot(a, a).sqrt()
}

/// Distance from `p` to the box of half-extents `half` about `c`, zero
/// inside.
pub fn to_box(p: Q, c: Q, half: Q) -> f64 {
    let mut out = [0.0; 3];
    for axis in 0..3 {
        out[axis] = ((p[axis] - c[axis]).abs() - half[axis]).max(0.0);
    }
    length(out)
}

/// Distance from `p` to the segment `a b`.
pub fn to_segment(p: Q, a: Q, b: Q) -> f64 {
    let ab = sub(b, a);
    let span = dot(ab, ab);
    let t = if span > 0.0 {
        (dot(sub(p, a), ab) / span).clamp(0.0, 1.0)
    } else {
        0.0
    };
    length(sub(p, add(a, scale(ab, t))))
}

/// Distance from `p` to a 3D collider's solid part, zero inside. A cut box
/// turns about the axis its normal has no part along, so it is its
/// cross-section in the other two, swept along that one. A convex collider
/// is the hull of its points, measured by `to_hull`.
pub fn to_collider(p: Q, c: &Collider<Vec3>) -> f64 {
    let centre = q(c.position);
    match (c.shape, c.cut) {
        (Shape::Box { half }, None) => to_box(p, centre, q(half)),
        (Shape::Box { half }, Some(cut)) => {
            let n = q(cut.normal());
            let along = (0..3)
                .find(|&a| n[a] == 0.0)
                .expect("a 3D cut turns about an axis");
            let (i, j) = match along {
                0 => (1, 2),
                1 => (2, 0),
                _ => (0, 1),
            };
            let half = q(half);
            let flat_box = Collider::new(
                0,
                Shape::Box {
                    half: sgl_core::math::Vec2::new(half[i] as f32, half[j] as f32),
                },
                sgl_core::math::Vec2::new(centre[i] as f32, centre[j] as f32),
            )
            .with_cut(sgl_physics::Cut::new(
                sgl_core::math::Vec2::new(n[i] as f32, n[j] as f32),
                cut.offset(),
            ));
            let section = flat::collider(&flat_box);
            let across = flat::separation(
                &section,
                &flat::Placed {
                    core: vec![(p[i], p[j])],
                    radius: 0.0,
                },
            )
            .max(0.0);
            let beyond = ((p[along] - centre[along]).abs() - half[along]).max(0.0);
            across.hypot(beyond)
        }
        (Shape::Ball { radius }, _) => (length(sub(p, centre)) - f64::from(radius)).max(0.0),
        (Shape::Capsule { half_axis, radius }, _) => {
            let h = q(half_axis);
            (to_segment(p, sub(centre, h), add(centre, h)) - f64::from(radius)).max(0.0)
        }
        (Shape::Convex(hull), _) => {
            let points: Vec<Q> = hull.points().iter().map(|&v| add(centre, q(v))).collect();
            to_hull(p, &points)
        }
    }
}

/// Distance from `p` to the convex hull of `points`, zero inside: the
/// largest signed distance to any plane through three of the points that
/// has every point on one side (the hull's face planes), when that is
/// inside, and otherwise the least distance to a face, an edge or a point.
/// Brute force, for the handful of points a test hull has.
pub fn to_hull(p: Q, points: &[Q]) -> f64 {
    let n = points.len();
    let mut outside: f64 = f64::NEG_INFINITY;
    let mut faces = Vec::new();
    let mut flat_hull = false;
    for a in 0..n {
        for b in a + 1..n {
            for c in b + 1..n {
                let normal = cross(sub(points[b], points[a]), sub(points[c], points[a]));
                let l = length(normal);
                if l < 1e-9 {
                    continue;
                }
                let normal = scale(normal, 1.0 / l);
                let side: Vec<f64> = points
                    .iter()
                    .map(|&x| dot(normal, sub(x, points[a])))
                    .collect();
                if side.iter().all(|&s| s.abs() <= 1e-9) {
                    // A flat hull has faces and no inside.
                    faces.push((points[a], points[b], points[c]));
                    flat_hull = true;
                    continue;
                }
                let normal = if side.iter().all(|&s| s <= 1e-9) {
                    normal
                } else if side.iter().all(|&s| s >= -1e-9) {
                    scale(normal, -1.0)
                } else {
                    continue;
                };
                outside = outside.max(dot(normal, sub(p, points[a])));
                faces.push((points[a], points[b], points[c]));
            }
        }
    }
    if outside <= 0.0 && !faces.is_empty() && !flat_hull {
        return 0.0;
    }
    let mut least = f64::INFINITY;
    for &x in points {
        least = least.min(length(sub(p, x)));
    }
    for a in 0..n {
        for b in a + 1..n {
            least = least.min(to_segment(p, points[a], points[b]));
        }
    }
    for (a, b, c) in faces {
        least = least.min(to_triangle(p, a, b, c));
    }
    least
}

/// Distance from `p` to the triangle `a b c`.
pub fn to_triangle(p: Q, a: Q, b: Q, c: Q) -> f64 {
    let normal = cross(sub(b, a), sub(c, a));
    let l = length(normal);
    let edges = to_segment(p, a, b)
        .min(to_segment(p, b, c))
        .min(to_segment(p, c, a));
    if l == 0.0 {
        return edges;
    }
    let normal = scale(normal, 1.0 / l);
    let height = dot(normal, sub(p, a));
    let foot = sub(p, scale(normal, height));
    let inside = [(a, b), (b, c), (c, a)]
        .iter()
        .all(|&(u, v)| dot(cross(sub(v, u), sub(foot, u)), normal) >= 0.0);
    if inside { height.abs() } else { edges }
}

/// How far a round body at `at` reaches into a solid whose distance from a
/// point `to` measures: its radius less the least distance from its core
/// (a point, or a segment) to the solid. Positive when it overlaps. The
/// distance to a convex solid is convex along the segment, so a golden
/// section search finds its least to well under a micrometre.
pub fn depth(body: &Shape<Vec3>, at: Vec3, to: impl Fn(Q) -> f64) -> f64 {
    let centre = q(at);
    match *body {
        Shape::Ball { radius } => f64::from(radius) - to(centre),
        Shape::Capsule { half_axis, radius } => {
            let h = q(half_axis);
            let point = |s: f64| add(centre, scale(h, s));
            let (mut lo, mut hi) = (-1.0f64, 1.0f64);
            let ratio = (5.0f64.sqrt() - 1.0) / 2.0;
            for _ in 0..60 {
                let m1 = hi - ratio * (hi - lo);
                let m2 = lo + ratio * (hi - lo);
                if to(point(m1)) <= to(point(m2)) {
                    hi = m2;
                } else {
                    lo = m1;
                }
            }
            let least = [-1.0, 1.0, 0.5 * (lo + hi)]
                .into_iter()
                .map(|s| to(point(s)))
                .fold(f64::INFINITY, f64::min);
            f64::from(radius) - least
        }
        _ => unreachable!("round bodies only"),
    }
}
