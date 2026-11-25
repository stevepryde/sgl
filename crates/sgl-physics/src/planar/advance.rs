//! Several planar bodies moved together through one step, stopping at each
//! impact in time order: what a race needs each tick.

use sgl_core::math::Vec2;

use super::bump::{Response, check_response, impulse, relative_velocity};
use super::manifold::posed;
use super::toi::{core, reach, sweep_slop, swept};
use super::{Body, Contact, Toi};
use crate::convex::d2::{Rot, Sweep, cross, dot, rotate, sweep_transform};
use crate::shape::Shape;
use crate::space;

/// The most impacts one [`advance`] resolves, the cap [`Settings`] may
/// lower. Each costs a time of impact for every pair. A pile-up of ten
/// cars resolves in a few dozen; past the cap the rest of the step moves
/// without impacts and the separation sorts out what overlaps.
pub const MAX_IMPACTS: usize = 64;

/// The most passes [`advance`]'s separation makes over the pairs, the cap
/// [`Settings`] may lower. Each pass pushes every overlapping pair apart
/// once; a pile pushed into a wall needs about one pass per body in the
/// row.
pub const MAX_SEPARATION_PASSES: usize = 16;

/// How much faster than the rounding of their speeds two bodies must close
/// at a contact for [`advance`] to bump them: 64 units in the last place
/// of the speeds there. A bump leaves the pair parting or still (with no
/// restitution) to within a few units in the last place; the allowance
/// keeps that rounding from reading as a new approach at the same instant.
const APPROACH_ROUNDING: f32 = 64.0 * f32::EPSILON;

/// What the game chooses for an [`advance`]: how hard and how grippy its
/// bumps are, and the caps it may lower. Build it with [`Settings::new`]
/// and change the caps by name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// The share of the approach speed a bump gives back: 0 for none, 1
    /// for all. Finite and not negative.
    pub restitution: f32,
    /// The friction coefficient of a bump: the most its tangential impulse
    /// may be, as a share of the impulse that stopped the approach. Finite
    /// and not negative.
    pub friction: f32,
    /// The most impacts the step resolves, at most [`MAX_IMPACTS`].
    pub max_impacts: usize,
    /// The most separation passes, at most [`MAX_SEPARATION_PASSES`].
    pub max_separation_passes: usize,
}

impl Settings {
    /// `restitution` and `friction`, with the caps at their most.
    #[must_use]
    pub const fn new(restitution: f32, friction: f32) -> Self {
        Self {
            restitution,
            friction,
            max_impacts: MAX_IMPACTS,
            max_separation_passes: MAX_SEPARATION_PASSES,
        }
    }

    fn check(&self) {
        check_response(self.restitution, self.friction);
        assert!(
            self.max_impacts <= MAX_IMPACTS,
            "planar::Settings: max_impacts is at most MAX_IMPACTS"
        );
        assert!(
            self.max_separation_passes <= MAX_SEPARATION_PASSES,
            "planar::Settings: max_separation_passes is at most MAX_SEPARATION_PASSES"
        );
    }
}

/// One bump in an [`advance`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Impact {
    /// The id of the body first in body order (lower id).
    pub a: u64,
    /// The id of the other body.
    pub b: u64,
    /// When in the step, as a fraction of `dt` in `[0, 1]`.
    pub time: f32,
    /// Where, midway between the two surfaces, in world units.
    pub point: Vec2,
    /// Unit normal from body `a` toward body `b`.
    pub normal: Vec2,
    /// The normal impulse of the bump: how hard, in mass times world units
    /// per second.
    pub impulse: f32,
}

/// A body during the step: where it was at the start, how far it has
/// moved, and where its motion takes it by the end.
#[derive(Clone, Copy)]
struct Moving {
    /// Its place in the game's slice.
    index: usize,
    body: Body,
    reach: f32,
    /// Its position at the start of the step: every pair is computed
    /// relative to its first body's.
    start: Vec2,
    /// How far it has moved since the start, now and by the end.
    moved: Vec2,
    moved_by_end: Vec2,
    /// Its rotation now and by the end.
    rotation: Rot,
    rotation_by_end: Rot,
}

impl Moving {
    /// The body as it starts the step, or `None` when it is not sound or
    /// its own step is not finite: its motion, or its position plus its
    /// motion, past what `f32` holds, or its turn not finite.
    fn new(index: usize, body: &Body, dt: f32) -> Option<Self> {
        let rotation = body.sound_rotation()?;
        let mut out = Self {
            index,
            body: *body,
            reach: reach(&body.shape),
            start: body.position,
            moved: Vec2::ZERO,
            moved_by_end: Vec2::ZERO,
            rotation,
            rotation_by_end: rotation,
        };
        out.restart(dt).then_some(out)
    }

