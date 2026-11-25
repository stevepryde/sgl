//! How two planar bodies that overlap or nearly touch meet: `Box2D`'s
//! contact manifold, through the collision function its contact registry
//! picks for the pair ([`super::collide`]).
//!
//! The pair order follows `b2_contactRegistry` of `Box2D` v3.2.0 (commit
//! `178495ba35695aefe9c321e569d2277f06db3b90`), `src/contact.c`.
//! Copyright 2026 Erin Catto, MIT: `LICENSE-box2d.txt` beside this
//! crate's manifest. Departures are in `PROVENANCE.md`.

use sgl_core::math::Vec2;

use super::Body;
use super::collide::{self, Local, Polygon};
use super::toi::reach;
use crate::convex::d2::{self, Rot, dot, rotate, transform_point};
use crate::shape::Shape;
use crate::space;
use crate::sweep::{meets, rounding_slop, shape_slop};

/// How far apart, in slops, a manifold still reports two shapes: `Box2D`'s
/// `B2_SPECULATIVE_DISTANCE`, four times its `B2_LINEAR_SLOP`.
const SPECULATIVE_SLOPS: f32 = 4.0;

/// How two bodies that overlap, or are within four slops of each other,
/// meet: `Box2D`'s contact manifold.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Manifold {
    /// Unit normal from body `a` toward body `b`. Zero only for two balls
    /// (or a ball and a capsule's segment) whose centres coincide, which
    /// no normal separates.
    pub normal: Vec2,
    /// How far the bodies overlap along `normal` at the deepest point, in
    /// world units: positive when they overlap, negative (the gap) when
    /// they are apart within four slops. Moving `b` this far along
    /// `normal`, or `a` against it, leaves them touching and no longer
    /// overlapping.
    pub depth: f32,
    points: [Vec2; 2],
    /// How far apart the surfaces are at each point (negative: overlap).
    separations: [f32; 2],
    count: usize,
}

impl Manifold {
    /// The one or two contact points, midway between the surfaces, in world
    /// units.
    #[must_use]
    pub fn points(&self) -> &[Vec2] {
        &self.points[..self.count]
    }

    /// The centre of the contact: midway between the two points when
    /// their separations are within `slop` of each other (two faces
    /// meeting flat), and otherwise the closer point (a corner meeting a
    /// face first; the other end of the edge may be far off, as a
    /// speculative point).
    pub(super) fn centre(&self, slop: f32) -> Vec2 {
        let [first, second] = self.separations;
        if self.count < 2 || first + slop < second {
            self.points[0]
        } else if second + slop < first {
            self.points[1]
        } else {
            self.points[0] * 0.5 + self.points[1] * 0.5
        }
    }
}

/// How `a` and `b`, as they stand, overlap or nearly touch: the normal from
/// `a` toward `b`, the depth along it and up to two contact points (`Box2D`'s
/// manifold). `None` when they are further apart than four slops, or one
/// of them is not sound.
#[must_use]
pub fn manifold(a: &Body, b: &Body) -> Option<Manifold> {
    let (q_a, q_b) = (a.sound_rotation()?, b.sound_rotation()?);
    let rel = b.position - a.position;
    if !space::is_finite(rel) {
        return None;
    }
    let (found, _) = posed(&a.shape, Vec2::ZERO, q_a, &b.shape, rel, q_b)?;
    let mut out = found;
    for point in &mut out.points[..out.count] {
        *point = a.position + *point;
    }
    Some(out)
}

/// A shape as the manifold meets it, with `Box2D`'s shape types.
enum Solid {
    Circle(Vec2, f32),
    Capsule(Vec2, Vec2, f32),
    Polygon(Polygon),
}

impl Solid {
    /// `shape` in its own frame. A capsule no longer than `slop`, which
    /// `b2MakeCapsule` cannot give a direction and `b2CreateCapsuleShape`
    /// refuses, is met as the circle it nearly is.
    fn of(shape: &Shape<Vec2>, slop: f32) -> Self {
        match *shape {
            Shape::Ball { radius } => Self::Circle(Vec2::ZERO, radius),
            Shape::Capsule { half_axis, radius } => {
                let length = half_axis * 2.0;
                if dot(length, length) <= slop * slop {
                    Self::Circle(Vec2::ZERO, radius)
                } else {
                    Self::Capsule(-half_axis, half_axis, radius)
                }
            }
            Shape::Box { half } => Self::Polygon(Polygon::boxed(half)),
            Shape::Convex(convex) => Self::Polygon(Polygon::of(convex.points())),
        }
    }

