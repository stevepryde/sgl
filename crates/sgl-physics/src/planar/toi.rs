//! When two moving planar bodies first touch: the kernel's time of impact
//! (`b2ComputeTimeOfImpact`), with each body's motion a translation and a
//! turn, contact a slop apart, and a typed outcome.

use sgl_core::math::Vec2;

use super::manifold::posed;
use super::{Body, Contact, Motion};
use crate::convex::d2::{
    self, Proxy, Sweep, ToiState, dot, inv_mul_transforms, rotate, sweep_transform, transform_point,
};
use crate::convex::{self, DistanceState};
use crate::shape::Shape;
use crate::space;
use crate::sweep::{meets, rounding_slop, shape_slop};

/// When two moving bodies first touch: a typed outcome, never a bare
/// "nothing" that could hide a failure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Toi {
    /// The bodies stay further apart than the contact band over the whole
    /// motion (or one of them is not sound: it occupies nothing).
    Separated,
    /// The bodies come within the contact band, three quarters to one and
    /// a quarter slops apart, at `fraction` of the motion, in `[0, 1]`.
    /// Bodies already that close at the start (touching) hit at zero,
    /// whether or not they are approaching.
    Hit {
        /// The fraction of the motion travelled.
        fraction: f32,
        /// Where and along what normal, with both bodies at `fraction`:
        /// the [`manifold`](super::manifold())'s normal, and the middle of
        /// its two points when two faces meet flat, or else its closer
        /// point.
        contact: Contact,
    },
    /// The bodies overlap at the start: [`manifold`](super::manifold()) says how to separate
    /// them.
    Overlapping,
    /// The search reached an iteration cap (the kernel's, or the gap
    /// check's). The bodies are apart up to `fraction`, which is the
    /// latest time known to be clear, and may meet after it: this is never
    /// a clear path. `contact` is the nearest points there, when they have
    /// a normal.
    Failed {
        /// The latest fraction known to be clear.
        fraction: f32,
        /// The bodies' nearest points and normal at `fraction`.
        contact: Option<Contact>,
    },
}

impl Toi {
    fn moved(self, by: Vec2) -> Self {
        match self {
            Self::Hit { fraction, contact } => Self::Hit {
                fraction,
                contact: contact.moved(by),
            },
            Self::Failed { fraction, contact } => Self::Failed {
                fraction,
                contact: contact.map(|contact| contact.moved(by)),
            },
            other => other,
        }
    }
}

/// When `a` moving by `motion_a` and `b` moving by `motion_b` first touch
/// over the same step (`b2ComputeTimeOfImpact`): the fraction of the step,
/// with the point and normal there.
///
/// Each body's centre moves in a straight line while it turns about it by
/// the blend of its start and end headings (normalised), so a spinning
/// car's corner cannot pass through another car. Contact is the surfaces
/// coming within three quarters to one and a quarter slops of each other
/// (see the module's notes): a hit leaves them that far apart.
///
/// `Box2D`'s search can pass over a contact that begins and ends within
/// the step when the shapes turn (a car clipping another's corner at
/// speed). The answer is checked for one, by the gap at the start of each
/// stretch of the motion and how fast turning can close it, and a contact
/// deeper than half a slop is found; a check that cannot settle within its
/// cap answers [`Toi::Failed`] where it stopped.
///
/// Only `a`'s and `b`'s shapes, positions and headings are read; their
/// velocities are not.
#[must_use]
pub fn time_of_impact(a: &Body, motion_a: Motion, b: &Body, motion_b: Motion) -> Toi {
    let (Some(q_a), Some(q_b)) = (a.sound_rotation(), b.sound_rotation()) else {
        return Toi::Separated;
    };
    let rel = b.position - a.position;
    if !(motion_a.is_sound() && motion_b.is_sound() && space::is_finite(rel)) {
        return Toi::Separated;
    }
    let sweep_a = Sweep {
        local_center: Vec2::ZERO,
        c1: Vec2::ZERO,
        c2: motion_a.translation,
        q1: q_a,
        q2: q_a.integrated(motion_a.turn),
    };
    let sweep_b = Sweep {
        local_center: Vec2::ZERO,
        c1: rel,
        c2: rel + motion_b.translation,
        q1: q_b,
        q2: q_b.integrated(motion_b.turn),
    };
    let slop = sweep_slop(&a.shape, &sweep_a, &b.shape, &sweep_b);
    swept(&a.shape, &sweep_a, &b.shape, &sweep_b, slop).moved(a.position)
}

