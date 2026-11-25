//! Audit of planar bumps (#924, physics.md 20), written from the contract:
//! typed outcomes checked against the shapes' own definitions sampled
//! along the motion the contract describes, `advance` against the physical
//! laws and the shapes in `f64`, and hostile bodies. Every expected value
//! comes from geometry worked here or a sentence of the contract, never
//! from the crate. Runs natively and in a wasm runtime.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::too_many_lines
)]

mod audit_support;

use audit_support::Rng;
use audit_support::flat::{self, P, Placed, length, place, separation};
use sgl_core::math::Vec2;
use sgl_physics::planar::{self, Body, Contact, Motion, Settings, Toi};
use sgl_physics::{Convex, Shape};
use wasm_bindgen_test::wasm_bindgen_test;

fn v2(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

/// A random 2D shape of play size: a box, a ball, a capsule turned any
/// way, or a convex polygon of three to eight vertices.
fn shape(rng: &mut Rng) -> Shape<Vec2> {
    match rng.below(4) {
        0 => Shape::Box {
            half: v2(rng.range(0.3, 1.5), rng.range(0.3, 1.0)),
        },
        1 => Shape::Ball {
            radius: rng.range(0.2, 1.0),
        },
        2 => Shape::Capsule {
            half_axis: v2(rng.range(-0.8, 0.8), rng.range(-0.8, 0.8)),
            radius: rng.range(0.1, 0.5),
        },
        _ => {
            let count = rng.int(3, 8) as usize;
            let radius = rng.range(0.4, 1.0);
            let step = core::f32::consts::TAU / count as f32;
            let points: Vec<Vec2> = (0..count)
                .map(|i| {
                    let angle = step * (i as f32 + rng.range(0.0, 0.45));
                    v2(angle.cos(), angle.sin()) * radius
                })
                .collect();
            Shape::Convex(Convex::polygon(&points, 1.0, 0.0))
        }
    }
}

fn heading(rng: &mut Rng) -> Vec2 {
    let angle = rng.range(-3.1, 3.1);
    v2(angle.cos(), angle.sin())
}

/// A free body of unit-order mass, with the inertia of its enclosing box.
fn body(rng: &mut Rng, id: u64, at: Vec2) -> Body {
    let shape = shape(rng);
    let mass = rng.range(0.5, 3.0);
    Body {
        id,
        shape,
        position: at,
        heading: heading(rng),
        velocity: rng.vector(-12.0, 12.0),
        spin: rng.range(-4.0, 4.0),
        inverse_mass: 1.0 / mass,
        inverse_inertia: planar::box_inverse_inertia(mass, shape.half_extents()),
    }
}

/// The unit rotation a heading names, in `f64` (physics.md 20: normalised
/// before use).
fn unit(h: Vec2) -> P {
    let q = flat::p(h);
    let l = length(q);
    (q.0 / l, q.1 / l)
}

/// The contract's turn of a heading by `angle` (physics.md 20, Turning):
/// `Box2D`'s normalised first-order step.
fn turned(q: P, angle: f64) -> P {
    let (c, s) = (q.0 - angle * q.1, q.1 + angle * q.0);
    let l = (c * c + s * s).sqrt();
    (c / l, s / l)
}

/// Where a body is at `t` of a motion, by the contract: its centre moves
/// straight, its heading is the normalised blend of its start and end.
fn at(body: &Body, motion: Motion, t: f64) -> Placed {
    let q0 = unit(body.heading);
    let q1 = turned(q0, f64::from(motion.turn));
    let blend = flat::add(flat::scale(q0, 1.0 - t), flat::scale(q1, t));
    let l = length(blend);
    let centre = flat::add(
        flat::p(body.position),
        flat::scale(flat::p(motion.translation), t),
    );
    place(&body.shape, centre, (blend.0 / l, blend.1 / l))
}

/// The contract's slop for a pair (physics.md 19 and 20): a hundredth of
/// the smaller shape's extent (its largest half-extent), at most a quarter
/// of the smaller radius. The rounding floor is far below it at these
/// sizes.
fn slop(a: &Shape<Vec2>, b: &Shape<Vec2>) -> f64 {
    let extent = |s: &Shape<Vec2>| {
        let h = s.half_extents();
        f64::from(h.x.max(h.y))
    };
    let radius = |s: &Shape<Vec2>| match *s {
        Shape::Ball { radius } | Shape::Capsule { radius, .. } => f64::from(radius),
        _ => 0.0,
    };
    let mut out = extent(a).min(extent(b)) / 100.0;
    for r in [radius(a), radius(b)] {
        if r > 0.0 {
            out = out.min(r / 4.0);
        }
    }
    out
}

/// Defect: a heading of a sound direction that the planar functions take
/// for none, so the body is left out of every impact (or one whose length
/// overflows read as unsound), because its length is found by squaring
/// first. Oracle: physics.md 12 (every direction is normalised by
/// dividing by its largest component before squaring, so neither
/// underflow nor overflow occurs) and 20 (a heading is normalised before
/// use; only one with no direction occupies nothing). A heading scaled by
/// any finite factor names the same direction: a scene whose car heads
/// along `(1e-30, 0)`, `(2, 0)` or `(3e38, 0)` moves as the one heading
/// along `(1, 0)`, and one heading along `(3e38, 3e38)` or `(1e-30,
/// 1e-30)` as the one heading along `(1, 1)`, bit for bit, impacts
/// included.
#[wasm_bindgen_test(unsupported = test)]
fn a_heading_of_any_finite_length_is_a_direction() {
    let car = |heading: Vec2| Body {
        id: 1,
        shape: Shape::Box { half: v2(2.0, 1.0) },
        position: v2(0.0, 0.0),
        heading,
        velocity: v2(30.0, 0.0),
        spin: 0.5,
        inverse_mass: 1.0 / 1200.0,
        inverse_inertia: planar::box_inverse_inertia(1200.0, v2(2.0, 1.0)),
    };
    let parked = Body {
        id: 2,
        shape: Shape::Box { half: v2(2.0, 1.0) },
        position: v2(5.0, 0.5),
        heading: v2(1.0, 0.0),
        velocity: Vec2::ZERO,
        spin: 0.0,
        inverse_mass: 1.0 / 1500.0,
        inverse_inertia: planar::box_inverse_inertia(1500.0, v2(2.0, 1.0)),
    };
    let run = |heading: Vec2| {
        let mut bodies = [car(heading), parked];
        let impacts = planar::advance(&mut bodies, 0.1, &Settings::new(0.3, 0.5), |_, _| true);
        let toi = planar::time_of_impact(
            &car(heading),
            Motion::of(&car(heading), 0.1),
            &parked,
            Motion::of(&parked, 0.1),
        );
        // The heading out is normalised and turned: compare its direction.
        let out = bodies.map(|b| (b.position, unit(b.heading), b.velocity, b.spin));
        format!("{out:?} {impacts:?} {toi:?}")
    };
    let along_x = run(v2(1.0, 0.0));
    assert!(
        along_x.contains("Impact"),
        "the scene has an impact: {along_x}"
    );
    for scaled in [v2(1e-30, 0.0), v2(2.0, 0.0), v2(3e38, 0.0)] {
        assert_eq!(run(scaled), along_x, "heading {scaled}");
    }
    let diagonal = run(v2(1.0, 1.0));
    for scaled in [v2(3e38, 3e38), v2(1e-30, 1e-30)] {
        assert_eq!(run(scaled), diagonal, "heading {scaled}");
    }
}

/// Defect: a typed outcome that hides a contact: `Separated` for bodies
/// that come within the band, a `Hit` whose bodies are not within the
/// band at its fraction or that passed a deeper contact before it, a
/// `Failed` not clear before its fraction, or `Overlapping` for bodies
/// apart. Oracle: the shapes' own definitions in `f64` sampled along the
/// motion the contract describes (physics.md 20: the centre moves
/// straight, the heading is the normalised blend of its start and end,
/// turned by `Box2D`'s step), and its sentences: contact is the surfaces
/// within 0.75 to 1.25 slops; bodies within 1.25 slops at the start hit
/// at zero; a contact deeper than half a slop is found; `Failed` is clear
/// up to its fraction; `Overlapping` is cores or surfaces overlapping at
/// the start. 300 seeded pairs of every shape, moving up to 12 units and
/// turning up to 1.5 radians each.
#[wasm_bindgen_test(unsupported = test)]
fn a_time_of_impact_never_hides_a_contact() {
    const SAMPLES: usize = 400;
    let mut rng = Rng::new(0x0924_2001);
    let mut kinds = [0usize; 4];
    for case in 0..300 {
        let (at_a, at_b) = (rng.vector(-3.0, 3.0), rng.vector(-3.0, 3.0));
        let a = body(&mut rng, 1, at_a);
        let b = body(&mut rng, 2, at_b);
        let motion = |rng: &mut Rng| Motion {
            translation: rng.vector(-12.0, 12.0),
            turn: rng.range(-1.5, 1.5),
        };
        let (ma, mb) = (motion(&mut rng), motion(&mut rng));
        let slop = slop(&a.shape, &b.shape);
        // f32 positions and headings round at about 1e-6 here.
        let tol = 2e-5;
        let gap = |t: f64| separation(&at(&a, ma, t), &at(&b, mb, t));
        let least_before = |end: f64| {
            (0..=SAMPLES)
                .map(|i| end * i as f64 / SAMPLES as f64)
                .map(gap)
                .fold(f64::INFINITY, f64::min)
        };
        let start = gap(0.0);
        let found = planar::time_of_impact(&a, ma, &b, mb);
        let why = || format!("case {case}: {a:?} {ma:?} {b:?} {mb:?}: {found:?}, start {start}");
        match found {
            Toi::Separated => {
                kinds[0] += 1;
                assert!(start > 1.25 * slop - tol, "{}", why());
                assert!(least_before(1.0) > 0.25 * slop, "{}", why());
            }
            Toi::Hit { fraction, contact } => {
                kinds[1] += 1;
                let t = f64::from(fraction);
                assert!((0.0..=1.0).contains(&t), "{}", why());
                let there = gap(t);
                assert!(
                    there > 0.75 * slop - tol && there < 1.25 * slop + tol,
                    "{}: {there} apart, slop {slop}",
                    why()
                );
                assert!(least_before(t) > 0.25 * slop - tol, "{}", why());
                assert!(
                    (length(flat::p(contact.normal)) - 1.0).abs() < 1e-5,
                    "{}",
                    why()
                );
            }
            Toi::Failed { fraction, .. } => {
                kinds[2] += 1;
                assert!(least_before(f64::from(fraction)) > -tol, "{}", why());
            }
            Toi::Overlapping => {
                kinds[3] += 1;
                assert!(start < tol, "{}", why());
            }
        }
        if start < -tol {
            assert_eq!(found, Toi::Overlapping, "{}", why());
        }
    }
    assert!(
        kinds[0] >= 30 && kinds[1] >= 30 && kinds[3] >= 10,
        "outcomes (separated, hit, failed, overlapping): {kinds:?}"
    );
}

/// Kinetic energy, linear momentum and mass-weighted position of free
/// bodies, in `f64`.
fn totals(bodies: &[Body]) -> (f64, P, P) {
    let mut energy = 0.0;
    let mut momentum = (0.0, 0.0);
    let mut weighted = (0.0, 0.0);
    for b in bodies {
        let m = 1.0 / f64::from(b.inverse_mass);
        let i = 1.0 / f64::from(b.inverse_inertia);
        let v = flat::p(b.velocity);
        let w = f64::from(b.spin);
        energy += 0.5 * m * flat::dot(v, v) + 0.5 * i * w * w;
        momentum = flat::add(momentum, flat::scale(v, m));
        weighted = flat::add(weighted, flat::scale(flat::p(b.position), m));
    }
    (energy, momentum, weighted)
}

/// A random scene of `n` free bodies scattered over a small arena, so
/// several meet within a step.
fn scene(rng: &mut Rng, n: u64) -> Vec<Body> {
    (0..n)
        .map(|id| {
            let at = rng.vector(-6.0, 6.0);
            body(rng, id * 3 + 1, at)
        })
        .collect()
}

/// Defect: an `advance` that makes or loses momentum (an impulse applied
/// to one body only, or twice), that adds energy, or whose separation is
/// not split by inverse mass, so the pile drifts. Oracle: the physical
/// laws in `f64` and physics.md 20 (only bumps change velocities, each
/// keeping momentum and never adding energy at restitution at most 1;
/// keeping it at 1 without friction; the separation is split by inverse
/// mass). So over a step: total momentum is kept, kinetic energy never
/// grows (and is kept at restitution 1 without friction), and the
/// mass-weighted centre moves by the total momentum times the step,
/// whatever the impacts and pushes. 40 seeded scenes of eight bodies.
#[wasm_bindgen_test(unsupported = test)]
fn advance_keeps_momentum_and_the_centre_of_mass_and_adds_no_energy() {
    let mut rng = Rng::new(0x0924_2002);
    let mut bumped = 0;
    for case in 0..40 {
        let mut bodies = scene(&mut rng, 8);
        let elastic = case % 3 == 0;
        let settings = if elastic {
            Settings::new(1.0, 0.0)
        } else {
            Settings::new(rng.range(0.0, 1.0), rng.range(0.0, 1.2))
        };
        let dt = 0.25;
        let (e0, p0, x0) = totals(&bodies);
        let impacts = planar::advance(&mut bodies, dt, &settings, |_, _| true);
        bumped += impacts.len();
        let (e1, p1, x1) = totals(&bodies);
        let speed = bodies
            .iter()
            .map(|b| length(flat::p(b.velocity)) / f64::from(b.inverse_mass))
            .sum::<f64>()
            .max(1.0);
        let why = || format!("case {case}: {} impacts, {settings:?}", impacts.len());
        assert!(
            length(flat::sub(p1, p0)) < 1e-5 * speed,
            "momentum: {}",
            why()
        );
        assert!(
            e1 <= e0 * (1.0 + 1e-5),
            "energy grew {e0} -> {e1}: {}",
            why()
        );
        if elastic {
            assert!(
                (e1 - e0).abs() <= 1e-5 * e0,
                "energy {e0} -> {e1}: {}",
                why()
            );
        }
        let drift = flat::sub(x1, flat::add(x0, flat::scale(p0, f64::from(dt))));
        assert!(
            length(drift) < 1e-4 * speed,
            "centre drift {drift:?}: {}",
            why()
        );
    }
    assert!(bumped >= 40, "{bumped} impacts over the scenes");
}

/// Defect: an `advance` that reports its separation finished while it
/// leaves a pair overlapping (a pass that stops early, a push split the
/// wrong way, a manifold depth too short). Oracle: physics.md 20
/// (Guaranteed: when the separation ends before its cap, no pair overlaps
/// by more than rounding) and the shapes in `f64`. Whether the separation
/// ended before its cap is read without the crate's help: when a step
/// capped at 8 passes and one capped at 16 end bit-equal, passes 9 to 16
/// pushed nothing, so it ended before 16. Scenes of eight bodies packed
/// into a small arena, many overlapping at the start.
#[wasm_bindgen_test(unsupported = test)]
fn advance_leaves_nothing_overlapping_when_its_separation_ends_before_the_cap() {
    let mut rng = Rng::new(0x0924_2003);
    let mut checked = 0;
    for case in 0..40 {
        let start: Vec<Body> = (0..8)
            .map(|id| {
                let at = rng.vector(-2.5, 2.5);
                body(&mut rng, id + 1, at)
            })
            .collect();
        let run = |passes: usize| {
            let mut bodies = start.clone();
            let settings = Settings {
                max_separation_passes: passes,
                ..Settings::new(0.2, 0.4)
            };
            planar::advance(&mut bodies, 1.0 / 60.0, &settings, |_, _| true);
            bodies
        };
        let (eight, sixteen) = (run(8), run(16));
        if eight != sixteen {
            continue;
        }
        checked += 1;
        let placed: Vec<Placed> = sixteen
            .iter()
            .map(|b| place(&b.shape, flat::p(b.position), unit(b.heading)))
            .collect();
        for i in 0..placed.len() {
            for j in i + 1..placed.len() {
                let largest = flat::largest(&placed[i]).max(flat::largest(&placed[j]));
                let rounding = 64.0 * f64::from(f32::EPSILON) * largest.max(1.0);
                let gap = separation(&placed[i], &placed[j]);
                assert!(
                    gap > -rounding,
                    "case {case}: bodies {} and {} overlap by {}",
                    sixteen[i].id,
                    sixteen[j].id,
                    -gap
                );
            }
        }
    }
    assert!(checked >= 25, "{checked} scenes ended their separation");
}

/// Bodies the contract says occupy nothing (physics.md 20, Bodies): a
/// non-finite number anywhere, a negative inverse mass or inertia, a
/// heading with no direction, a shape that occupies nothing.
fn unsound(at: Vec2) -> Vec<Body> {
    let good = Body {
        id: 7,
        shape: Shape::Box { half: v2(1.0, 0.5) },
        position: at,
        heading: v2(1.0, 0.0),
        velocity: v2(-20.0, 0.0),
        spin: 1.0,
        inverse_mass: 1.0,
        inverse_inertia: 1.0,
    };
    vec![
        Body {
            inverse_mass: -1.0,
            ..good
        },
        Body {
            inverse_inertia: -0.5,
            ..good
        },
        Body {
            inverse_mass: f32::INFINITY,
            ..good
        },
        Body {
            inverse_inertia: f32::NAN,
            ..good
        },
        Body {
            position: v2(f32::NAN, at.y),
            ..good
        },
        Body {
            velocity: v2(0.0, f32::INFINITY),
            ..good
        },
        Body {
            spin: f32::NEG_INFINITY,
            ..good
        },
        Body {
            heading: Vec2::ZERO,
            ..good
        },
        Body {
            heading: v2(f32::NAN, 1.0),
            ..good
        },
        Body {
            shape: Shape::Ball { radius: -0.5 },
            ..good
        },
        Body {
            shape: Shape::Box {
                half: v2(1.0, -0.1),
            },
            ..good
        },
        Body {
            shape: Shape::Capsule {
                half_axis: v2(f32::NAN, 0.0),
                radius: 0.5,
            },
            ..good
        },
    ]
}

/// Defect: an unsound body that moves, is bumped, pushes another or is
/// pushed, or a planar function that panics on one. Oracle: physics.md 20
/// (such a body occupies nothing: every planar function leaves it as it is
/// and meets nothing with it). So a scene with one is, for the other
/// bodies, bit-equal to the scene without it, and it comes out bit-equal
/// to how it went in; `time_of_impact` says `Separated`, `manifold`
/// `None`, and `bump` changes nothing. The unsound body sits right in the
/// others' path and inside one of them.
#[wasm_bindgen_test(unsupported = test)]
fn an_unsound_body_is_left_as_it_is_and_meets_nothing() {
    let others = [
        Body {
            id: 1,
            shape: Shape::Ball { radius: 0.5 },
            position: v2(-2.0, 0.0),
            heading: v2(1.0, 0.0),
            velocity: v2(20.0, 0.0),
            spin: 0.0,
            inverse_mass: 1.0,
            inverse_inertia: 2.0,
        },
        Body {
            id: 9,
            shape: Shape::Box { half: v2(0.5, 0.5) },
            position: v2(2.0, 0.2),
            heading: v2(0.0, 1.0),
            velocity: v2(-10.0, 0.0),
            spin: -2.0,
            inverse_mass: 0.5,
            inverse_inertia: 1.0,
        },
    ];
    let settings = Settings::new(0.5, 0.5);
    let mut alone = others;
    let alone_impacts = planar::advance(&mut alone, 0.2, &settings, |_, _| true);
    assert!(!alone_impacts.is_empty(), "the others meet");
    for hostile in unsound(v2(0.0, 0.0)) {
        let mut bodies = vec![others[0], hostile, others[1]];
        let impacts = planar::advance(&mut bodies, 0.2, &settings, |_, _| true);
        let why = format!("{hostile:?}");
        assert_eq!(format!("{:?}", bodies[1]), format!("{hostile:?}"), "{why}");
        assert_eq!((bodies[0], bodies[2]), (alone[0], alone[1]), "{why}");
        assert_eq!(impacts, alone_impacts, "{why}");
        let toi = planar::time_of_impact(
            &hostile,
            Motion::of(&others[0], 0.2),
            &others[0],
            Motion::of(&others[0], 0.2),
        );
        assert_eq!(toi, Toi::Separated, "{why}");
        assert_eq!(planar::manifold(&hostile, &others[0]), None, "{why}");
        assert_eq!(planar::manifold(&others[0], &hostile), None, "{why}");
        let contact = Contact {
            point: v2(-1.0, 0.0),
            normal: v2(1.0, 0.0),
        };
        let bump = planar::bump(&others[0], &hostile, contact, 0.5, 0.5);
        assert_eq!(
            (bump.velocity_a, bump.spin_a, bump.normal_impulse),
            (others[0].velocity, others[0].spin, 0.0),
            "{why}"
        );
    }
}

/// Defect: an `advance` whose work or result runs away when many bodies
/// share one point (a spawn bug in a game): a loop past its caps, a
/// panic, or a non-finite body. Oracle: physics.md 20 (bounded work: at
/// most `(max_impacts + 1) n (n - 1) / 2` times of impact and
/// `max_separation_passes n (n - 1) / 2` manifolds; impacts in time order,
/// lower id first, at most `max_impacts`). 120 boxes, balls and capsules
/// at one point, half of them moving, come out finite, with their impacts
/// in order, within the caps.
#[wasm_bindgen_test(unsupported = test)]
fn a_hundred_bodies_at_one_point_end_finite_within_the_caps() {
    let mut rng = Rng::new(0x0924_2004);
    let mut bodies: Vec<Body> = (0..120)
        .map(|id| {
            let mut b = body(&mut rng, id, v2(4.0, -3.0));
            if id % 2 == 0 {
                b.velocity = Vec2::ZERO;
                b.spin = 0.0;
            }
            b
        })
        .collect();
    let settings = Settings::new(0.5, 0.5);
    let impacts = planar::advance(&mut bodies, 1.0 / 60.0, &settings, |_, _| true);
    assert!(impacts.len() <= settings.max_impacts, "{}", impacts.len());
    for pair in impacts.windows(2) {
        assert!(pair[0].time <= pair[1].time, "{pair:?}");
    }
    for impact in &impacts {
        assert!(impact.a < impact.b, "{impact:?}");
        assert!((0.0..=1.0).contains(&impact.time), "{impact:?}");
    }
    for b in &bodies {
        let finite = [
            b.position.x,
            b.position.y,
            b.velocity.x,
            b.velocity.y,
            b.spin,
        ]
        .iter()
        .all(|v| v.is_finite());
        assert!(finite && length(flat::p(b.heading)) > 0.5, "{b:?}");
    }
}

/// Defect: two bodies with no direction between them (two balls whose
/// centres coincide, a ball on a capsule's segment) that `advance` never
/// separates, because their manifold's normal is zero, though it reports
/// its separation finished. Oracle: physics.md 20 (when the separation
/// ends before its cap, no pair overlaps; a pair with no direction between
/// them is pushed apart along +x, the one first in body order toward −x;
/// a shuffled slice gives the same bodies, bit for bit), with overlap
/// judged by the shapes' own definitions in `f64`. Each pair at rest at one
/// point, the capsule lying along x so a push of the balls' radii alone
/// does not clear it, ends apart, the lower id on the −x side, and the
/// same with the slice reversed.
#[wasm_bindgen_test(unsupported = test)]
fn bodies_at_one_point_end_apart_along_x() {
    let at = v2(4.0, -3.0);
    let ball = |id: u64, radius: f32| Body {
        id,
        shape: Shape::Ball { radius },
        position: at,
        heading: v2(1.0, 0.0),
        velocity: Vec2::ZERO,
        spin: 0.0,
        inverse_mass: 1.0,
        inverse_inertia: 1.0,
    };
    let capsule = |id: u64| Body {
        shape: Shape::Capsule {
            half_axis: v2(1.5, 0.0),
            radius: 0.25,
        },
        inverse_mass: 0.5,
        ..ball(id, 0.0)
    };
    let pairs = [
        [ball(3, 0.5), ball(8, 0.5)],
        [ball(3, 0.5), ball(8, 0.2)],
        [ball(3, 0.4), capsule(8)],
        [capsule(3), ball(8, 0.4)],
    ];
    for pair in pairs {
        let mut bodies = pair;
        let impacts = planar::advance(&mut bodies, 1.0 / 60.0, &Settings::new(0.5, 0.5), |_, _| {
            true
        });
        assert!(impacts.is_empty(), "{impacts:?}");
        let placed = bodies.map(|b| place(&b.shape, flat::p(b.position), unit(b.heading)));
        let gap = separation(&placed[0], &placed[1]);
        assert!(
            gap > 0.0,
            "{pair:?} ends overlapping by {}: {bodies:?}",
            -gap
        );
        assert!(bodies[0].position.x < bodies[1].position.x, "{bodies:?}");
        assert_eq!(bodies[0].position.y, at.y, "{bodies:?}");
        assert_eq!(bodies[1].position.y, at.y, "{bodies:?}");
        let mut reversed = [pair[1], pair[0]];
        planar::advance(
            &mut reversed,
            1.0 / 60.0,
            &Settings::new(0.5, 0.5),
            |_, _| true,
        );
        assert_eq!([reversed[1], reversed[0]], bodies);
    }
}

/// Whether every number a body carries is finite and its heading is unit
/// to a few units in the last place, measured in `f64` (physics.md 12 and
/// 20: a heading is normalised to within a few units in the last place).
fn finite_and_unit(b: &Body) -> bool {
    let numbers = [
        b.position.x,
        b.position.y,
        b.velocity.x,
        b.velocity.y,
        b.heading.x,
        b.heading.y,
        b.spin,
    ];
    let heading = length(flat::p(b.heading));
    numbers.iter().all(|v| v.is_finite()) && (heading - 1.0).abs() <= 4.0 * f64::from(f32::EPSILON)
}

/// Defect: a turn that overflows before it is normalised, so a body
/// spinning fast enough ends with heading `(0, 0)` and from then on
/// occupies nothing. Oracle: physics.md 20 (a heading stays a unit
/// direction for any finite spin and step; a turn of `angle` turns by
/// `atan(angle)`, worked in `f64` here). A ball heading along x with
/// spin `1e30` advanced by `1/60` turns by all but a quarter turn: its
/// heading is `(c, s) / |(c, s)|` of `(1, 1e30 / 60)`, and it still meets
/// a ball laid over it.
#[wasm_bindgen_test(unsupported = test)]
fn a_heading_turned_by_a_huge_spin_stays_unit() {
    let spinning = Body {
        id: 1,
        shape: Shape::Box { half: v2(1.0, 0.5) },
        position: v2(0.0, 0.0),
        heading: v2(1.0, 0.0),
        velocity: Vec2::ZERO,
        spin: 1e30,
        inverse_mass: 1.0,
        inverse_inertia: 1.0,
    };
    let dt = 1.0f32 / 60.0;
    let mut bodies = [spinning];
    planar::advance(&mut bodies, dt, &Settings::new(0.5, 0.5), |_, _| true);
    let after = bodies[0];
    let expected = turned((1.0, 0.0), f64::from(spinning.spin * dt));
    let heading = flat::p(after.heading);
    assert!(finite_and_unit(&after), "{after:?}");
    assert!(
        (heading.0 - expected.0).abs() <= 1e-7 && (heading.1 - expected.1).abs() <= 1e-7,
        "{heading:?} against {expected:?}"
    );
    assert_eq!(after.position, spinning.position);
    let over = Body {
        id: 2,
        shape: Shape::Ball { radius: 0.25 },
        position: v2(0.0, 0.8),
        spin: 0.0,
        ..spinning
    };
    // Turned to stand along y, the box reaches 1 up, through the ball.
    let met = planar::manifold(&after, &over).expect("the turned box occupies its place");
    assert!(met.depth > 0.0, "{met:?}");
}

/// Defect: a body whose motion is near the top of `f32` makes a bump
/// overflow, and the overflow reaches the body it meets: the audit's
/// reproduction left a well-formed ball's position NaN. Oracle: physics.md
/// 20 (every planar function answers finitely for finite inputs; a body
/// whose own step `f32` cannot hold is left as it was and takes no part,
/// and no other body is made non-finite by it). A ball moving at `3e38`
/// meets well-formed balls ahead of and beside it, advanced by `0.25`, and
/// two balls at one point ride together to the top of `f32`: every body
/// ends finite with a unit heading, and the impacts are finite.
#[wasm_bindgen_test(unsupported = test)]
fn a_body_near_the_top_of_f32_makes_no_other_non_finite() {
    let ball = Body {
        id: 1,
        shape: Shape::Ball { radius: 0.5 },
        position: v2(0.0, 0.0),
        heading: v2(1.0, 0.0),
        velocity: v2(3e38, 0.0),
        spin: 0.0,
        inverse_mass: 1.0,
        inverse_inertia: 1.0,
    };
    for at in [v2(1.0, 0.0), v2(2.0, 0.0), v2(5.0, 0.3), v2(0.0, 5.0)] {
        let well_formed = Body {
            id: 2,
            position: at,
            velocity: v2(1.0, 0.0),
            ..ball
        };
        let mut bodies = [ball, well_formed];
        let impacts = planar::advance(&mut bodies, 0.25, &Settings::new(0.5, 0.5), |_, _| true);
        for b in &bodies {
            assert!(finite_and_unit(b), "beside {at}: {b:?}");
        }
        for i in &impacts {
            let numbers = [i.point.x, i.point.y, i.normal.x, i.normal.y, i.impulse];
            assert!(numbers.iter().all(|v| v.is_finite()), "{i:?}");
        }
    }
    // Two balls riding together to the top of `f32`, at one point: their
    // separation's push is not something `f32` can hold there.
    let riding = Body {
        velocity: v2(f32::MAX, 0.0),
        ..ball
    };
    let mut bodies = [riding, Body { id: 2, ..riding }];
    planar::advance(&mut bodies, 1.0, &Settings::new(0.5, 0.5), |_, _| true);
    for b in &bodies {
        assert!(finite_and_unit(b), "{b:?}");
    }
}

/// Defect: a body whose own step `f32` cannot hold (its motion, its
/// position plus its motion, its turn) that moves part way, turns to no
/// heading, or bumps the others with a non-finite motion. Oracle:
/// physics.md 20 (such a body is left exactly as it was for the step and
/// takes no part in it). So it comes out bit-equal to how it went in, and
/// the others bit-equal to the same step without it, though it sits in
/// their path.
#[wasm_bindgen_test(unsupported = test)]
fn a_body_whose_step_f32_cannot_hold_is_left_as_it_was() {
    let others = [
        Body {
            id: 1,
            shape: Shape::Ball { radius: 0.5 },
            position: v2(-2.0, 0.0),
            heading: v2(1.0, 0.0),
            velocity: v2(20.0, 0.0),
            spin: 0.0,
            inverse_mass: 1.0,
            inverse_inertia: 2.0,
        },
        Body {
            id: 9,
            shape: Shape::Box { half: v2(0.5, 0.5) },
            position: v2(2.0, 0.2),
            heading: v2(0.0, 1.0),
            velocity: v2(-10.0, 0.0),
            spin: -2.0,
            inverse_mass: 0.5,
            inverse_inertia: 1.0,
        },
    ];
    let dt = 2.0;
    let settings = Settings::new(0.5, 0.5);
    let mut alone = others;
    let alone_impacts = planar::advance(&mut alone, dt, &settings, |_, _| true);
    assert!(!alone_impacts.is_empty(), "the others meet");
    let base = Body {
        id: 5,
        shape: Shape::Box { half: v2(1.0, 0.5) },
        position: v2(0.0, 0.0),
        heading: v2(1.0, 0.0),
        velocity: Vec2::ZERO,
        spin: 0.0,
        inverse_mass: 1.0,
        inverse_inertia: 1.0,
    };
    let hostile = [
        // Its motion overflows.
        Body {
            velocity: v2(0.0, f32::MAX),
            ..base
        },
        // Its turn overflows.
        Body {
            spin: -f32::MAX,
            ..base
        },
        // Its end is past `f32`.
        Body {
            position: v2(0.0, 2e38),
            velocity: v2(0.0, 1e38),
            ..base
        },
    ];
    for hostile in hostile {
        let mut bodies = [others[0], hostile, others[1]];
        let impacts = planar::advance(&mut bodies, dt, &settings, |_, _| true);
        assert_eq!(bodies[1], hostile);
        assert_eq!((bodies[0], bodies[2]), (alone[0], alone[1]), "{hostile:?}");
        assert_eq!(impacts, alone_impacts, "{hostile:?}");
    }
}

/// A value from `normal` or, one time in three, one of `hostile`.
fn hostile_or(rng: &mut Rng, normal: f32, hostile: &[f32]) -> f32 {
    if rng.chance(33) {
        rng.pick(hostile) * if rng.chance(50) { -1.0 } else { 1.0 }
    } else {
        normal
    }
}

/// Defect: any planar function that turns finite inputs into a non-finite
/// body, impact, outcome or bump, or a heading that stops being unit: an
/// overflow in a turn, a motion, a frame between two far bodies, a bump or
/// a separation. Oracle: physics.md 20 (every planar function answers
/// finitely for finite inputs; a heading stays unit for any finite spin
/// and step) and 12 (unit to a few units in the last place), measured in
/// `f64`. Groups of bodies near one hostile place, each with hostile
/// spins, velocities, masses and inertias up to `f32::MAX` and subnormal,
/// advanced by steps from a frame to ten seconds, and every pair through
/// `time_of_impact`, `manifold` and `bump`.
#[wasm_bindgen_test(unsupported = test)]
fn no_finite_body_ends_non_finite() {
    const HUGE: [f32; 6] = [1e20, 1e30, 1e38, 3e38, f32::MAX, 1e-40];
    let mut rng = Rng::new(0x0924_A001);
    for case in 0..200 {
        let near: Vec2 = rng.vector(-50.0, 50.0);
        let centre = v2(
            hostile_or(&mut rng, near.x, &HUGE),
            hostile_or(&mut rng, near.y, &HUGE),
        );
        let count = rng.int(2, 5) as u64;
        let mut bodies: Vec<Body> = (0..count)
            .map(|id| {
                let mut b = body(&mut rng, id, centre);
                b.position = centre + rng.vector::<Vec2>(-2.0, 2.0);
                if rng.chance(20) {
                    b.position.x = hostile_or(&mut rng, b.position.x, &HUGE);
                }
                b.velocity = v2(
                    hostile_or(&mut rng, b.velocity.x, &HUGE),
                    hostile_or(&mut rng, b.velocity.y, &HUGE),
                );
                b.spin = hostile_or(&mut rng, b.spin, &HUGE);
                b.inverse_mass = hostile_or(&mut rng, b.inverse_mass, &HUGE).abs();
                b.inverse_inertia = hostile_or(&mut rng, b.inverse_inertia, &HUGE).abs();
                if rng.chance(10) {
                    b.inverse_mass = 0.0;
                }
                b
            })
            .collect();
        let dt = rng.pick(&[1.0 / 60.0, 0.25, 10.0]);
        let before = bodies.clone();
        let why = format!("case {case}: dt {dt} {before:?}");
        for a in &before {
            for b in &before {
                let toi = planar::time_of_impact(a, Motion::of(a, dt), b, Motion::of(b, dt));
                let contact = match toi {
                    Toi::Hit { fraction, contact }
                    | Toi::Failed {
                        fraction,
                        contact: Some(contact),
                    } => {
                        assert!((0.0..=1.0).contains(&fraction), "{toi:?} {why}");
                        Some(contact)
                    }
                    Toi::Failed {
                        fraction,
                        contact: None,
                    } => {
                        assert!((0.0..=1.0).contains(&fraction), "{toi:?} {why}");
                        None
                    }
                    _ => None,
                };
                if let Some(c) = contact {
                    let numbers = [c.point.x, c.point.y, c.normal.x, c.normal.y];
                    assert!(numbers.iter().all(|v| v.is_finite()), "{toi:?} {why}");
                }
                if let Some(m) = planar::manifold(a, b) {
                    let mut numbers = vec![m.normal.x, m.normal.y, m.depth];
                    numbers.extend(m.points().iter().flat_map(|p| [p.x, p.y]));
                    assert!(numbers.iter().all(|v| v.is_finite()), "{m:?} {why}");
                }
                let contact = Contact {
                    point: a.position,
                    normal: v2(1.0, 0.0),
                };
                let bump = planar::bump(a, b, contact, 0.5, 0.5);
                let numbers = [
                    bump.velocity_a.x,
                    bump.velocity_a.y,
                    bump.spin_a,
                    bump.velocity_b.x,
                    bump.velocity_b.y,
                    bump.spin_b,
                    bump.normal_impulse,
                    bump.friction_impulse,
                ];
                assert!(numbers.iter().all(|v| v.is_finite()), "{bump:?} {why}");
            }
        }
        let impacts = planar::advance(&mut bodies, dt, &Settings::new(0.5, 0.5), |_, _| true);
        for b in &bodies {
            let start = before.iter().find(|s| s.id == b.id).expect("same bodies");
            let unmoved = b == start;
            assert!(unmoved || finite_and_unit(b), "{b:?} {why}");
        }
        for i in &impacts {
            let numbers = [
                i.time, i.point.x, i.point.y, i.normal.x, i.normal.y, i.impulse,
            ];
            assert!(numbers.iter().all(|v| v.is_finite()), "{i:?} {why}");
        }
    }
}