    /// `Box2D`'s shape type order: a pair is collided with the later type as
    /// shape A (the primary order of `b2_contactRegistry`).
    fn rank(&self) -> u8 {
        match self {
            Self::Circle(..) => 0,
            Self::Capsule(..) => 1,
            Self::Polygon(_) => 3,
        }
    }
}

/// `Box2D`'s collision function for the pair, shape A of rank at least B's,
/// `xf` being B's pose in A's frame: the function `b2_contactRegistry` names. Two
/// capsules go through `b2CollidePolygons` as two-sided polygons, as a
/// polygon and a capsule do in the source (`PROVENANCE.md`).
fn collide(a: &Solid, b: &Solid, xf: d2::Transform, slop: f32) -> Local {
    let speculative = SPECULATIVE_SLOPS * slop;
    match (a, b) {
        (Solid::Polygon(poly_a), Solid::Polygon(poly_b)) => {
            collide::collide_polygons(poly_a, poly_b, xf, slop, speculative)
        }
        (Solid::Polygon(poly_a), Solid::Capsule(p1, p2, radius)) => {
            let poly_b = Polygon::capsule(*p1, *p2, *radius);
            collide::collide_polygons(poly_a, &poly_b, xf, slop, speculative)
        }
        (Solid::Polygon(poly_a), Solid::Circle(center, radius)) => {
            collide::collide_polygon_and_circle(poly_a, *center, *radius, xf, speculative)
        }
        (Solid::Capsule(a1, a2, radius_a), Solid::Capsule(b1, b2, radius_b)) => {
            let poly_a = Polygon::capsule(*a1, *a2, *radius_a);
            let poly_b = Polygon::capsule(*b1, *b2, *radius_b);
            collide::collide_polygons(&poly_a, &poly_b, xf, slop, speculative)
        }
        (Solid::Capsule(p1, p2, radius_a), Solid::Circle(center, radius_b)) => {
            collide::collide_capsule_and_circle(
                (*p1, *p2, *radius_a),
                *center,
                *radius_b,
                xf,
                speculative,
            )
        }
        (Solid::Circle(center_a, radius_a), Solid::Circle(center_b, radius_b)) => {
            collide::collide_circles(*center_a, *radius_a, *center_b, *radius_b, xf, speculative)
        }
        // A lower rank is never shape A: the caller swaps the pair.
        _ => Local::default(),
    }
}

/// [`manifold`] for `a` at `p_a` turned by `q_a` and `b` at `p_b` turned
/// by `q_b`, in one frame, and the pair's slop.
pub(super) fn posed(
    a: &Shape<Vec2>,
    p_a: Vec2,
    q_a: Rot,
    b: &Shape<Vec2>,
    p_b: Vec2,
    q_b: Rot,
) -> Option<(Manifold, f32)> {
    if !meets(a, b) {
        return None;
    }
    let largest = space::max(
        space::largest_abs(p_a) + reach(a),
        space::largest_abs(p_b) + reach(b),
    );
    let slop = space::max(shape_slop(a, b), rounding_slop(largest));
    let (solid_a, solid_b) = (Solid::of(a, slop), Solid::of(b, slop));
    let swap = solid_a.rank() < solid_b.rank();
    let (local, p, q) = if swap {
        let xf = collide::relative(p_b, q_b, p_a, q_a);
        (collide(&solid_b, &solid_a, xf, slop), p_b, q_b)
    } else {
        let xf = collide::relative(p_a, q_a, p_b, q_b);
        (collide(&solid_a, &solid_b, xf, slop), p_a, q_a)
    };
    if local.count == 0 {
        return None;
    }
    let mut out = Manifold {
        normal: rotate(q, local.normal),
        depth: -f32::MAX,
        points: [Vec2::ZERO; 2],
        separations: [0.0; 2],
        count: local.count,
    };
    if swap {
        out.normal = -out.normal;
    }
    for (slot, point) in local.points[..local.count].iter().enumerate() {
        out.points[slot] = transform_point(p, q, point.point);
        out.separations[slot] = point.separation;
        out.depth = space::max(out.depth, -point.separation);
    }
    Some((out, slop))
}