/// A shape's core in its own frame, as the kernel takes it: a box's
/// corners counter-clockwise, a ball's centre, a capsule's two ends or a
/// polygon's vertices, and the radius.
pub(super) fn core(shape: &Shape<Vec2>) -> Proxy {
    match *shape {
        Shape::Box { half } => Proxy::new(
            &[
                Vec2::new(-half.x, -half.y),
                Vec2::new(half.x, -half.y),
                Vec2::new(half.x, half.y),
                Vec2::new(-half.x, half.y),
            ],
            0.0,
        ),
        Shape::Ball { radius } => Proxy::new(&[Vec2::ZERO], radius),
        Shape::Capsule { half_axis, radius } => Proxy::new(&[-half_axis, half_axis], radius),
        Shape::Convex(convex) => Proxy::new(convex.points(), 0.0),
    }
}

/// How far a shape reaches from its centre, whichever way it is turned.
pub(super) fn reach(shape: &Shape<Vec2>) -> f32 {
    let proxy = core(shape);
    let furthest = proxy.points().iter().fold(0.0, |most, point| {
        space::max(most, dot(*point, *point).sqrt())
    });
    furthest + proxy.radius()
}

/// The slop of two shapes on sweeps in one frame: the query rule, at least
/// the rounding of the largest coordinate the sweeps meet.
pub(super) fn sweep_slop(
    a: &Shape<Vec2>,
    sweep_a: &Sweep,
    b: &Shape<Vec2>,
    sweep_b: &Sweep,
) -> f32 {
    let extent = |shape: &Shape<Vec2>, sweep: &Sweep| {
        space::max(space::largest_abs(sweep.c1), space::largest_abs(sweep.c2)) + reach(shape)
    };
    let largest = space::max(extent(a, sweep_a), extent(b, sweep_b));
    space::max(shape_slop(a, b), rounding_slop(largest))
}

/// The closest points of the two cores with each shape where its sweep
/// puts it at `time`, measured as the kernel's time of impact measures
/// them, in A's frame, and returned in the sweeps' frame: the distance
/// between the cores, the unit normal from A toward B, and the point
/// midway between the surfaces; or [`Closest::Deep`] when the cores
/// overlap or have no normal.
fn closest(a: &Proxy, sweep_a: &Sweep, b: &Proxy, sweep_b: &Sweep, time: f32) -> Closest {
    let xf_a = sweep_transform(sweep_a, time);
    let xf_b = sweep_transform(sweep_b, time);
    let relative = inv_mul_transforms(xf_a, xf_b);
    let local_b = Proxy::offset(b.points(), b.radius(), relative.p, relative.q);
    let found = d2::distance(a, &local_b, false);
    if found.state == DistanceState::Overlapped || !convex::is_unit(found.normal) {
        return Closest::Deep;
    }
    let normal = rotate(xf_a.q, found.normal);
    let on_a = transform_point(xf_a.p, xf_a.q, found.point_a) + normal * a.radius();
    let on_b = transform_point(xf_a.p, xf_a.q, found.point_b) - normal * b.radius();
    Closest::Apart {
        distance: found.distance,
        contact: Contact {
            point: on_a * 0.5 + on_b * 0.5,
            normal,
        },
    }
}

/// Where two cores stand at one time.
enum Closest {
    /// They overlap, or `f32` cannot tell them apart: no normal.
    Deep,
    /// They are `distance` apart, meeting at `contact`.
    Apart { distance: f32, contact: Contact },
}

/// The most distance queries [`passed_over`] makes for one time of
/// impact. Shapes that do not turn settle in one; a close pass of turning
/// shapes, a dozen or so.
pub(super) const MAX_GAP_CHECKS: usize = 32;

/// How deep, in slops, a contact the kernel passed over must be for the
/// check to find it: half a slop, inside the kernel's own band.
const PASSED_OVER_SLOPS: f32 = 0.5;

/// The most times the kernel searches again before an answer stands.
const MAX_SEARCHES: usize = 3;

/// What [`passed_over`] found.
enum Check {
    /// No contact as deep as it looks for.
    Clear,
    /// The shapes are that close at this fraction.
    Close(f32),
    /// It ran out of queries: clear only up to this fraction.
    Unsure(f32),
}

