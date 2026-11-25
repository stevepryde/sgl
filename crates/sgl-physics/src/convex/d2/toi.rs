//! Time of impact in 2D: when two shapes that each move and turn over a
//! step first come within the target distance of each other.
//!
//! Ported from `Box2D` v3.2.0 (commit
//! `178495ba35695aefe9c321e569d2277f06db3b90`), `src/distance.c`
//! (`b2GetSweepTransform`, `b2MakeSeparationFunction`,
//! `b2FindMinSeparation`, `b2EvaluateSeparation` and
//! `b2ComputeTimeOfImpact`), with `b2Sweep`, `b2TOIInput` and
//! `b2TOIOutput` of `include/box2d/collision.h`. Copyright 2022 Erin Catto,
//! MIT: `LICENSE-box2d.txt` beside this crate's manifest. Departures and
//! algorithm notes are in `PROVENANCE.md`.
//!
//! This is continuous collision by the local separating axis method. At a
//! time `t1` where the shapes are apart, the distance query names the
//! features nearest each other (two points, or a point and an edge), which
//! define an axis that moves with the shapes. Along that axis the
//! separation is a function of time alone, so a one-dimensional root finder
//! gives the latest time the axis still separates the shapes by the target:
//! the shapes cannot have met before it. Time advances to there and the
//! search repeats with the features of the new configuration, until the
//! shapes are within tolerance of the target (a hit) or separated at the end
//! of the step.

use sgl_core::math::Vec2;

use super::{
    Features, MAX_GJK_ITERATIONS, MAX_POINTS, Proxy, Rot, dot, find_support, gjk, inv_rotate,
    normalize, rotate, transform_point,
};
use crate::convex::target_and_tolerance;

/// The most separating axes one query tries (`k_maxIterations` in
/// `b2ComputeTimeOfImpact`), each from a new distance query. Shapes that
/// approach along one axis need two or three.
pub(crate) const MAX_TOI_ITERATIONS: u32 = 20;

/// The most times one axis is pushed back to its deepest point: once for
/// each point a shape may have (`B2_MAX_POLYGON_VERTICES`), since each push
/// resolves a different vertex.
pub(crate) const MAX_PUSH_BACK_ITERATIONS: usize = MAX_POINTS;

/// The most steps the root finder takes on one axis. It alternates
/// bisection with false position, so fifty halvings are far past what `f32`
/// resolves.
pub(crate) const MAX_ROOT_ITERATIONS: u32 = 50;

/// The motion of one shape over the step (`b2Sweep`): `local_center`, a
/// point given in the shape's own frame, travels in a straight line from
/// `c1` to `c2` while the shape turns from `q1` to `q2` about it.
///
/// The rotation between the two is the normalised blend of their cosines
/// and sines, not a constant angular speed: a quarter turn is exact at both
/// ends and at the middle, and within a degree elsewhere. Both rotations
/// are unit. `c1` and `c2` of both sweeps share one frame, which the caller
/// keeps near the shapes (relative to one body's start).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Sweep {
    /// The point of the shape, in its own frame, that travels straight: a
    /// body's centre of mass in the source, a vehicle's pivot here.
    pub local_center: Vec2,
    /// Where that point starts.
    pub c1: Vec2,
    /// Where that point ends.
    pub c2: Vec2,
    /// The rotation at the start.
    pub q1: Rot,
    /// The rotation at the end.
    pub q2: Rot,
}

/// How a time of impact query ended (`b2TOIState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToiState {
    /// The search gave up: the root finder ran out of steps and left the
    /// shapes closer than the target, or [`MAX_TOI_ITERATIONS`] axes were
    /// tried. The fraction is the latest time known to be safe; a mover
    /// stops there.
    Failed,
    /// The shapes' cores overlap at the start. The fraction is zero.
    Overlapped,
    /// The shapes come within the target distance at the fraction, with the
    /// point and normal of the contact.
    Hit,
    /// The shapes stay apart over the whole step. The fraction is
    /// `max_fraction`.
    Separated,
}

/// When two moving shapes first meet (`b2TOIOutput`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Toi {
    /// How the query ended.
    pub state: ToiState,
    /// The fraction of the step at which it ended, in `[0, max_fraction]`.
    pub fraction: f32,
    /// The contact point, midway between the two surfaces, in the sweeps'
    /// frame. Zero unless the state is `Hit`, or `Failed` at the cap.
    pub point: Vec2,
    /// Unit normal from shape A toward shape B, in the sweeps' frame. Zero
    /// unless the state is `Hit`, or `Failed` at the cap.
    pub normal: Vec2,
}