    /// Its motion over the rest of the step, in a frame at `origin` from
    /// its start.
    fn sweep(&self, origin: Vec2) -> Sweep {
        Sweep {
            local_center: Vec2::ZERO,
            c1: origin + self.moved,
            c2: origin + self.moved_by_end,
            q1: self.rotation,
            q2: self.rotation_by_end,
        }
    }

    /// Moves it `fraction` of the way along the rest of its motion, as the
    /// time of impact places it.
    fn advance(&mut self, fraction: f32) {
        let pose = sweep_transform(&self.sweep(Vec2::ZERO), fraction);
        self.moved = pose.p;
        self.rotation = pose.q;
    }

    /// Restarts its motion from where it is, with its velocity and spin,
    /// over the `left` seconds that remain, and says whether that motion is
    /// finite: its velocity and spin, the turn they give, and where it ends,
    /// from the start of the step and in world units. When it is not, the
    /// body is as it was before the call.
    fn restart(&mut self, left: f32) -> bool {
        let moved_by_end = self.moved + self.body.velocity * left;
        let turn = self.body.spin * left;
        let finite = space::is_finite(self.body.velocity)
            && turn.is_finite()
            && space::is_finite(moved_by_end)
            && space::is_finite(self.start + moved_by_end);
        if finite {
            self.moved_by_end = moved_by_end;
            self.rotation_by_end = self.rotation.integrated(turn);
        }
        finite
    }

    /// It bumped to `velocity` and `spin`, moving on with them over the
    /// `left` seconds that remain; `None` when that motion is not finite.
    fn bumped(mut self, velocity: Vec2, spin: f32, left: f32) -> Option<Self> {
        self.body.velocity = velocity;
        self.body.spin = spin;
        self.restart(left).then_some(self)
    }
}

/// The earliest impact found: which pair, when in the rest of the step,
/// and where in the frame of the pair's first body.
struct Found {
    pair: (usize, usize),
    fraction: f32,
    contact: Contact,
}