/// How far, at most, the turning of the shapes moves any point of their
/// surfaces per unit of fraction: each one's fastest turn times its reach
/// from the point it turns about. A sweep's blend of headings `phi` apart
/// turns at most `2 tan(phi / 2)` per unit of fraction, worked from their
/// cosine and sine.
fn turning_bound(a: &Proxy, sweep_a: &Sweep, b: &Proxy, sweep_b: &Sweep) -> f32 {
    let turning = |proxy: &Proxy, sweep: &Sweep| {
        let (q1, q2) = (sweep.q1, sweep.q2);
        let c = q1.c * q2.c + q1.s * q2.s;
        let s = q1.c * q2.s - q1.s * q2.c;
        if s == 0.0 {
            return 0.0;
        }
        let rate = if 1.0 + c > 0.0 {
            2.0 * s.abs() / (1.0 + c)
        } else {
            f32::INFINITY
        };
        let reach = proxy.points().iter().fold(0.0, |most, point| {
            space::max(most, dot(*point, *point).sqrt())
        });
        rate * (reach + proxy.radius())
    };
    turning(a, sweep_a) + turning(b, sweep_b)
}

/// The gap between the surfaces along `axis` (from A toward B) with each
/// shape's centre where its sweep puts it at `time` and its heading as it
/// was at `turned`: the least projection of B less the greatest of A.
fn gap_along(
    a: &Proxy,
    sweep_a: &Sweep,
    b: &Proxy,
    sweep_b: &Sweep,
    axis: Vec2,
    time: f32,
    turned: f32,
) -> f32 {
    let reach = |proxy: &Proxy, sweep: &Sweep, sign: f32| {
        let at = sweep_transform(sweep, time).p;
        let q = sweep_transform(sweep, turned).q;
        proxy
            .points()
            .iter()
            .fold(f32::NEG_INFINITY, |most, &point| {
                space::max(most, sign * dot(axis, transform_point(at, q, point)))
            })
            + proxy.radius()
    };
    -reach(b, sweep_b, -1.0) - reach(a, sweep_a, 1.0)
}

/// Whether the shapes come within `close` of each other between the start
/// and `until`: a contact the kernel's separating axes can pass over. The
/// kernel stops at the end of the motion when the axis it found still
/// separates the shapes there; when the shapes turn, a pair that meets and
/// parts again within the motion, a car clipping another's corner at
/// speed, is missed that way (`Box2D` says so of `b2ComputeTimeOfImpact`).
///
/// Over a stretch of the motion, take the axis of the shapes' closest
/// points at its start. Were the shapes not turning, the gap along that
/// axis would be at least the lesser of its values at the two ends (it is
/// the least of differences of straight-line motions, so concave in time),
/// and the gap between the shapes is never less than the gap along an
/// axis. Turning moves their points by at most [`turning_bound`] per unit
/// of fraction. So the gap over the stretch is at least the lesser end
/// value, with the headings held at the start, less that bound times the
/// stretch's length. A stretch where that does not clear `close` is
/// halved, its middle measured (the shapes within `close` there are
/// found), and the earlier half looked at first, at most
/// [`MAX_GAP_CHECKS`] distance queries in all. The end must be further
/// apart than `close`.
fn passed_over(
    a: &Proxy,
    sweep_a: &Sweep,
    b: &Proxy,
    sweep_b: &Sweep,
    until: f32,
    close: f32,
) -> Check {
    let turning = turning_bound(a, sweep_a, b, sweep_b);
    // The axis of the closest points at a time, or `None` when the shapes
    // are within `close` there.
    let axis_at = |time: f32| match closest(a, sweep_a, b, sweep_b, time) {
        Closest::Apart { distance, contact } if distance - a.radius() - b.radius() > close => {
            Some(contact.normal)
        }
        _ => None,
    };
    let Some(first) = axis_at(0.0) else {
        return Check::Close(0.0);
    };
    let mut stack = [(0.0f32, 0.0f32, Vec2::ZERO); MAX_GAP_CHECKS + 1];
    stack[0] = (0.0, until, first);
    let mut depth = 1;
    let mut queries = 1;
    while depth > 0 {
        depth -= 1;
        let (t0, t1, axis) = stack[depth];
        let start = gap_along(a, sweep_a, b, sweep_b, axis, t0, t0);
        let end = gap_along(a, sweep_a, b, sweep_b, axis, t1, t0);
        if space::min(start, end) - turning * (t1 - t0) > close {
            continue;
        }
        if queries >= MAX_GAP_CHECKS {
            return Check::Unsure(t0);
        }
        let middle = 0.5 * (t0 + t1);
        queries += 1;
        let Some(at_middle) = axis_at(middle) else {
            return Check::Close(middle);
        };
        stack[depth] = (middle, t1, at_middle);
        stack[depth + 1] = (t0, middle, axis);
        depth += 2;
    }
    Check::Clear
}