/// A position and a rotation (`b2Transform`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Transform {
    /// The position.
    pub p: Vec2,
    /// The rotation.
    pub q: Rot,
}

/// Where a sweep puts its shape at `time` (`b2GetSweepTransform`).
pub(crate) fn sweep_transform(sweep: &Sweep, time: f32) -> Transform {
    let center = Vec2::new(
        (1.0 - time) * sweep.c1.x + time * sweep.c2.x,
        (1.0 - time) * sweep.c1.y + time * sweep.c2.y,
    );

    let c = (1.0 - time) * sweep.q1.c + time * sweep.q2.c;
    let s = (1.0 - time) * sweep.q1.s + time * sweep.q2.s;
    let mag = (s * s + c * c).sqrt();
    let inv_mag = if mag > 0.0 { 1.0 / mag } else { 0.0 };
    let q = Rot {
        c: c * inv_mag,
        s: s * inv_mag,
    };

    // Shift to the shape's origin.
    Transform {
        p: center - rotate(q, sweep.local_center),
        q,
    }
}

/// The pose of `b` in the frame of `a` (`b2InvMulTransforms`).
pub(crate) fn inv_mul_transforms(a: Transform, b: Transform) -> Transform {
    Transform {
        q: Rot {
            s: a.q.c * b.q.s - a.q.s * b.q.c,
            c: a.q.c * b.q.c + a.q.s * b.q.s,
        },
        p: inv_rotate(a.q, b.p - a.p),
    }
}

/// Which features define the separating axis (`b2SeparationType`).
#[derive(Clone, Copy)]
enum SeparationType {
    /// One point of each shape: the axis is fixed in the sweeps' frame.
    Points,
    /// An edge of A and a point of B: the axis turns with A.
    FaceA,
    /// An edge of B and a point of A: the axis turns with B.
    FaceB,
}

/// A separating axis that moves with the shapes (`b2SeparationFunction`).
struct SeparationFunction<'a> {
    proxy_a: &'a Proxy,
    proxy_b: &'a Proxy,
    sweep_a: &'a Sweep,
    sweep_b: &'a Sweep,
    local_point: Vec2,
    axis: Vec2,
    kind: SeparationType,
}

impl<'a> SeparationFunction<'a> {
    /// The axis through the features nearest each other at `t1`
    /// (`b2MakeSeparationFunction`), which `features` names.
    fn new(
        features: &Features,
        proxy_a: &'a Proxy,
        sweep_a: &'a Sweep,
        proxy_b: &'a Proxy,
        sweep_b: &'a Sweep,
        t1: f32,
    ) -> Self {
        let points_a = proxy_a.points();
        let points_b = proxy_b.points();
        let xf_a = sweep_transform(sweep_a, t1);
        let xf_b = sweep_transform(sweep_b, t1);

        let mut f = Self {
            proxy_a,
            proxy_b,
            sweep_a,
            sweep_b,
            local_point: Vec2::ZERO,
            axis: Vec2::ZERO,
            kind: SeparationType::Points,
        };

        if features.count == 1 {
            let point_a = transform_point(xf_a.p, xf_a.q, points_a[features.index_a[0]]);
            let point_b = transform_point(xf_b.p, xf_b.q, points_b[features.index_b[0]]);
            f.axis = normalize(point_b - point_a);
            return f;
        }

        if features.index_a[0] == features.index_a[1] {
            // Two points on B and one on A.
            f.kind = SeparationType::FaceB;
            let local_point_b1 = points_b[features.index_b[0]];
            let local_point_b2 = points_b[features.index_b[1]];

            f.axis = normalize(edge_normal(local_point_b2 - local_point_b1));
            let normal = rotate(xf_b.q, f.axis);

            f.local_point = midpoint(local_point_b1, local_point_b2);
            let point_b = transform_point(xf_b.p, xf_b.q, f.local_point);
            let point_a = transform_point(xf_a.p, xf_a.q, points_a[features.index_a[0]]);

            if dot(point_a - point_b, normal) < 0.0 {
                f.axis = -f.axis;
            }
            return f;
        }

        // Two points on A and one or two points on B.
        f.kind = SeparationType::FaceA;
        let local_point_a1 = points_a[features.index_a[0]];
        let local_point_a2 = points_a[features.index_a[1]];

        f.axis = normalize(edge_normal(local_point_a2 - local_point_a1));
        let normal = rotate(xf_a.q, f.axis);

        f.local_point = midpoint(local_point_a1, local_point_a2);
        let point_a = transform_point(xf_a.p, xf_a.q, f.local_point);
        let point_b = transform_point(xf_b.p, xf_b.q, points_b[features.index_b[0]]);

        if dot(point_b - point_a, normal) < 0.0 {
            f.axis = -f.axis;
        }
        f
    }