/// Moves `bodies` together through `dt` seconds, stopping at each impact in
/// time order, bumping the pair and carrying on with the time left, and
/// returns the impacts in time order.
///
/// Each step it finds the earliest [`time_of_impact`](super::time_of_impact)
/// among the pairs, advances every body to it, [`bump`](super::bump)s that
/// pair with `settings`' restitution and friction, and carries on, at most
/// `settings.max_impacts` times; the rest of the step then moves without
/// impacts. A pair counts when it approaches at the contact faster than
/// the rounding of its speeds there and can respond along the normal; a
/// pair touching at the start that is parting is passed; a pair whose time
/// of impact fails is bumped where it is known clear, when it approaches
/// there. A bump elsewhere leaves a body's own motion as it was. Then whatever
/// overlaps (pairs the step left overlapping or that overlapped at the
/// start) is pushed apart along its [`manifold`](super::manifold) to a slop
/// apart, split by inverse mass, pair by pair in passes, at most
/// `settings.max_separation_passes`, stopping after a pass that pushed
/// nothing; a pair with no direction between them (two balls at one point)
/// is pushed apart along +x, the one first in body order toward −x. A body
/// with zero inverse mass is never pushed; one with zero inverse mass and
/// inertia is never bumped either. Each body ends at its
/// position, heading (normalised and turned), velocity and spin.
///
/// `collide(a, b)` says whether a pair can collide at all: a game returns
/// `false` for a pair apart in height (a bridge over a road, an airborne
/// car clearing a low prop). It is called with the bodies as they were at
/// the start of the step, first in body order first, perhaps several times
/// for one pair, and must answer the same each time. `|_, _| true` meets
/// everything.
///
/// Order never shows: bodies are processed in body order (id, then their
/// numbers), ties between impacts at the same instant go to the pair first
/// in that order, and a shuffled slice gives the same bodies and impacts,
/// bit for bit. Every pair is computed relative to its first body's start,
/// so far from the origin is as good as near. A body that is not sound is
/// left as it is, and so is one whose own step `f32` cannot hold (its
/// position plus its motion, its turn, or its velocity, spin or end after a
/// bump), which takes no further part in the step: no body is ever made
/// non-finite by another.
///
/// Guaranteed: impacts in time order; a body with zero inverse mass moves
/// only by its own velocity; and when the separation ends before its cap,
/// no pair overlaps by more than rounding. Not guaranteed: stacking,
/// joints, resting contact (a pair pressed together is bumped once a step
/// and pushed apart when it sinks in), or a pair whose time of impact
/// fails or that the caps leave overlapping. Every pair is tested, which
/// suits the tens of bodies a race has: a step runs at most
/// `(max_impacts + 1) * n * (n - 1) / 2` times of impact and
/// `max_separation_passes * n * (n - 1) / 2` manifolds for `n` bodies, and
/// allocates only for the bodies and the impacts.
///
/// A `dt` that is not positive and finite changes nothing.
///
/// # Panics
/// If `settings` is not valid: a negative or non-finite restitution or
/// friction, or a cap above its maximum.
pub fn advance(
    bodies: &mut [Body],
    dt: f32,
    settings: &Settings,
    collide: impl Fn(&Body, &Body) -> bool,
) -> Vec<Impact> {
    settings.check();
    let mut impacts = Vec::new();
    if !(dt.is_finite() && dt > 0.0) {
        return impacts;
    }
    let mut order: Vec<usize> = (0..bodies.len()).collect();
    order.sort_by(|&i, &j| bodies[i].order(&bodies[j]));
    let mut moving: Vec<Moving> = order
        .iter()
        .filter_map(|&index| Moving::new(index, &bodies[index], dt))
        .collect();
    let collide = |a: &Moving, b: &Moving| collide(&bodies[a.index], &bodies[b.index]);

    // The fraction of the step done.
    let mut now = 0.0f32;
    for _ in 0..settings.max_impacts {
        let Some(found) = earliest(&moving, &collide) else {
            break;
        };
        for body in &mut moving {
            body.advance(found.fraction);
        }
        now += found.fraction * (1.0 - now);
        let left = (1.0 - now) * dt;
        let (i, j) = found.pair;
        let (a, b) = (moving[i], moving[j]);
        let rel = b.start - a.start;
        let bumped = impulse(
            &Response::of(&a.body),
            &Response::of(&b.body),
            found.contact.point - a.moved,
            found.contact.point - (rel + b.moved),
            found.contact.normal,
            settings.restitution,
            settings.friction,
        );
        // A bump that leaves a body with a motion `f32` cannot hold is not
        // applied: that body is left as it was and takes no further part,
        // and the other keeps its own motion.
        match (
            moving[i].bumped(bumped.velocity_a, bumped.spin_a, left),
            moving[j].bumped(bumped.velocity_b, bumped.spin_b, left),
        ) {
            (Some(after_a), Some(after_b)) => {
                moving[i] = after_a;
                moving[j] = after_b;
            }
            (after_a, after_b) => {
                if after_b.is_none() {
                    moving.remove(j);
                }
                if after_a.is_none() {
                    moving.remove(i);
                }
                continue;
            }
        }
        impacts.push(Impact {
            a: a.body.id,
            b: b.body.id,
            time: now,
            point: a.start + found.contact.point,
            normal: found.contact.normal,
            impulse: bumped.normal_impulse,
        });
    }
    for body in &mut moving {
        body.moved = body.moved_by_end;
        body.rotation = body.rotation_by_end;
    }

    separate(&mut moving, settings.max_separation_passes, &collide);

    for body in &moving {
        let out = &mut bodies[body.index];
        out.position = body.start + body.moved;
        out.heading = Vec2::new(body.rotation.c, body.rotation.s);
        out.velocity = body.body.velocity;
        out.spin = body.body.spin;
    }
    impacts
}

/// The earliest impact over the rest of the step among the pairs that may
/// collide, the first pair in body order among equals.
fn earliest(moving: &[Moving], collide: &impl Fn(&Moving, &Moving) -> bool) -> Option<Found> {
    let mut best: Option<Found> = None;
    for (i, a) in moving.iter().enumerate() {
        for (j, b) in moving.iter().enumerate().skip(i + 1) {
            if best.as_ref().is_some_and(|best| best.fraction == 0.0) {
                // Nothing comes before the start, and ties go to the first.
                return best;
            }
            if (a.body.fixed() && b.body.fixed()) || !collide(a, b) {
                continue;
            }
            let rel = b.start - a.start;
            let (sweep_a, sweep_b) = (a.sweep(Vec2::ZERO), b.sweep(rel));
            let slop = sweep_slop(&a.body.shape, &sweep_a, &b.body.shape, &sweep_b);
            if apart_throughout(a, &sweep_a, b, &sweep_b, slop) {
                continue;
            }
            let (Toi::Hit { fraction, contact }
            | Toi::Failed {
                fraction,
                contact: Some(contact),
            }) = swept(&a.body.shape, &sweep_a, &b.body.shape, &sweep_b, slop)
            else {
                continue;
            };
            if best.as_ref().is_some_and(|best| fraction >= best.fraction) {
                continue;
            }
            if approaching(a, &sweep_a, b, &sweep_b, fraction, &contact) {
                best = Some(Found {
                    pair: (i, j),
                    fraction,
                    contact,
                });
            }
        }
    }
    best
}