/// [`time_of_impact`] for two shapes on sweeps in one frame, with their
/// `slop`; the contact is in that frame.
///
/// The bodies overlap when their cores do, or their surfaces do by any
/// amount. Otherwise the kernel runs with each shape grown by the slop, so
/// its target between the cores, `max(slop, radii - slop)`, is the radii
/// plus one slop: contact a slop apart whatever the radii. Its answer is
/// then checked for a contact its separating axes passed over before it
/// ([`passed_over`]): one found makes the kernel search again up to there,
/// and a check that cannot settle makes the answer `Failed` where it
/// stopped.
///
/// The contact of a hit is the manifold's at that instant, as `Box2D`
/// meets a pair its time of impact brought together: its normal, and the
/// middle of its two points when they are within a slop of each other's
/// separation (faces meeting flat), or else its closer point (a corner
/// meeting a face). The kernel's own point is a witness of the distance
/// query, which for two parallel faces may lie anywhere along them, and a
/// bump there would spin a car that met a wall square. The kernel's point
/// and normal stand when the manifold has none.
pub(super) fn swept(
    a: &Shape<Vec2>,
    sweep_a: &Sweep,
    b: &Shape<Vec2>,
    sweep_b: &Sweep,
    slop: f32,
) -> Toi {
    if !meets(a, b) {
        return Toi::Separated;
    }
    let (core_a, core_b) = (core(a), core(b));
    match closest(&core_a, sweep_a, &core_b, sweep_b, 0.0) {
        Closest::Deep => return Toi::Overlapping,
        Closest::Apart { distance, .. } if distance < core_a.radius() + core_b.radius() => {
            return Toi::Overlapping;
        }
        Closest::Apart { .. } => {}
    }
    let grown_a = Proxy::new(core_a.points(), core_a.radius() + slop);
    let grown_b = Proxy::new(core_b.points(), core_b.radius() + slop);
    let at = |fraction: f32, kernel: Contact| {
        let pose_a = sweep_transform(sweep_a, fraction);
        let pose_b = sweep_transform(sweep_b, fraction);
        match posed(a, pose_a.p, pose_a.q, b, pose_b.p, pose_b.q) {
            Some((manifold, slop)) if convex::is_unit(manifold.normal) => Contact {
                point: manifold.centre(slop),
                normal: manifold.normal,
            },
            _ => kernel,
        }
    };
    let failed = |fraction: f32| Toi::Failed {
        fraction,
        contact: match closest(&core_a, sweep_a, &core_b, sweep_b, fraction) {
            Closest::Apart { contact, .. } => Some(at(fraction, contact)),
            Closest::Deep => None,
        },
    };

    let mut until = 1.0;
    let mut answer;
    for _ in 0..MAX_SEARCHES {
        let found = d2::time_of_impact(&grown_a, sweep_a, &grown_b, sweep_b, until, slop);
        let end = match found.state {
            ToiState::Separated => {
                answer = Toi::Separated;
                // Searched up to a contact the check found: met there.
                if until < 1.0 {
                    return match closest(&core_a, sweep_a, &core_b, sweep_b, until) {
                        Closest::Apart { contact, .. } => Toi::Hit {
                            fraction: until,
                            contact: at(until, contact),
                        },
                        Closest::Deep => failed(0.0),
                    };
                }
                1.0
            }
            ToiState::Hit => {
                answer = Toi::Hit {
                    fraction: found.fraction,
                    contact: at(
                        found.fraction,
                        Contact {
                            point: found.point,
                            normal: found.normal,
                        },
                    ),
                };
                found.fraction
            }
            // The cores met at a later axis, which the kernel only reaches
            // through rounding: nothing after the start is known to be
            // clear.
            ToiState::Overlapped => return failed(0.0),
            ToiState::Failed => {
                answer = failed(found.fraction);
                found.fraction
            }
        };
        if end <= 0.0 {
            return answer;
        }
        match passed_over(
            &core_a,
            sweep_a,
            &core_b,
            sweep_b,
            end,
            PASSED_OVER_SLOPS * slop,
        ) {
            Check::Clear => return answer,
            Check::Close(fraction) => until = fraction,
            Check::Unsure(fraction) => return failed(fraction),
        }
    }
    // Still finding contacts before each answer: clear only to the start.
    failed(0.0)
}