    /// The separation along the axis at `t`, and the points of A and B
    /// deepest along it there (`b2FindMinSeparation`). A face's own shape
    /// has no such point; its index is unused.
    fn find_min_separation(&self, t: f32) -> (f32, usize, usize) {
        let xf_a = sweep_transform(self.sweep_a, t);
        let xf_b = sweep_transform(self.sweep_b, t);
        let points_a = self.proxy_a.points();
        let points_b = self.proxy_b.points();

        match self.kind {
            SeparationType::Points => {
                let axis_a = inv_rotate(xf_a.q, self.axis);
                let axis_b = inv_rotate(xf_b.q, -self.axis);

                let index_a = find_support(points_a, axis_a);
                let index_b = find_support(points_b, axis_b);

                let point_a = transform_point(xf_a.p, xf_a.q, points_a[index_a]);
                let point_b = transform_point(xf_b.p, xf_b.q, points_b[index_b]);

                (dot(point_b - point_a, self.axis), index_a, index_b)
            }
            SeparationType::FaceA => {
                let normal = rotate(xf_a.q, self.axis);
                let point_a = transform_point(xf_a.p, xf_a.q, self.local_point);

                let axis_b = inv_rotate(xf_b.q, -normal);
                let index_b = find_support(points_b, axis_b);
                let point_b = transform_point(xf_b.p, xf_b.q, points_b[index_b]);

                (dot(point_b - point_a, normal), 0, index_b)
            }
            SeparationType::FaceB => {
                let normal = rotate(xf_b.q, self.axis);
                let point_b = transform_point(xf_b.p, xf_b.q, self.local_point);

                let axis_a = inv_rotate(xf_a.q, -normal);
                let index_a = find_support(points_a, axis_a);
                let point_a = transform_point(xf_a.p, xf_a.q, points_a[index_a]);

                (dot(point_a - point_b, normal), index_a, 0)
            }
        }
    }

    /// The separation along the axis at `t` of the given points of A and B
    /// (`b2EvaluateSeparation`).
    fn evaluate(&self, index_a: usize, index_b: usize, t: f32) -> f32 {
        let xf_a = sweep_transform(self.sweep_a, t);
        let xf_b = sweep_transform(self.sweep_b, t);
        let points_a = self.proxy_a.points();
        let points_b = self.proxy_b.points();

        match self.kind {
            SeparationType::Points => {
                let point_a = transform_point(xf_a.p, xf_a.q, points_a[index_a]);
                let point_b = transform_point(xf_b.p, xf_b.q, points_b[index_b]);
                dot(point_b - point_a, self.axis)
            }
            SeparationType::FaceA => {
                let normal = rotate(xf_a.q, self.axis);
                let point_a = transform_point(xf_a.p, xf_a.q, self.local_point);
                let point_b = transform_point(xf_b.p, xf_b.q, points_b[index_b]);
                dot(point_b - point_a, normal)
            }
            SeparationType::FaceB => {
                let normal = rotate(xf_b.q, self.axis);
                let point_b = transform_point(xf_b.p, xf_b.q, self.local_point);
                let point_a = transform_point(xf_a.p, xf_a.q, points_a[index_a]);
                dot(point_a - point_b, normal)
            }
        }
    }
}

/// An edge turned a quarter turn clockwise: the source's
/// `b2CrossVS( edge, 1.0f )`.
fn edge_normal(edge: Vec2) -> Vec2 {
    Vec2::new(edge.y, -edge.x)
}

fn midpoint(a: Vec2, b: Vec2) -> Vec2 {
    Vec2::new(0.5 * (a.x + b.x), 0.5 * (a.y + b.y))
}

/// The point midway between the two surfaces along `normal`, from the
/// closest points on the cores.
fn contact_point(point_a: Vec2, radius_a: f32, point_b: Vec2, radius_b: f32, normal: Vec2) -> Vec2 {
    let on_a = Vec2::new(
        point_a.x + radius_a * normal.x,
        point_a.y + radius_a * normal.y,
    );
    let on_b = Vec2::new(
        point_b.x + -radius_b * normal.x,
        point_b.y + -radius_b * normal.y,
    );
    Vec2::new(
        (1.0 - 0.5) * on_a.x + 0.5 * on_b.x,
        (1.0 - 0.5) * on_a.y + 0.5 * on_b.y,
    )
}