/// Whether the circles that hold each body over the rest of its motion
/// are more than two slops apart: then no contact is possible, the time of
/// impact's band being at most one and a quarter.
fn apart_throughout(a: &Moving, sweep_a: &Sweep, b: &Moving, sweep_b: &Sweep, slop: f32) -> bool {
    let bound = |body: &Moving, sweep: &Sweep| {
        let half = (sweep.c2 - sweep.c1) * 0.5;
        (sweep.c1 + half, body.reach + dot(half, half).sqrt())
    };
    let (centre_a, radius_a) = bound(a, sweep_a);
    let (centre_b, radius_b) = bound(b, sweep_b);
    let gap = centre_b - centre_a;
    let reach = radius_a + radius_b + 2.0 * slop;
    dot(gap, gap) > reach * reach
}

/// Whether the pair, placed at `fraction` of its sweeps, closes at the
/// contact faster than the rounding of the speeds there, and has an
/// effective mass along the normal to respond with.
fn approaching(
    a: &Moving,
    sweep_a: &Sweep,
    b: &Moving,
    sweep_b: &Sweep,
    fraction: f32,
    contact: &Contact,
) -> bool {
    let r_a = contact.point - sweep_transform(sweep_a, fraction).p;
    let r_b = contact.point - sweep_transform(sweep_b, fraction).p;
    let (v_a, w_a) = (a.body.velocity, a.body.spin);
    let (v_b, w_b) = (b.body.velocity, b.body.spin);
    let closing = dot(
        contact.normal,
        relative_velocity(v_a, w_a, r_a, v_b, w_b, r_b),
    );
    let one_norm = |v: Vec2| v.x.abs() + v.y.abs();
    let speeds =
        one_norm(v_a) + one_norm(v_b) + w_a.abs() * one_norm(r_a) + w_b.abs() * one_norm(r_b);
    let (lever_a, lever_b) = (cross(r_a, contact.normal), cross(r_b, contact.normal));
    let responds = a.body.inverse_mass
        + b.body.inverse_mass
        + a.body.inverse_inertia * lever_a * lever_a
        + b.body.inverse_inertia * lever_b * lever_b
        > 0.0;
    responds && closing < -APPROACH_ROUNDING * speeds
}

/// Pushes overlapping pairs apart along their manifolds to a slop apart,
/// split by inverse mass, in passes over the pairs in body order, stopping
/// after a pass that pushed nothing.
fn separate(moving: &mut [Moving], passes: usize, collide: &impl Fn(&Moving, &Moving) -> bool) {
    for _ in 0..passes {
        let mut pushed = false;
        for i in 0..moving.len() {
            for j in i + 1..moving.len() {
                let (a, b) = (moving[i], moving[j]);
                let total = a.body.inverse_mass + b.body.inverse_mass;
                if total <= 0.0 || !collide(&a, &b) {
                    continue;
                }
                let at_b = (b.start - a.start) + b.moved;
                let gap = at_b - a.moved;
                if space::largest_abs(gap) > a.reach + b.reach {
                    continue;
                }
                let Some((found, slop)) = posed(
                    &a.body.shape,
                    a.moved,
                    a.rotation,
                    &b.body.shape,
                    at_b,
                    b.rotation,
                ) else {
                    continue;
                };
                if found.depth <= 0.0 {
                    continue;
                }
                let push = if found.normal == Vec2::ZERO {
                    // No direction between them (two balls whose centres
                    // coincide): apart along +x, the first in body order
                    // toward -x, by how far they overlap along x.
                    let (_, a_most) = span_x(&a.body.shape, a.moved, a.rotation);
                    let (b_least, _) = span_x(&b.body.shape, at_b, b.rotation);
                    Vec2::new(a_most - b_least + slop, 0.0)
                } else {
                    found.normal * (found.depth + slop)
                };
                let moved_a = a.moved - push * (a.body.inverse_mass / total);
                let moved_b = b.moved + push * (b.body.inverse_mass / total);
                if !(space::is_finite(a.start + moved_a) && space::is_finite(b.start + moved_b)) {
                    // A push `f32` cannot hold leaves the pair where it is.
                    continue;
                }
                moving[i].moved = moved_a;
                moving[j].moved = moved_b;
                pushed = true;
            }
        }
        if !pushed {
            break;
        }
    }
}

/// The least and greatest x a shape reaches with its centre at `at`,
/// turned by `rotation`.
fn span_x(shape: &Shape<Vec2>, at: Vec2, rotation: Rot) -> (f32, f32) {
    let proxy = core(shape);
    let (least, most) =
        proxy
            .points()
            .iter()
            .fold((f32::MAX, -f32::MAX), |(least, most), point| {
                let x = rotate(rotation, *point).x;
                (space::min(least, x), space::max(most, x))
            });
    (at.x + least - proxy.radius(), at.x + most + proxy.radius())
}
