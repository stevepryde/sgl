//! What a bump leaves two planar bodies with: the single-pair sequential
//! impulse of `Box2D`'s contact solver, applied once.
//!
//! Written from `Box2D` v3.2.0 (commit
//! `178495ba35695aefe9c321e569d2277f06db3b90`), `src/contact_solver.c`
//! (the normal and friction rows of `b2PrepareContacts_Overflow` and
//! `b2SolveContacts_Overflow`, and the velocity target of
//! `b2ApplyRestitution_Overflow`), with `b2CrossSV`, `b2RightPerp`,
//! `b2MaxFloat` and `b2ClampFloat` of `include/box2d/math_functions.h`.
//! Copyright 2026 Erin Catto, MIT: `LICENSE-box2d.txt` beside this
//! crate's manifest. Departures and algorithm notes are in
//! `PROVENANCE.md`.

// Box2D's contact solver names the two bodies A and B and their anchors,
// velocities and spins r, v and w; the bump keeps those names so it reads
// against it.
#![allow(clippy::similar_names)]

use sgl_core::math::Vec2;

use super::{Body, Contact};
use crate::convex::d2::{self, cross, cross_sv, dot};
use crate::space;

/// What a bump leaves two bodies with: their velocities and spins, and the
/// impulses that did it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bump {
    /// Body `a`'s velocity after the bump.
    pub velocity_a: Vec2,
    /// Body `a`'s spin after the bump.
    pub spin_a: f32,
    /// Body `b`'s velocity after the bump.
    pub velocity_b: Vec2,
    /// Body `b`'s spin after the bump.
    pub spin_b: f32,
    /// The impulse along the normal, pushing the bodies apart: zero when
    /// they were not approaching. In mass times world units per second.
    pub normal_impulse: f32,
    /// The friction impulse along the tangent (the normal turned a quarter
    /// turn clockwise), at most `friction` times `normal_impulse`.
    pub friction_impulse: f32,
}

/// The velocities and spins `a` and `b` leave a bump at `contact` with:
/// one sequential impulse of `Box2D`'s contact solver, applied once, without
/// warm starting.
///
/// When the bodies approach at the contact point along the normal, an
/// impulse along it reverses `restitution` of the approach (1 bounces back
/// at full speed, 0 stops it); friction then acts along the tangent, at
/// most `friction` times that impulse; and should friction, through the
/// spins, have turned some of the motion back into an approach, a last
/// impulse along the normal stops it. The normal impulse only ever pushes,
/// and friction never exceeds `friction` times it. Linear and angular
/// momentum are kept; kinetic energy is too at `restitution` 1 without
/// friction, and never grows for `restitution` at most 1. The bodies are
/// left not approaching there. Bodies already parting are left as they
/// are, and so is a body with zero inverse mass and inertia.
///
/// Pure: the game applies the result, scales it or ignores it. The normal
/// is normalised first; a zero one, or a body that is not sound, changes
/// nothing.
///
/// # Panics
/// If `restitution` or `friction` is negative or not finite: an authoring
/// error.
#[must_use]
pub fn bump(a: &Body, b: &Body, contact: Contact, restitution: f32, friction: f32) -> Bump {
    check_response(restitution, friction);
    let unchanged = Bump {
        velocity_a: a.velocity,
        spin_a: a.spin,
        velocity_b: b.velocity,
        spin_b: b.spin,
        normal_impulse: 0.0,
        friction_impulse: 0.0,
    };
    let normal = d2::normalize(contact.normal);
    let sound = a.sound_rotation().is_some()
        && b.sound_rotation().is_some()
        && space::is_finite(contact.point)
        && normal != Vec2::ZERO;
    if !sound {
        return unchanged;
    }
    impulse(
        &Response::of(a),
        &Response::of(b),
        contact.point - a.position,
        contact.point - b.position,
        normal,
        restitution,
        friction,
    )
}

/// Panics unless `restitution` and `friction` are finite and not negative.
pub(super) fn check_response(restitution: f32, friction: f32) {
    assert!(
        restitution.is_finite() && restitution >= 0.0,
        "planar: restitution must be finite and not negative"
    );
    assert!(
        friction.is_finite() && friction >= 0.0,
        "planar: friction must be finite and not negative"
    );
}

/// What a bump reads of a body.
#[derive(Clone, Copy)]
pub(super) struct Response {
    velocity: Vec2,
    spin: f32,
    inverse_mass: f32,
    inverse_inertia: f32,
}

impl Response {
    pub(super) fn of(body: &Body) -> Self {
        Self {
            velocity: body.velocity,
            spin: body.spin,
            inverse_mass: body.inverse_mass,
            inverse_inertia: body.inverse_inertia,
        }
    }
}