/// The fraction of the step, in `[0, max_fraction]`, at which shape `a`
/// moving along `sweep_a` and shape `b` moving along `sweep_b` first come
/// within the target distance of each other (`b2ComputeTimeOfImpact`).
///
/// The proxies are each in their own shape's frame; the sweeps place them.
/// `slop` sets the target distance between the cores and its tolerance as
/// for a shape cast: `max(slop, radii - slop)`, within a quarter of `slop`.
/// It must be positive.
///
/// It may miss a contact that begins and ends within the step when the
/// shapes are apart at its end along the axis found at its start: a thin
/// shape spinning fast past another. The source says the same.
pub(crate) fn time_of_impact(
    a: &Proxy,
    sweep_a: &Sweep,
    b: &Proxy,
    sweep_b: &Sweep,
    max_fraction: f32,
    slop: f32,
) -> Toi {
    let t_max = max_fraction;
    let (target, tolerance) = target_and_tolerance(a.radius() + b.radius(), slop);

    let mut t1 = 0.0;
    // The contact of the last distance query, for the answer at the cap.
    let mut world_normal = Vec2::ZERO;
    let mut world_point = Vec2::ZERO;

    // The outer loop tries one separating axis after another, each from the
    // features nearest each other at t1.
    for _ in 0..MAX_TOI_ITERATIONS {
        let xf_a = sweep_transform(sweep_a, t1);
        let xf_b = sweep_transform(sweep_b, t1);

        // The distance query runs in A's frame.
        let relative = inv_mul_transforms(xf_a, xf_b);
        let local_b = Proxy::offset(b.points(), b.radius(), relative.p, relative.q);
        let (found, features) = gjk(a, &local_b, false, MAX_GJK_ITERATIONS);

        world_normal = rotate(xf_a.q, found.normal);
        world_point = contact_point(
            transform_point(xf_a.p, xf_a.q, found.point_a),
            a.radius(),
            transform_point(xf_a.p, xf_a.q, found.point_b),
            b.radius(),
            world_normal,
        );

        // If the shapes are overlapped, give up on continuous collision.
        if found.distance <= 0.0 {
            return Toi {
                state: ToiState::Overlapped,
                fraction: 0.0,
                point: Vec2::ZERO,
                normal: Vec2::ZERO,
            };
        }

        if found.distance <= target + tolerance {
            return Toi {
                state: ToiState::Hit,
                fraction: t1,
                point: world_point,
                normal: world_normal,
            };
        }

        let f = SeparationFunction::new(&features, a, sweep_a, b, sweep_b, t1);

        // The time of impact on this axis, by resolving its deepest point
        // again and again: once for each vertex at most.
        let mut t2 = t_max;
        for _ in 0..MAX_PUSH_BACK_ITERATIONS {
            // The deepest point at t2.
            let (mut s2, index_a, index_b) = f.find_min_separation(t2);

            // Is the final configuration separated?
            if s2 > target + tolerance {
                return Toi {
                    state: ToiState::Separated,
                    fraction: t_max,
                    point: Vec2::ZERO,
                    normal: Vec2::ZERO,
                };
            }

            // Has the separation reached tolerance?
            if s2 > target - tolerance {
                // Advance the sweeps.
                t1 = t2;
                break;
            }

            // The separation of the same points at the start.
            let mut s1 = f.evaluate(index_a, index_b, t1);

            // Closer than the target already: the root finder ran out of
            // steps on an earlier axis.
            if s1 < target - tolerance {
                return Toi {
                    state: ToiState::Failed,
                    fraction: t1,
                    point: Vec2::ZERO,
                    normal: Vec2::ZERO,
                };
            }

            // Touching: t1 holds the time of impact (it may be zero).
            if s1 <= target + tolerance {
                return Toi {
                    state: ToiState::Hit,
                    fraction: t1,
                    point: world_point,
                    normal: world_normal,
                };
            }

            // The root of separation(t) - target between t1 and t2.
            let mut a1 = t1;
            let mut a2 = t2;
            for root_iteration in 0..MAX_ROOT_ITERATIONS {
                let t = if root_iteration & 1 == 1 {
                    // False position, to converge faster.
                    a1 + (target - s1) * (a2 - a1) / (s2 - s1)
                } else {
                    // Bisection, to guarantee progress.
                    0.5 * (a1 + a2)
                };

                let s = f.evaluate(index_a, index_b, t);

                if (s - target).abs() < tolerance {
                    // t2 holds a tentative value for t1.
                    t2 = t;
                    break;
                }

                // Keep the root bracketed.
                if s > target {
                    a1 = t;
                    s1 = s;
                } else {
                    a2 = t;
                    s2 = s;
                }
            }
        }
    }

    // The root finder got stuck: the latest time known to be safe.
    Toi {
        state: ToiState::Failed,
        fraction: t1,
        point: world_point,
        normal: world_normal,
    }
}
