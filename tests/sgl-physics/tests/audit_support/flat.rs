//! Plane geometry in `f64`, written for the audit from the shapes'
//! definitions (physics.md 9 and 19) and never from the crate: every 2D
//! shape is a convex core (a point, a segment or a polygon) grown by a
//! radius, and two shapes are apart by the distance between their cores
//! less both radii.

use sgl_core::math::Vec2;
use sgl_physics::{Collider, Shape};

pub type P = (f64, f64);

pub fn p(v: Vec2) -> P {
    (f64::from(v.x), f64::from(v.y))
}

pub fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}

pub fn add(a: P, b: P) -> P {
    (a.0 + b.0, a.1 + b.1)
}

pub fn scale(a: P, s: f64) -> P {
    (a.0 * s, a.1 * s)
}

pub fn dot(a: P, b: P) -> f64 {
    a.0 * b.0 + a.1 * b.1
}

pub fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

pub fn length(a: P) -> f64 {
    dot(a, a).sqrt()
}

/// `v` turned by the unit rotation `(c, s)`.
pub fn turn(v: P, q: P) -> P {
    (q.0 * v.0 - q.1 * v.1, q.1 * v.0 + q.0 * v.1)
}

/// A shape placed in the plane: its core's points (one, two, or a convex
/// polygon counter-clockwise) and its radius.
#[derive(Debug, Clone)]
pub struct Placed {
    pub core: Vec<P>,
    pub radius: f64,
}

/// `shape` with its centre at `at`, turned by the unit rotation `q`.
pub fn place(shape: &Shape<Vec2>, at: P, q: P) -> Placed {
    let put = |v: P| add(at, turn(v, q));
    match *shape {
        Shape::Box { half } => {
            let (x, y) = p(half);
            Placed {
                core: [(-x, -y), (x, -y), (x, y), (-x, y)]
                    .into_iter()
                    .map(put)
                    .collect(),
                radius: 0.0,
            }
        }
        Shape::Ball { radius } => Placed {
            core: vec![at],
            radius: f64::from(radius),
        },
        Shape::Capsule { half_axis, radius } => {
            let h = p(half_axis);
            Placed {
                core: vec![put((-h.0, -h.1)), put(h)],
                radius: f64::from(radius),
            }
        }
        Shape::Convex(convex) => Placed {
            core: convex.points().iter().map(|&v| put(p(v))).collect(),
            radius: 0.0,
        },
    }
}

/// A collider as placed: its shape at its position, unturned. A cut box
/// is clipped by its plane (physics.md 16).
pub fn collider(c: &Collider<Vec2>) -> Placed {
    let mut placed = place(&c.shape, p(c.position), (1.0, 0.0));
    if let Some(cut) = c.cut {
        let n = p(cut.normal());
        let offset = f64::from(cut.offset());
        let centre = p(c.position);
        let side = |q: P| dot(n, sub(q, centre)) - offset;
        let corners = placed.core.clone();
        let mut out = Vec::new();
        for i in 0..corners.len() {
            let (a, b) = (corners[i], corners[(i + 1) % corners.len()]);
            let (sa, sb) = (side(a), side(b));
            if sa <= 0.0 {
                out.push(a);
            }
            if (sa < 0.0 && sb > 0.0) || (sa > 0.0 && sb < 0.0) {
                out.push(add(a, scale(sub(b, a), sa / (sa - sb))));
            }
        }
        placed.core = out;
    }
    placed
}

/// Distance from `q` to the segment `a b`.
pub fn to_segment(q: P, a: P, b: P) -> f64 {
    let ab = sub(b, a);
    let span = dot(ab, ab);
    let t = if span > 0.0 {
        (dot(sub(q, a), ab) / span).clamp(0.0, 1.0)
    } else {
        0.0
    };
    length(sub(q, add(a, scale(ab, t))))
}

fn edges(core: &[P]) -> Vec<(P, P)> {
    match core.len() {
        1 => vec![(core[0], core[0])],
        2 => vec![(core[0], core[1])],
        n => (0..n).map(|i| (core[i], core[(i + 1) % n])).collect(),
    }
}

/// Whether `q` lies inside the polygon `core` (three or more points,
/// counter-clockwise), its boundary included.
fn contains(core: &[P], q: P) -> bool {
    core.len() >= 3
        && (0..core.len()).all(|i| {
            let (a, b) = (core[i], core[(i + 1) % core.len()]);
            cross(sub(b, a), sub(q, a)) >= 0.0
        })
}

fn segments_cross(a: (P, P), b: (P, P)) -> bool {
    let d1 = cross(sub(a.1, a.0), sub(b.0, a.0));
    let d2 = cross(sub(a.1, a.0), sub(b.1, a.0));
    let d3 = cross(sub(b.1, b.0), sub(a.0, b.0));
    let d4 = cross(sub(b.1, b.0), sub(a.1, b.0));
    d1 * d2 < 0.0 && d3 * d4 < 0.0
}

/// How far the cores must move apart to clear, along the axis that needs
/// least (the edge normals of both: the separating axes of two convex
/// cores), when they intersect.
fn penetration(a: &[P], b: &[P]) -> f64 {
    let mut axes = Vec::new();
    for core in [a, b] {
        if core.len() >= 2 {
            for (u, v) in edges(core) {
                let e = sub(v, u);
                let l = length(e);
                if l > 0.0 {
                    axes.push((-e.1 / l, e.0 / l));
                }
            }
        }
    }
    axes.iter()
        .map(|&axis| {
            let span = |core: &[P]| {
                core.iter()
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &q| {
                        let d = dot(q, axis);
                        (lo.min(d), hi.max(d))
                    })
            };
            // How far one must move along the axis to clear the other.
            let ((alo, ahi), (blo, bhi)) = (span(a), span(b));
            (ahi - blo).min(bhi - alo)
        })
        .fold(f64::INFINITY, f64::min)
        .min(1e300)
}

/// The signed gap between two placed shapes: the distance between their
/// surfaces when apart, minus how deep they are when they overlap.
pub fn separation(a: &Placed, b: &Placed) -> f64 {
    let (ea, eb) = (edges(&a.core), edges(&b.core));
    let intersect = a.core.iter().any(|&q| contains(&b.core, q))
        || b.core.iter().any(|&q| contains(&a.core, q))
        || ea.iter().any(|&x| eb.iter().any(|&y| segments_cross(x, y)));
    let core = if intersect {
        -penetration(&a.core, &b.core).max(0.0)
    } else {
        let one_way = |core: &[P], other: &[(P, P)]| {
            core.iter()
                .flat_map(|&q| other.iter().map(move |&(u, v)| to_segment(q, u, v)))
                .fold(f64::INFINITY, f64::min)
        };
        one_way(&a.core, &eb).min(one_way(&b.core, &ea))
    };
    core - a.radius - b.radius
}

/// The largest coordinate magnitude of a placed shape, for its rounding.
pub fn largest(a: &Placed) -> f64 {
    a.core
        .iter()
        .fold(0.0, |most: f64, q| most.max(q.0.abs()).max(q.1.abs()))
        + a.radius
}