/// The relative velocity of `b`'s point at `r_b` past `a`'s at `r_a`.
pub(super) fn relative_velocity(
    v_a: Vec2,
    w_a: f32,
    r_a: Vec2,
    v_b: Vec2,
    w_b: f32,
    r_b: Vec2,
) -> Vec2 {
    let vr_a = v_a + cross_sv(w_a, r_a);
    let vr_b = v_b + cross_sv(w_b, r_b);
    vr_b - vr_a
}

/// The single-pair sequential impulse at a contact `r_a` from `a`'s centre
/// and `r_b` from `b`'s, along the unit `normal` from `a` toward `b`, from
/// the rows of `Box2D`'s contact solver (`b2PrepareContacts_Overflow`,
/// `b2SolveContacts_Overflow`): the normal row with restitution's velocity
/// target (`b2ApplyRestitution_Overflow`'s bias) folded into it, the
/// friction row clamped by that impulse, then the normal row once more
/// without restitution, which only adds. Once, with no warm start,
/// softness, speculative bias, rolling resistance, tangent speed or
/// restitution threshold, and each impulse applied as it is found.
pub(super) fn impulse(
    a: &Response,
    b: &Response,
    r_a: Vec2,
    r_b: Vec2,
    normal: Vec2,
    restitution: f32,
    friction: f32,
) -> Bump {
    let (m_a, i_a) = (a.inverse_mass, a.inverse_inertia);
    let (m_b, i_b) = (b.inverse_mass, b.inverse_inertia);
    let (mut v_a, mut w_a) = (a.velocity, a.spin);
    let (mut v_b, mut w_b) = (b.velocity, b.spin);
    // b2RightPerp
    let tangent = Vec2::new(normal.y, -normal.x);

    let rn_a = cross(r_a, normal);
    let rn_b = cross(r_b, normal);
    let k_normal = m_a + m_b + i_a * rn_a * rn_a + i_b * rn_b * rn_b;
    let normal_mass = if k_normal > 0.0 { 1.0 / k_normal } else { 0.0 };

    let rt_a = cross(r_a, tangent);
    let rt_b = cross(r_b, tangent);
    let k_tangent = m_a + m_b + i_a * rt_a * rt_a + i_b * rt_b * rt_b;
    let tangent_mass = if k_tangent > 0.0 {
        1.0 / k_tangent
    } else {
        0.0
    };

    // The approach before the bump.
    let relative = dot(normal, relative_velocity(v_a, w_a, r_a, v_b, w_b, r_b));

    let mut apply = |p: Vec2| {
        v_a = Vec2::new(v_a.x - m_a * p.x, v_a.y - m_a * p.y);
        w_a -= i_a * cross(r_a, p);
        v_b = Vec2::new(v_b.x + m_b * p.x, v_b.y + m_b * p.y);
        w_b += i_b * cross(r_b, p);
        (v_a, w_a, v_b, w_b)
    };

    // Non-penetration with restitution: an approach becomes a parting at
    // `restitution` of its speed. A parting pair is left alone.
    let velocity_bias = if relative < 0.0 {
        restitution * relative
    } else {
        0.0
    };
    let mut normal_impulse = max_float(-normal_mass * (relative + velocity_bias), 0.0);
    let (v_a1, w_a1, v_b1, w_b1) = apply(normal * normal_impulse);

    // Friction, clamped by the normal impulse.
    let vt = dot(relative_velocity(v_a1, w_a1, r_a, v_b1, w_b1, r_b), tangent);
    let max_friction = friction * normal_impulse;
    let friction_impulse = clamp(-tangent_mass * vt, -max_friction, max_friction);
    let (v_a2, w_a2, v_b2, w_b2) = apply(tangent * friction_impulse);

    // Non-penetration once more: friction acts through the spins on the
    // normal too, and must not leave the pair approaching. Only a push.
    let vn = dot(relative_velocity(v_a2, w_a2, r_a, v_b2, w_b2, r_b), normal);
    let extra = max_float(-normal_mass * vn, 0.0);
    normal_impulse += extra;
    let (velocity_a, spin_a, velocity_b, spin_b) = apply(normal * extra);

    Bump {
        velocity_a,
        spin_a,
        velocity_b,
        spin_b,
        normal_impulse,
        friction_impulse,
    }
}

/// `b2MaxFloat`.
fn max_float(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

/// `b2ClampFloat`.
fn clamp(a: f32, lower: f32, upper: f32) -> f32 {
    if a < lower {
        lower
    } else if a > upper {
        upper
    } else {
        a
    }
}
