//! Planar bumps (#921): the time of impact of bodies that turn as they
//! move, the manifold of overlapping ones, the bump and `advance`, through
//! the public API. Every expected value comes from geometry worked in the
//! test, a shape's own definition evaluated in `f64` without the crate, the
//! physical laws, or a requirement of `specs/physics.md` (planar bodies).
//! Runs natively and in a wasm runtime.
// Values worked in `f64` come back to `f32` to compare; fractions and
// restitutions the contract states exactly (a hit at zero, restitution 1)
// are compared exactly; the laws name their quantities by letter.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::float_cmp,
    clippy::many_single_char_names,
    clippy::too_many_lines
)]

use core::f32::consts::FRAC_1_SQRT_2;

use sgl_core::math::Vec2;
use sgl_core::{CanonicalWrite, SplitMix64, StateHasher};
use sgl_physics::planar::{self, Body, Contact, MAX_IMPACTS, Motion, Settings, Toi};
use sgl_physics::{Convex, Shape};
use wasm_bindgen_test::wasm_bindgen_test;

fn v2(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

fn cuboid(x: f32, y: f32) -> Shape<Vec2> {
    Shape::Box { half: v2(x, y) }
}

fn ball(radius: f32) -> Shape<Vec2> {
    Shape::Ball { radius }
}

/// A body at rest, heading along x, of unit mass and a box's or a disc's
/// inertia (a capsule and a convex shape take their enclosing box's).
fn body(id: u64, shape: Shape<Vec2>, position: Vec2) -> Body {
    let inverse_inertia = match shape {
        Shape::Ball { radius } => planar::disc_inverse_inertia(1.0, radius),
        _ => planar::box_inverse_inertia(1.0, shape.half_extents()),
    };
    Body {
        id,
        shape,
        position,
        heading: v2(1.0, 0.0),
        velocity: Vec2::ZERO,
        spin: 0.0,
        inverse_mass: 1.0,
        inverse_inertia,
    }
}

/// The same body, immovable: a wall or a parked prop.
fn fixed(body: Body) -> Body {
    Body {
        inverse_mass: 0.0,
        inverse_inertia: 0.0,
        ..body
    }
}

fn moving(translation: Vec2) -> Motion {
    Motion {
        translation,
        turn: 0.0,
    }
}

const STILL: Motion = Motion {
    translation: Vec2::ZERO,
    turn: 0.0,
};

/// The contract's query slop for a pair: a hundredth of the smaller
/// shape's extent (its largest half-extent), at most a quarter of the
/// smaller radius (physics.md, 19). Every pair here is small enough that
/// the rounding floor never applies.
fn slop(a: &Shape<Vec2>, b: &Shape<Vec2>) -> f32 {
    let extent = |shape: &Shape<Vec2>| {
        let half = shape.half_extents();
        half.x.max(half.y)
    };
    let mut slop = (extent(a).min(extent(b))) / 100.0;
    for shape in [a, b] {
        if let Shape::Ball { radius } | Shape::Capsule { radius, .. } = *shape {
            slop = slop.min(radius / 4.0);
        }
    }
    slop
}

#[track_caller]
fn assert_near(actual: f32, expected: f32, tolerance: f32) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} is not within {tolerance} of {expected}"
    );
}

#[track_caller]
fn assert_vec_near(actual: Vec2, expected: Vec2, tolerance: f32) {
    assert!(
        (actual - expected).abs().max_element() <= tolerance,
        "{actual} is not within {tolerance} of {expected}"
    );
}

/// Asserts a hit where geometry puts it. `touch` is the fraction at which
/// the surfaces touch and `closing` how fast the gap between them closes
/// along the normal per unit of fraction, both worked by hand in the
/// caller. The contract: contact when the surfaces are three quarters to
/// one and a quarter slops apart.
#[track_caller]
fn assert_hit(toi: Toi, touch: f32, closing: f32, slop: f32, normal: Vec2) -> Contact {
    let Toi::Hit { fraction, contact } = toi else {
        panic!("expected a hit, got {toi:?}");
    };
    let earliest = touch - 1.25 * slop / closing;
    let latest = touch - 0.75 * slop / closing;
    assert!(
        fraction >= earliest - 1e-5 && fraction <= latest + 1e-5,
        "fraction {fraction} is outside {earliest}..{latest}"
    );
    assert_vec_near(contact.normal, normal, 1e-5);
    contact
}

// ------------------------------------------------------- time of impact

/// Defect: a head-on approach met late (tunnelling) or far from where the
/// faces close. Oracle: geometry by hand. Two 2 x 1 boxes 3 apart, one
/// closing at 4 per step: the faces touch at three quarters of the step,
/// and both moving at 2 meet at the same instant. The contact lies midway
/// between the faces, at the middle of the face they share.
#[wasm_bindgen_test(unsupported = test)]
fn boxes_closing_head_on_meet_when_the_gap_is_gone() {
    let a = body(1, cuboid(1.0, 0.5), v2(0.0, 0.0));
    let b = body(2, cuboid(1.0, 0.5), v2(5.0, 0.0));
    let slop = slop(&a.shape, &b.shape);
    let one_moves = planar::time_of_impact(&a, STILL, &b, moving(v2(-4.0, 0.0)));
    let contact = assert_hit(one_moves, 0.75, 4.0, slop, v2(1.0, 0.0));
    // Midway between faces 0.75 to 1.25 slops apart, at the middle of the
    // shared face, where moving B has put its own face.
    assert!(contact.point.x > 1.0 && contact.point.x < 1.0 + 0.625 * slop + 1e-5);
    assert_near(contact.point.y, 0.0, 1e-5);

    let both_move = planar::time_of_impact(&a, moving(v2(2.0, 0.0)), &b, moving(v2(-2.0, 0.0)));
    assert_hit(both_move, 0.75, 4.0, slop, v2(1.0, 0.0));
}

/// Defect: a shallow approach to a face missed, or met at its side.
/// Oracle: geometry by hand. A unit box falling one unit per step while
/// running six, from above and to the left of a 2 x 1 box: its side passes
/// the box's left face while still above its top (at a quarter of the
/// step), and its bottom reaches the top face at half the step, its whole
/// bottom face over the top one: the contact is the middle of the falling
/// face, under its centre, `-3 + 6 * fraction` along.
#[wasm_bindgen_test(unsupported = test)]
fn a_glancing_approach_meets_the_face_it_lands_on() {
    let a = body(1, cuboid(1.0, 0.5), v2(0.0, 0.0));
    let b = body(2, cuboid(0.5, 0.5), v2(-3.0, 1.5));
    let toi = planar::time_of_impact(&a, STILL, &b, moving(v2(6.0, -1.0)));
    let contact = assert_hit(toi, 0.5, 1.0, slop(&a.shape, &b.shape), v2(0.0, 1.0));
    let Toi::Hit { fraction, .. } = toi else {
        unreachable!()
    };
    assert_near(contact.point.x, -3.0 + 6.0 * fraction, 1e-4);
}

/// Defect: a turned box's corner met where its centre or its unturned box
/// would be. Oracle: geometry by hand. A unit box turned an eighth of a
/// turn reaches `sqrt(1/2)` ahead of its centre, at its corner, so moving
/// 4 toward a face 4 - 1 - sqrt(1/2) away it meets the face at that share
/// of the step, at the height of its centre.
#[wasm_bindgen_test(unsupported = test)]
fn a_turned_corner_meets_a_face_where_the_corner_reaches() {
    let a = body(1, cuboid(1.0, 0.5), v2(0.0, 0.0));
    let b = Body {
        heading: v2(FRAC_1_SQRT_2, FRAC_1_SQRT_2),
        ..body(2, cuboid(0.5, 0.5), v2(4.0, 0.0))
    };
    let touch = (4.0 - 1.0 - FRAC_1_SQRT_2) / 4.0;
    let toi = planar::time_of_impact(&a, STILL, &b, moving(v2(-4.0, 0.0)));
    let contact = assert_hit(toi, touch, 4.0, slop(&a.shape, &b.shape), v2(1.0, 0.0));
    assert_near(contact.point.y, 0.0, 1e-4);
}

/// Defect: a pass that never comes within the contact band reported as a
/// hit, or one inside it missed. Oracle: the contract's band (contact
/// within one and a quarter slops). A unit box slides along over a 2 x 1
/// box, its bottom face a whole unit above the top one, then a slop and a
/// half above (outside the band): no contact; then nine tenths of a slop
/// above, already in the band at the start: a hit at zero.
#[wasm_bindgen_test(unsupported = test)]
fn a_pass_clear_of_the_band_misses_and_one_inside_it_hits_at_once() {
    let a = body(1, cuboid(1.0, 0.5), v2(0.0, 0.0));
    let slop = slop(&a.shape, &cuboid(0.5, 0.5));
    let pass = |gap: f32| {
        let b = body(2, cuboid(0.5, 0.5), v2(-0.5, 1.0 + gap));
        planar::time_of_impact(&a, STILL, &b, moving(v2(6.0, 0.0)))
    };
    assert_eq!(pass(1.0), Toi::Separated);
    assert_eq!(pass(1.5 * slop), Toi::Separated);
    let Toi::Hit { fraction, contact } = pass(0.9 * slop) else {
        panic!("a pass inside the band hits");
    };
    assert_eq!(fraction, 0.0);
    assert_vec_near(contact.normal, v2(0.0, 1.0), 1e-5);
}

/// Defect: bodies already touching reported clear, or overlapping ones
/// reported as a hit (a mover would then take the overlap for contact).
/// Oracle: the contract. Two boxes half a slop apart are touching: a hit
/// at zero, whichever way they move; boxes a tenth of a unit into each
/// other, and balls whose surfaces overlap while their centres do not,
/// overlap.
#[wasm_bindgen_test(unsupported = test)]
fn touching_bodies_hit_at_once_and_overlapping_ones_say_so() {
    let a = body(1, cuboid(1.0, 0.5), v2(0.0, 0.0));
    let slop = slop(&a.shape, &a.shape);
    let touching = body(2, cuboid(1.0, 0.5), v2(2.0 + 0.5 * slop, 0.0));
    for motion in [moving(v2(1.0, 0.0)), moving(v2(-1.0, 0.0)), STILL] {
        let Toi::Hit { fraction, contact } = planar::time_of_impact(&a, STILL, &touching, motion)
        else {
            panic!("touching bodies hit at once");
        };
        assert_eq!(fraction, 0.0);
        assert_vec_near(contact.normal, v2(1.0, 0.0), 1e-5);
    }
    let into = body(2, cuboid(1.0, 0.5), v2(1.9, 0.0));
    assert_eq!(
        planar::time_of_impact(&a, STILL, &into, moving(v2(1.0, 0.0))),
        Toi::Overlapping
    );
    let round = body(3, ball(0.5), v2(0.0, 0.0));
    let other = body(4, ball(0.5), v2(0.9, 0.0));
    assert_eq!(
        planar::time_of_impact(&round, STILL, &other, moving(v2(1.0, 0.0))),
        Toi::Overlapping
    );
}

/// The heading by the crate's rotation rule, worked in `f64`: a turn of
/// `turn` radians turns by `atan(turn)` (physics.md, planar bodies), and
/// between the two headings the sweep blends their cosines and sines and
/// normalises.
fn swept_angle(turn: f64, fraction: f64) -> f64 {
    let end = turn.atan();
    (fraction * end.sin()).atan2((1.0 - fraction) + fraction * end.cos())
}

/// Defect: a turning body swept as if it only translated, so a spinning
/// car's corner passes through another car. Oracle: geometry by hand,
/// with the turn by the documented rule worked in `f64`. A 4 x 0.2 bar
/// pinned at the origin turns by one radian's rule (an eighth of a turn);
/// a half-unit box at (1.2, 1.2) is reached only by the turn: its corner
/// (1.45, 0.95), a radius `r` from the pin at an angle `phi`, meets the
/// bar's top face when the bar has turned `phi - asin(0.1 / r)`. Without
/// the turn the bar never reaches it.
#[wasm_bindgen_test(unsupported = test)]
fn a_bar_that_only_turns_reaches_a_box_by_turning() {
    let bar = body(1, cuboid(2.0, 0.1), v2(0.0, 0.0));
    let target = body(2, cuboid(0.25, 0.25), v2(1.2, 1.2));
    let slop = f64::from(slop(&bar.shape, &target.shape));
    assert_eq!(
        planar::time_of_impact(&bar, STILL, &target, STILL),
        Toi::Separated
    );
    let turning = Motion {
        translation: Vec2::ZERO,
        turn: 1.0,
    };
    let Toi::Hit { fraction, contact } = planar::time_of_impact(&bar, turning, &target, STILL)
    else {
        panic!("the turning bar reaches the box");
    };
    // The corner's height above the bar's top face at a fraction.
    let (cx, cy) = (1.45f64, 0.95f64);
    let gap = |fraction: f64| {
        let angle = swept_angle(1.0, fraction);
        -cx * angle.sin() + cy * angle.cos() - 0.1
    };
    let at = gap(f64::from(fraction));
    assert!(
        at >= 0.75 * slop - 1e-5 && at <= 1.25 * slop + 1e-5,
        "the corner is {at} from the face at {fraction}"
    );
    // The gap closes steadily, so the band is entered once: before the
    // corner touches.
    let (r, phi) = (cx.hypot(cy), cy.atan2(cx));
    let touch_angle = phi - (0.1 / r).asin();
    assert!(swept_angle(1.0, f64::from(fraction)) < touch_angle);
    let normal = v2(-(touch_angle.sin() as f32), touch_angle.cos() as f32);
    assert_vec_near(contact.normal, normal, 2e-3);
    assert_vec_near(contact.point, v2(1.45, 0.95), 0.01);
}

/// Defect: a contact that begins and ends within the step passed over,
/// because the kernel's separating axis still separates the boxes at the
/// end (`Box2D`'s documented limit: a car clipping another's corner at
/// speed). Oracle: the shapes' own definitions along the documented
/// motion, in `f64`: these two turning boxes (a case the comparison with
/// parry found) overlap at a tenth of the step and are apart again at its
/// end; the time of impact meets them before the tenth.
#[wasm_bindgen_test(unsupported = test)]
fn a_contact_that_begins_and_ends_within_the_step_is_met() {
    let a = Body {
        heading: v2(0.365_148, 0.930_949_5),
        ..body(1, cuboid(1.192_449_2, 0.798_901_2), v2(0.0, 0.0))
    };
    let motion_a = Motion {
        translation: v2(-1.246_357_4, 0.165_470_6),
        turn: -0.548_866_4,
    };
    let b = Body {
        heading: v2(-0.502_718_5, -0.864_450_16),
        ..body(
            2,
            cuboid(0.476_680_64, 0.536_366_16),
            v2(1.337_449_6, -0.542_583_7),
        )
    };
    let motion_b = Motion {
        translation: v2(-2.481_893, -1.157_036_3),
        turn: -0.415_850_28,
    };
    let at = |body: &Body, motion: &Motion, fraction: f64| {
        let start = f64::from(body.heading.y).atan2(f64::from(body.heading.x));
        let angle = start + swept_angle(f64::from(motion.turn), fraction);
        Body {
            position: v2(
                (f64::from(body.position.x) + fraction * f64::from(motion.translation.x)) as f32,
                (f64::from(body.position.y) + fraction * f64::from(motion.translation.y)) as f32,
            ),
            heading: v2(angle.cos() as f32, angle.sin() as f32),
            ..*body
        }
    };
    assert!(separation(&at(&a, &motion_a, 0.1), &at(&b, &motion_b, 0.1)) < -0.01);
    assert!(separation(&at(&a, &motion_a, 1.0), &at(&b, &motion_b, 1.0)) > 0.1);
    let Toi::Hit { fraction, .. } = planar::time_of_impact(&a, motion_a, &b, motion_b) else {
        panic!("the boxes meet within the step");
    };
    assert!(fraction < 0.1, "{fraction}");
}

/// Defect: a heading turned by some other rule than the documented one
/// (a game predicting a heading would disagree with the crate). Oracle:
/// the rule in physics.md, worked in `f64`: a spin of `w` for `dt` turns
/// the heading by `atan(w dt)`. One radian's turn is an eighth of a turn;
/// a tenth of a radian is `atan(0.1)`.
#[wasm_bindgen_test(unsupported = test)]
fn a_heading_turns_by_the_documented_rule() {
    for (spin, dt) in [(2.0f32, 0.5f32), (6.0, 1.0 / 60.0), (-3.0, 0.1)] {
        let mut bodies = [Body {
            spin,
            ..body(1, cuboid(1.0, 0.5), v2(3.0, -2.0))
        }];
        planar::advance(&mut bodies, dt, &Settings::new(0.0, 0.0), |_, _| true);
        let angle = (f64::from(spin) * f64::from(dt)).atan();
        let expected = v2(angle.cos() as f32, angle.sin() as f32);
        assert_vec_near(bodies[0].heading, expected, 1e-6);
        assert_eq!(bodies[0].position, v2(3.0, -2.0));
    }
}

// ------------------------------------------------------ the f64 oracle

/// A shape's core in world coordinates, worked in `f64` from its own
/// definition, and its radius: a box's four corners, a ball's centre, a
/// capsule's two ends or a polygon's vertices, turned by the heading.
fn world_core(body: &Body) -> (Vec<(f64, f64)>, f64) {
    let length = f64::from(body.heading.x).hypot(f64::from(body.heading.y));
    let (c, s) = (
        f64::from(body.heading.x) / length,
        f64::from(body.heading.y) / length,
    );
    let place = |x: f32, y: f32| {
        let (x, y) = (f64::from(x), f64::from(y));
        (
            f64::from(body.position.x) + c * x - s * y,
            f64::from(body.position.y) + s * x + c * y,
        )
    };
    match body.shape {
        Shape::Box { half } => (
            vec![
                place(-half.x, -half.y),
                place(half.x, -half.y),
                place(half.x, half.y),
                place(-half.x, half.y),
            ],
            0.0,
        ),
        Shape::Ball { radius } => (vec![place(0.0, 0.0)], f64::from(radius)),
        Shape::Capsule { half_axis, radius } => (
            vec![
                place(-half_axis.x, -half_axis.y),
                place(half_axis.x, half_axis.y),
            ],
            f64::from(radius),
        ),
        Shape::Convex(convex) => (
            convex.points().iter().map(|p| place(p.x, p.y)).collect(),
            0.0,
        ),
    }
}

type P = (f64, f64);

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}

fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

fn dot(a: P, b: P) -> f64 {
    a.0 * b.0 + a.1 * b.1
}

/// The edges of a core: a point is one degenerate edge, a segment one
/// edge, a polygon its closed loop.
fn edges(points: &[P]) -> Vec<(P, P)> {
    match points.len() {
        1 => vec![(points[0], points[0])],
        2 => vec![(points[0], points[1])],
        n => (0..n).map(|i| (points[i], points[(i + 1) % n])).collect(),
    }
}

fn point_segment(p: P, (a, b): (P, P)) -> f64 {
    let ab = sub(b, a);
    let length = dot(ab, ab);
    let t = if length > 0.0 {
        (dot(sub(p, a), ab) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let closest = (a.0 + ab.0 * t, a.1 + ab.1 * t);
    let d = sub(p, closest);
    dot(d, d).sqrt()
}

fn segments_cross((a1, a2): (P, P), (b1, b2): (P, P)) -> bool {
    let d1 = cross(sub(a2, a1), sub(b1, a1));
    let d2 = cross(sub(a2, a1), sub(b2, a1));
    let d3 = cross(sub(b2, b1), sub(a1, b1));
    let d4 = cross(sub(b2, b1), sub(a2, b1));
    d1 * d2 < 0.0 && d3 * d4 < 0.0
}

/// Whether `p` is strictly inside the counter-clockwise polygon.
fn inside(p: P, polygon: &[P]) -> bool {
    polygon.len() >= 3
        && edges(polygon)
            .iter()
            .all(|&(a, b)| cross(sub(b, a), sub(p, a)) > 0.0)
}

/// How far two polygons overlap: the least overlap of their projections
/// on every edge normal of both (the separating-axis theorem), negative
/// when an axis separates them.
fn penetration(a: &[P], b: &[P]) -> f64 {
    let mut least = f64::INFINITY;
    for (p, q) in edges(a).into_iter().chain(edges(b)) {
        let edge = sub(q, p);
        let length = dot(edge, edge).sqrt();
        if length == 0.0 {
            continue;
        }
        let axis = (edge.1 / length, -edge.0 / length);
        let span = |points: &[P]| {
            points
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &p| {
                    (lo.min(dot(p, axis)), hi.max(dot(p, axis)))
                })
        };
        let ((lo_a, hi_a), (lo_b, hi_b)) = (span(a), span(b));
        least = least.min(hi_a.min(hi_b) - lo_a.max(lo_b));
    }
    least
}

/// How far apart two bodies' surfaces are, worked from their own
/// definitions in `f64`: the distance between their cores less their
/// radii, and when the cores meet, minus how far they overlap (for two
/// polygons, the separating-axis depth). Negative means overlap.
fn separation(a: &Body, b: &Body) -> f64 {
    let ((core_a, radius_a), (core_b, radius_b)) = (world_core(a), world_core(b));
    let (edges_a, edges_b) = (edges(&core_a), edges(&core_b));
    let meet = edges_a
        .iter()
        .any(|&e| edges_b.iter().any(|&f| segments_cross(e, f)))
        || core_b.iter().any(|&p| inside(p, &core_a))
        || core_a.iter().any(|&p| inside(p, &core_b));
    if meet {
        let depth = if core_a.len() >= 3 && core_b.len() >= 3 {
            penetration(&core_a, &core_b)
        } else {
            0.0
        };
        return -depth - radius_a - radius_b;
    }
    let mut distance = f64::INFINITY;
    for &p in &core_a {
        for &e in &edges_b {
            distance = distance.min(point_segment(p, e));
        }
    }
    for &p in &core_b {
        for &e in &edges_a {
            distance = distance.min(point_segment(p, e));
        }
    }
    distance - radius_a - radius_b
}

fn draw(rng: &mut SplitMix64, low: f32, high: f32) -> f32 {
    low + (high - low) * ((rng.next_u64() >> 40) as f32 / 16_777_216.0)
}

fn pick(rng: &mut SplitMix64, n: u64) -> u64 {
    rng.next_u64() % n
}

/// A random shape of each kind: a box, a ball, a capsule or a convex
/// polygon of three to eight vertices on a circle.
fn random_shape(rng: &mut SplitMix64) -> Shape<Vec2> {
    match pick(rng, 4) {
        0 => cuboid(draw(rng, 0.2, 1.5), draw(rng, 0.2, 1.0)),
        1 => ball(draw(rng, 0.2, 1.0)),
        2 => Shape::Capsule {
            half_axis: v2(draw(rng, -0.8, 0.8), draw(rng, -0.8, 0.8)),
            radius: draw(rng, 0.1, 0.5),
        },
        _ => {
            let count = 3 + pick(rng, 6);
            let radius = draw(rng, 0.3, 1.0);
            let step = core::f32::consts::TAU / count as f32;
            let points: Vec<Vec2> = (0..count)
                .map(|i| {
                    let angle = step * (i as f32 + draw(rng, 0.0, 0.5));
                    v2(angle.cos(), angle.sin()) * radius
                })
                .collect();
            Shape::Convex(Convex::polygon(&points, 1.0, 0.0))
        }
    }
}

fn random_heading(rng: &mut SplitMix64) -> Vec2 {
    let angle = draw(rng, -3.2, 3.2);
    v2(angle.cos(), angle.sin())
}

// ------------------------------------------------------------- manifold

/// Defect: a manifold with the wrong depth, normal or points for the
/// plainest overlaps. Oracle: geometry by hand. Two 2 x 1 boxes, the
/// second 1.9 along and 0.2 up: they overlap 0.1 along x, the shared part
/// of the faces runs from -0.3 to 0.5, and the points lie midway between
/// the faces, at x = 0.95. A ball of radius 0.5 whose centre is 1.3 from a
/// box's centre is 0.2 into its face, the point midway at 0.9; two balls
/// of radii 1 and 0.5 with centres 1.2 apart overlap 0.3, the point midway
/// between the surfaces at 0.85. A capsule lying 0.7 above a box's centre
/// is 0.05 into its top face, with points at the ends of its segment.
#[wasm_bindgen_test(unsupported = test)]
fn plain_overlaps_have_the_depth_normal_and_points_geometry_gives() {
    let a = body(1, cuboid(1.0, 0.5), v2(0.0, 0.0));
    let b = body(2, cuboid(1.0, 0.5), v2(1.9, 0.2));
    let found = planar::manifold(&a, &b).expect("overlapping boxes have a manifold");
    assert_near(found.depth, 0.1, 1e-5);
    assert_vec_near(found.normal, v2(1.0, 0.0), 1e-6);
    let mut points = found.points().to_vec();
    points.sort_by(|p, q| p.y.total_cmp(&q.y));
    assert_eq!(points.len(), 2);
    assert_vec_near(points[0], v2(0.95, -0.3), 1e-5);
    assert_vec_near(points[1], v2(0.95, 0.5), 1e-5);

    let round = body(3, ball(0.5), v2(1.3, 0.0));
    for (first, second, normal) in [(&a, &round, 1.0), (&round, &a, -1.0)] {
        let found = planar::manifold(first, second).expect("a ball in a box");
        assert_near(found.depth, 0.2, 1e-5);
        assert_vec_near(found.normal, v2(normal, 0.0), 1e-6);
        assert_eq!(found.points().len(), 1);
        assert_vec_near(found.points()[0], v2(0.9, 0.0), 1e-5);
    }

    let big = body(4, ball(1.0), v2(0.0, 0.0));
    let small = body(5, ball(0.5), v2(1.2, 0.0));
    let found = planar::manifold(&big, &small).expect("overlapping balls");
    assert_near(found.depth, 0.3, 1e-5);
    assert_vec_near(found.points()[0], v2(0.85, 0.0), 1e-5);

    let slab = body(6, cuboid(2.0, 0.5), v2(0.0, 0.0));
    let lying = body(
        7,
        Shape::Capsule {
            half_axis: v2(1.0, 0.0),
            radius: 0.25,
        },
        v2(0.0, 0.7),
    );
    let found = planar::manifold(&slab, &lying).expect("a capsule on a box");
    assert_near(found.depth, 0.05, 1e-5);
    assert_vec_near(found.normal, v2(0.0, 1.0), 1e-6);
    let mut xs: Vec<f32> = found.points().iter().map(|p| p.x).collect();
    xs.sort_by(f32::total_cmp);
    assert_eq!(xs.len(), 2);
    assert_near(xs[0], -1.0, 1e-5);
    assert_near(xs[1], 1.0, 1e-5);
}

/// Defect: a manifold reported for bodies that are apart beyond its
/// margin, or none for ones within it. Oracle: the contract (four slops,
/// `Box2D`'s speculative distance). Two boxes three slops apart have a
/// manifold whose depth is minus the gap; five slops apart, none.
#[wasm_bindgen_test(unsupported = test)]
fn a_manifold_reaches_four_slops_and_no_further() {
    let a = body(1, cuboid(1.0, 0.5), v2(0.0, 0.0));
    let slop = slop(&a.shape, &a.shape);
    let near = body(2, cuboid(1.0, 0.5), v2(2.0 + 3.0 * slop, 0.0));
    let found = planar::manifold(&a, &near).expect("within the margin");
    assert_near(found.depth, -3.0 * slop, 1e-5);
    let far = body(2, cuboid(1.0, 0.5), v2(2.0 + 5.0 * slop, 0.0));
    assert_eq!(planar::manifold(&a, &far), None);
}

/// Defect: a depth or a normal that does not separate the bodies, so
/// `advance`'s separation leaves them overlapping. Oracle: each shape's
/// own definition in `f64` (`separation`): a separating-axis test written
/// for the test, never a crate query. Over seeded random pairs of boxes,
/// balls, capsules and polygons at random headings that overlap, moving
/// the second by the depth along the normal leaves them no more than
/// rounding into each other (2e-5: a few units in the last place of
/// coordinates up to 4).
#[wasm_bindgen_test(unsupported = test)]
fn moving_by_the_depth_along_the_normal_separates_overlapping_bodies() {
    let mut rng = SplitMix64::new(921);
    let mut tried = 0;
    while tried < 300 {
        let a = Body {
            heading: random_heading(&mut rng),
            ..body(1, random_shape(&mut rng), v2(0.0, 0.0))
        };
        let b = Body {
            heading: random_heading(&mut rng),
            ..body(
                2,
                random_shape(&mut rng),
                v2(draw(&mut rng, -1.5, 1.5), draw(&mut rng, -1.5, 1.5)),
            )
        };
        if separation(&a, &b) > -1e-3 {
            continue;
        }
        tried += 1;
        let found = planar::manifold(&a, &b).expect("overlapping bodies have a manifold");
        assert!(found.depth > 0.0, "{a:?} {b:?} {found:?}");
        let moved = Body {
            position: b.position + found.normal * found.depth,
            ..b
        };
        let after = separation(&a, &moved);
        assert!(
            after >= -2e-5,
            "still {after} into each other: {a:?} {b:?} {found:?}"
        );
    }
}

// ------------------------------------------------------------------ bump

/// A random pair of bodies with finite masses and a contact between them.
fn random_pair(rng: &mut SplitMix64) -> (Body, Body, Contact) {
    let mut make = |id| Body {
        velocity: v2(draw(rng, -10.0, 10.0), draw(rng, -10.0, 10.0)),
        spin: draw(rng, -6.0, 6.0),
        inverse_mass: 1.0 / draw(rng, 0.5, 4.0),
        inverse_inertia: 1.0 / draw(rng, 0.2, 3.0),
        position: v2(draw(rng, -3.0, 3.0), draw(rng, -3.0, 3.0)),
        ..body(id, cuboid(1.0, 0.5), Vec2::ZERO)
    };
    let (a, b) = (make(1), make(2));
    let normal = random_heading(rng);
    let point = v2(draw(rng, -3.0, 3.0), draw(rng, -3.0, 3.0));
    (a, b, Contact { point, normal })
}

/// The laws a bump is held to, in `f64`: linear momentum, angular momentum
/// about the origin (a fixed point), kinetic energy, and the speed at which
/// the contact points close along the normal.
struct Laws {
    momentum: (f64, f64),
    angular: f64,
    energy: f64,
    closing: f64,
}

fn laws(
    (a, b): (&Body, &Body),
    (velocity_a, spin_a, velocity_b, spin_b): (Vec2, f32, Vec2, f32),
    contact: &Contact,
) -> Laws {
    let one = |body: &Body, velocity: Vec2, spin: f32| {
        let m = 1.0 / f64::from(body.inverse_mass);
        let i = 1.0 / f64::from(body.inverse_inertia);
        let (vx, vy, w) = (
            f64::from(velocity.x),
            f64::from(velocity.y),
            f64::from(spin),
        );
        let (cx, cy) = (f64::from(body.position.x), f64::from(body.position.y));
        let r = (
            f64::from(contact.point.x) - cx,
            f64::from(contact.point.y) - cy,
        );
        let at_point = (vx - w * r.1, vy + w * r.0);
        (
            (m * vx, m * vy),
            m * (cx * vy - cy * vx) + i * w,
            0.5 * m * (vx * vx + vy * vy) + 0.5 * i * w * w,
            at_point,
        )
    };
    let (pa, la, ea, va) = one(a, velocity_a, spin_a);
    let (pb, lb, eb, vb) = one(b, velocity_b, spin_b);
    let n = (f64::from(contact.normal.x), f64::from(contact.normal.y));
    Laws {
        momentum: (pa.0 + pb.0, pa.1 + pb.1),
        angular: la + lb,
        energy: ea + eb,
        closing: dot(sub(vb, va), n),
    }
}

/// Defect: an impulse that is not equal and opposite at one point, that
/// adds energy, or that leaves the pair approaching. Oracle: the physical
/// laws in `f64`. Over seeded random pairs, restitutions in [0, 1] and
/// frictions in [0, 1.5]: linear momentum and angular momentum about a
/// fixed point are kept, kinetic energy never grows, at restitution 1
/// without friction it is kept, and the contact points are not left
/// approaching. Tolerances are `f32` rounding of the speeds involved (a
/// few parts in a million).
#[wasm_bindgen_test(unsupported = test)]
fn a_bump_keeps_momentum_adds_no_energy_and_ends_the_approach() {
    let mut rng = SplitMix64::new(0x0b0b);
    let mut approaching = 0;
    for case in 0..400 {
        let (a, b, contact) = random_pair(&mut rng);
        let (restitution, friction) = if case % 4 == 0 {
            (1.0, 0.0)
        } else {
            (draw(&mut rng, 0.0, 1.0), draw(&mut rng, 0.0, 1.5))
        };
        let bumped = planar::bump(&a, &b, contact, restitution, friction);
        let before = laws((&a, &b), (a.velocity, a.spin, b.velocity, b.spin), &contact);
        let after = laws(
            (&a, &b),
            (
                bumped.velocity_a,
                bumped.spin_a,
                bumped.velocity_b,
                bumped.spin_b,
            ),
            &contact,
        );
        let scale = before.energy.sqrt().max(1.0);
        let tolerance = 1e-5 * scale * scale;
        assert!((after.momentum.0 - before.momentum.0).abs() <= 1e-5 * scale * 4.0);
        assert!((after.momentum.1 - before.momentum.1).abs() <= 1e-5 * scale * 4.0);
        assert!(
            (after.angular - before.angular).abs() <= 1e-4 * scale * 4.0,
            "{} {}",
            before.angular,
            after.angular
        );
        assert!(after.energy <= before.energy + tolerance, "energy grew");
        if restitution == 1.0 && friction == 0.0 {
            assert!(
                (after.energy - before.energy).abs() <= tolerance,
                "energy lost"
            );
        }
        assert!(
            after.closing >= -1e-4 * scale,
            "left approaching: {}",
            after.closing
        );
        assert!(bumped.friction_impulse.abs() <= friction * bumped.normal_impulse * (1.0 + 1e-6));
        // Beyond the rounding of the closing speed, an approach is met by
        // a push and a parting pair by none.
        if before.closing < -1e-4 * scale {
            approaching += 1;
            assert!(bumped.normal_impulse > 0.0);
        } else if before.closing > 1e-4 * scale {
            assert_eq!(bumped.normal_impulse, 0.0);
        }
    }
    assert!(approaching > 100, "{approaching}");
}

/// Defect: an inertia helper with the wrong moment, so every turn a bump
/// gives is off. Oracle: the textbook moments, `m (w² + h²) / 12` for a
/// box `w` by `h` and `m r² / 2` for a disc: a 4 x 2 box of 1200 has 2000,
/// a disc of radius 0.5 and mass 2 has a quarter; an infinite mass gives
/// zero.
#[wasm_bindgen_test(unsupported = test)]
fn the_inertia_helpers_give_the_textbook_moments() {
    assert_near(
        planar::box_inverse_inertia(1200.0, v2(2.0, 1.0)),
        1.0 / 2000.0,
        1e-9,
    );
    assert_near(planar::disc_inverse_inertia(2.0, 0.5), 4.0, 1e-6);
    assert_eq!(
        planar::box_inverse_inertia(f32::INFINITY, v2(2.0, 1.0)),
        0.0
    );
    assert_eq!(planar::disc_inverse_inertia(f32::INFINITY, 0.5), 0.0);
}

/// Defect: textbook collisions answered wrongly. Oracle: the textbook.
/// Equal balls meeting centre to centre without friction swap their
/// velocities at restitution 1 and share their mean at restitution 0; a
/// pair already parting is left exactly as it was.
#[wasm_bindgen_test(unsupported = test)]
fn equal_bodies_meeting_centre_to_centre_follow_the_textbook() {
    let a = Body {
        velocity: v2(3.0, 0.0),
        ..body(1, ball(0.5), v2(0.0, 0.0))
    };
    let b = Body {
        velocity: v2(-1.0, 0.0),
        ..body(2, ball(0.5), v2(1.0, 0.0))
    };
    let contact = Contact {
        point: v2(0.5, 0.0),
        normal: v2(1.0, 0.0),
    };
    let elastic = planar::bump(&a, &b, contact, 1.0, 0.0);
    assert_vec_near(elastic.velocity_a, v2(-1.0, 0.0), 1e-6);
    assert_vec_near(elastic.velocity_b, v2(3.0, 0.0), 1e-6);
    assert_eq!((elastic.spin_a, elastic.spin_b), (0.0, 0.0));
    let plastic = planar::bump(&a, &b, contact, 0.0, 0.0);
    assert_vec_near(plastic.velocity_a, v2(1.0, 0.0), 1e-6);
    assert_vec_near(plastic.velocity_b, v2(1.0, 0.0), 1e-6);

    let parting = Body {
        velocity: v2(-3.0, 0.0),
        ..a
    };
    let left = planar::bump(&parting, &b, contact, 1.0, 1.0);
    assert_eq!(
        (left.velocity_a, left.spin_a, left.velocity_b, left.spin_b),
        (parting.velocity, parting.spin, b.velocity, b.spin)
    );
    assert_eq!(left.normal_impulse, 0.0);
}

/// Defect: a wall or a parked prop knocked by a bump. Oracle: the
/// contract (zero inverse mass and inertia: unchanged). Over seeded random
/// contacts, an immovable body keeps its velocity and spin bit for bit,
/// and the other body is never left approaching it.
#[wasm_bindgen_test(unsupported = test)]
fn an_immovable_body_is_unchanged_by_a_bump() {
    let mut rng = SplitMix64::new(77);
    for _ in 0..200 {
        let (a, b, contact) = random_pair(&mut rng);
        let wall = fixed(a);
        let bumped = planar::bump(&wall, &b, contact, draw(&mut rng, 0.0, 1.0), 0.8);
        assert_eq!(bumped.velocity_a.x.to_bits(), wall.velocity.x.to_bits());
        assert_eq!(bumped.velocity_a.y.to_bits(), wall.velocity.y.to_bits());
        assert_eq!(bumped.spin_a.to_bits(), wall.spin.to_bits());
    }
}

// --------------------------------------------------------------- advance

fn everything(_: &Body, _: &Body) -> bool {
    true
}

/// The most any pair of `bodies` overlaps, by their own definitions.
fn deepest(bodies: &[Body]) -> f64 {
    let mut deepest = f64::NEG_INFINITY;
    for (i, a) in bodies.iter().enumerate() {
        for b in &bodies[i + 1..] {
            deepest = deepest.max(-separation(a, b));
        }
    }
    deepest
}

fn unit_box(id: u64, x: f32, velocity: f32) -> Body {
    Body {
        velocity: v2(velocity, 0.0),
        ..body(id, cuboid(0.5, 0.5), v2(x, 0.0))
    }
}

/// Defect: impacts out of time order, resolved at the wrong instant, or a
/// row left overlapping. Oracle: geometry and the textbook. Three unit
/// boxes on a line, one apart; the first runs at 8. It meets the second
/// after an eighth of a second and stops (equal masses, restitution 1, no
/// friction); the second meets the third an eighth later and stops; the
/// third leaves at 8. Over a 0.3 s step: two impacts, at 0.125 / 0.3 and
/// 0.25 / 0.3 of the step, each earlier by the travel of the contact gaps
/// so far (three quarters to one and a quarter slops each, at 8), in that
/// order whatever the ids, and no pair overlapping.
#[wasm_bindgen_test(unsupported = test)]
fn three_in_a_line_meet_in_time_order() {
    let mut bodies = [
        unit_box(30, 0.0, 8.0),
        unit_box(10, 2.0, 0.0),
        unit_box(20, 4.0, 0.0),
    ];
    let slop = 0.005;
    let impacts = planar::advance(&mut bodies, 0.3, &Settings::new(1.0, 0.0), everything);
    assert_eq!(impacts.len(), 2, "{impacts:?}");
    assert_eq!((impacts[0].a, impacts[0].b), (10, 30));
    assert_eq!((impacts[1].a, impacts[1].b), (10, 20));
    for (impact, (at, gaps)) in impacts.iter().zip([(0.125f32, 1.0f32), (0.25, 2.0)]) {
        let latest = (at - gaps * 0.75 * slop / 8.0) / 0.3;
        let earliest = (at - gaps * 1.25 * slop / 8.0) / 0.3;
        assert!(
            impact.time >= earliest - 1e-5 && impact.time <= latest + 1e-5,
            "{impact:?}"
        );
        // A unit mass stopped from 8.
        assert_near(impact.impulse, 8.0, 1e-3);
    }
    assert_vec_near(impacts[0].normal, v2(-1.0, 0.0), 1e-6);
    assert_vec_near(impacts[1].normal, v2(1.0, 0.0), 1e-6);
    assert!(impacts[0].time < impacts[1].time);
    let by_id = |id: u64| bodies.iter().find(|body| body.id == id).expect("there");
    assert_vec_near(by_id(30).velocity, Vec2::ZERO, 1e-4);
    assert_vec_near(by_id(10).velocity, Vec2::ZERO, 1e-4);
    assert_vec_near(by_id(20).velocity, v2(8.0, 0.0), 1e-4);
    for body in &bodies {
        assert_near(body.spin, 0.0, 1e-4);
    }
    assert!(deepest(&bodies) <= 0.0, "{}", deepest(&bodies));
}

/// Defect: impacts taken in the order of the pairs rather than of time,
/// so a later bump is resolved first and an earlier one passed through.
/// Oracle: geometry by hand. Two separate pairs of unit boxes closing at 8:
/// the pair with the lower ids has a gap of 2 (meeting at a quarter
/// second), the other a gap of 1 (an eighth); over a 0.3 s step they are
/// reported in time order, each at its own time less a slop's travel, and
/// neither pair ends inside the other.
#[wasm_bindgen_test(unsupported = test)]
fn impacts_of_separate_pairs_come_in_time_order() {
    let pair = |id: u64, y: f32, gap: f32| {
        [
            Body {
                velocity: v2(4.0, 0.0),
                ..body(id, cuboid(0.5, 0.5), v2(0.0, y))
            },
            Body {
                velocity: v2(-4.0, 0.0),
                ..body(id + 1, cuboid(0.5, 0.5), v2(1.0 + gap, y))
            },
        ]
    };
    let [a, b] = pair(1, 0.0, 2.0);
    let [c, d] = pair(3, 5.0, 1.0);
    let mut bodies = [a, b, c, d];
    let impacts = planar::advance(&mut bodies, 0.3, &Settings::new(0.5, 0.0), everything);
    assert_eq!(impacts.len(), 2, "{impacts:?}");
    let slop = 0.005;
    for (impact, (ids, gap)) in impacts.iter().zip([((3, 4), 1.0f32), ((1, 2), 2.0)]) {
        assert_eq!((impact.a, impact.b), ids);
        let earliest = (gap - 1.25 * slop) / 8.0 / 0.3;
        let latest = (gap - 0.75 * slop) / 8.0 / 0.3;
        assert!(
            impact.time >= earliest - 1e-5 && impact.time <= latest + 1e-5,
            "{impact:?}"
        );
    }
    assert!(deepest(&bodies) <= 0.0, "{}", deepest(&bodies));
}

/// Defect: bodies that start inside each other left so, or a wall pushed
/// out of the way. Oracle: the shapes' own definitions in `f64` and the
/// contract (separation split by inverse mass; zero moves nothing). Three
/// boxes at rest, the first half inside an immovable wall and the next
/// inside the first, are pushed apart in one step: no pair overlaps after
/// it, the wall has not moved, and the box in the wall has left it.
#[wasm_bindgen_test(unsupported = test)]
fn bodies_that_start_inside_each_other_are_pushed_apart() {
    let wall = fixed(body(1, cuboid(0.5, 3.0), v2(0.0, 0.0)));
    let mut bodies = [
        wall,
        body(2, cuboid(0.5, 0.5), v2(0.7, 0.1)),
        body(3, cuboid(0.5, 0.5), v2(1.5, -0.2)),
    ];
    assert!(deepest(&bodies) > 0.1);
    let impacts = planar::advance(
        &mut bodies,
        1.0 / 60.0,
        &Settings::new(0.0, 0.0),
        everything,
    );
    assert!(impacts.is_empty(), "{impacts:?}");
    assert_eq!(bodies[0], wall);
    assert!(bodies[1].position.x >= 1.0, "{:?}", bodies[1]);
    assert!(deepest(&bodies) <= 1e-5, "{}", deepest(&bodies));
}

/// Defect: a pair bumped again and again at the same instant because the
/// rounding of a bump that stopped it reads as a new approach, filling the
/// impacts with echoes. Oracle: the physics (a plastic collision of two
/// bodies is one impact). Over seeded pairs of turned boxes running into
/// each other with restitution 0, each step reports exactly one impact.
#[wasm_bindgen_test(unsupported = test)]
fn a_plastic_collision_is_one_impact() {
    let mut rng = SplitMix64::new(0x0b1a);
    for _ in 0..60 {
        let mut bodies = [
            Body {
                heading: random_heading(&mut rng),
                velocity: v2(draw(&mut rng, 3.0, 9.0), draw(&mut rng, -1.0, 1.0)),
                spin: draw(&mut rng, -2.0, 2.0),
                ..body(1, cuboid(1.0, 0.5), v2(-2.0, draw(&mut rng, -0.3, 0.3)))
            },
            Body {
                heading: random_heading(&mut rng),
                velocity: v2(draw(&mut rng, -9.0, -3.0), draw(&mut rng, -1.0, 1.0)),
                spin: draw(&mut rng, -2.0, 2.0),
                ..body(2, cuboid(1.0, 0.5), v2(2.0, draw(&mut rng, -0.3, 0.3)))
            },
        ];
        let impacts = planar::advance(&mut bodies, 0.5, &Settings::new(0.0, 0.4), everything);
        assert_eq!(impacts.len(), 1, "{impacts:?}");
    }
}

/// Defect: a bump elsewhere changes the motion of a body it does not
/// touch. Oracle: the contract (a body's motion over the step is its own
/// velocity and spin until it is bumped). A spinning car far from a pair
/// that collides mid-step ends with the same bits as when stepped alone.
#[wasm_bindgen_test(unsupported = test)]
fn a_body_no_bump_touches_keeps_its_own_motion() {
    let loner = car(9, v2(40.0, 30.0), v2(0.6, 0.8), v2(13.0, -7.0), 2.5);
    let mut alone = [loner];
    planar::advance(&mut alone, 1.0 / 30.0, &Settings::new(0.3, 0.5), everything);
    let mut together = [unit_box(1, 0.0, 30.0), unit_box(2, 1.6, -30.0), loner];
    let impacts = planar::advance(
        &mut together,
        1.0 / 30.0,
        &Settings::new(0.3, 0.5),
        everything,
    );
    assert_eq!(impacts.len(), 1);
    assert!(impacts[0].time > 0.1 && impacts[0].time < 0.9);
    assert_eq!(together[2], alone[0]);
}

/// A seeded pile-up: `count` bodies of every kind at random headings
/// around the origin, each running toward it and spinning, at least
/// `spacing` apart on a grid so none overlaps at the start.
fn pile_up(seed: u64, count: usize, spacing: f32) -> Vec<Body> {
    let mut rng = SplitMix64::new(seed);
    let side = (1..=count)
        .find(|side| side * side >= count)
        .expect("a square holds them");
    (0..count)
        .map(|i| {
            let shape = match pick(&mut rng, 3) {
                0 => cuboid(draw(&mut rng, 0.6, 1.0), draw(&mut rng, 0.3, 0.5)),
                1 => ball(draw(&mut rng, 0.3, 0.6)),
                _ => random_shape(&mut rng),
            };
            let at = v2(
                (i % side) as f32 * spacing - spacing * side as f32 / 2.0,
                (i / side) as f32 * spacing - spacing * side as f32 / 2.0,
            );
            Body {
                heading: random_heading(&mut rng),
                velocity: -at * draw(&mut rng, 1.0, 3.0)
                    + v2(draw(&mut rng, -2.0, 2.0), draw(&mut rng, -2.0, 2.0)),
                spin: draw(&mut rng, -4.0, 4.0),
                ..body(100 + 7 * i as u64 % 13, shape, at)
            }
        })
        .collect()
}

/// Defect: a result that depends on the order of the slice. Oracle: the
/// contract (order never shows): the same pile-up in three orders gives
/// the same bodies and impacts, bit for bit, over a dozen steps.
#[wasm_bindgen_test(unsupported = test)]
fn a_shuffled_slice_gives_the_same_result_bit_for_bit() {
    let run = |mut bodies: Vec<Body>| {
        let mut hasher = StateHasher::new();
        for _ in 0..12 {
            let impacts = planar::advance(
                &mut bodies,
                1.0 / 30.0,
                &Settings::new(0.3, 0.5),
                everything,
            );
            for impact in &impacts {
                hash_impact(&mut hasher, impact);
            }
            let mut sorted = bodies.clone();
            sorted.sort_by_key(|body| body.id);
            for body in &sorted {
                body.canonical_write(&mut hasher);
            }
        }
        hasher.finish().to_hex()
    };
    let bodies = pile_up(5, 9, 2.5);
    let mut reversed = bodies.clone();
    reversed.reverse();
    let mut rotated = bodies.clone();
    rotated.rotate_left(4);
    let forward = run(bodies);
    assert_eq!(forward, run(reversed));
    assert_eq!(forward, run(rotated));
}

fn hash_impact(hasher: &mut StateHasher, impact: &planar::Impact) {
    hasher.u64(impact.a);
    hasher.u64(impact.b);
    hasher.u32(impact.time.to_bits());
    for value in [
        impact.point.x,
        impact.point.y,
        impact.normal.x,
        impact.normal.y,
        impact.impulse,
    ] {
        hasher.u32(value.to_bits());
    }
}

/// Defect: a wall pushed by a bump or by the separation. Oracle: the
/// contract (zero inverse mass: never moved but by its own velocity).
/// Cars driven into a wall at rest from both sides, with spin, over many
/// steps: the wall's position, heading, velocity and spin stay as they
/// were, bit for bit, and the cars are bumped by it.
#[wasm_bindgen_test(unsupported = test)]
fn a_body_with_zero_inverse_mass_does_not_move() {
    let wall = fixed(body(1, cuboid(0.25, 4.0), v2(0.0, 0.0)));
    let car = |id, x: f32, vx: f32, spin| Body {
        velocity: v2(vx, 1.0),
        spin,
        heading: v2(0.96, 0.28),
        ..body(id, cuboid(1.0, 0.5), v2(x, 0.0))
    };
    let mut bodies = [car(2, -3.0, 9.0, 1.5), wall, car(3, 3.0, -7.0, -2.0)];
    let mut impacts = 0;
    for _ in 0..30 {
        impacts += planar::advance(
            &mut bodies,
            1.0 / 60.0,
            &Settings::new(0.2, 0.4),
            everything,
        )
        .len();
        assert_eq!(bodies[1], wall);
        assert!(deepest(&bodies) <= 1e-5, "{}", deepest(&bodies));
    }
    assert!(impacts >= 2);
    assert!(bodies[0].velocity.x < 0.0 && bodies[2].velocity.x > 0.0);
}

/// Defect: a pile-up that runs past its caps, or ends with bodies inside
/// each other. Oracle: the caps and the shapes' own definitions in `f64`.
/// Ten bodies of every kind thrown together and spinning, over sixty
/// steps: no step reports more impacts than its cap, and after each step
/// no pair overlaps by more than rounding (1e-4: a few units in the last
/// place of coordinates up to 10, through up to sixteen passes).
#[wasm_bindgen_test(unsupported = test)]
fn a_pile_up_of_ten_stays_within_the_caps_and_ends_apart() {
    let mut bodies = pile_up(2, 10, 2.2);
    assert!(deepest(&bodies) < 0.0);
    let mut total = 0;
    for _ in 0..60 {
        let impacts = planar::advance(
            &mut bodies,
            1.0 / 60.0,
            &Settings::new(0.2, 0.6),
            everything,
        );
        assert!(impacts.len() <= MAX_IMPACTS);
        assert!(impacts.windows(2).all(|pair| pair[0].time <= pair[1].time));
        total += impacts.len();
        let deepest = deepest(&bodies);
        assert!(deepest <= 1e-4, "{deepest}");
    }
    assert!(total > 10, "{total}");
}

/// The pile-up moved by `shift`.
fn shifted(bodies: &[Body], shift: Vec2) -> Vec<Body> {
    bodies
        .iter()
        .map(|body| Body {
            position: body.position + shift,
            ..*body
        })
        .collect()
}

/// A pile-up on a grid of eighths: positions, velocities and headings
/// whose sums and differences with a power-of-two shift are exact.
fn coarse_pile_up() -> Vec<Body> {
    let mut rng = SplitMix64::new(44);
    let mut grid = |low: f32, high: f32| (draw(&mut rng, low, high) * 8.0).round() / 8.0;
    let mut bodies = Vec::new();
    for i in 0..6u64 {
        let at = v2((i % 3) as f32 * 2.5 - 2.5, (i / 3) as f32 * 2.5 - 1.25);
        bodies.push(Body {
            heading: v2(grid(-1.0, 1.0), grid(0.25, 1.0)),
            velocity: -at * 2.0 + v2(grid(-1.0, 1.0), grid(-1.0, 1.0)),
            spin: grid(-3.0, 3.0),
            ..body(
                i,
                if i % 2 == 0 {
                    cuboid(0.875, 0.5)
                } else {
                    ball(0.5)
                },
                at,
            )
        });
    }
    bodies
}

/// Defect: a result that depends on where in the world the scene is (a
/// driving game's route runs for kilometres). Oracle: the contract (near
/// is as good as far). The same pile-up at the origin and 4096 and -8192
/// away, on a grid of eighths so the shift keeps every difference between
/// positions exact: one step gives bit-equal impacts (but for their points,
/// rounded once into world coordinates), velocities, spins and headings,
/// and positions within one unit in the last place of the far coordinate
/// (2^-11 at 4096, 2^-10 at 8192) of the near ones moved by the shift.
#[wasm_bindgen_test(unsupported = test)]
fn far_from_the_origin_is_the_same_as_at_it() {
    let shift = v2(4096.0, -8192.0);
    let mut near = coarse_pile_up();
    let mut far = shifted(&near, shift);
    let settings = Settings::new(0.4, 0.3);
    let near_impacts = planar::advance(&mut near, 0.25, &settings, everything);
    let far_impacts = planar::advance(&mut far, 0.25, &settings, everything);
    assert!(!near_impacts.is_empty());
    assert_eq!(near_impacts.len(), far_impacts.len());
    for (n, f) in near_impacts.iter().zip(&far_impacts) {
        assert_eq!((n.a, n.b, n.time.to_bits()), (f.a, f.b, f.time.to_bits()));
        assert_eq!(n.normal, f.normal);
        assert_eq!(n.impulse.to_bits(), f.impulse.to_bits());
        assert_vec_near(f.point - shift, n.point, 2.0f32.powi(-10));
    }
    for (n, f) in near.iter().zip(&far) {
        assert_eq!(
            (n.velocity, n.spin, n.heading),
            (f.velocity, f.spin, f.heading)
        );
        assert!((f.position.x - shift.x - n.position.x).abs() <= 2.0f32.powi(-11));
        assert!((f.position.y - shift.y - n.position.y).abs() <= 2.0f32.powi(-10));
    }
}

/// Defect: accuracy that falls with distance from the origin. Oracle: the
/// contract (near is as good as far), within a stated tolerance where the
/// shift is not exact. Two 4 x 2 cars meet off-centre (so the bump also
/// turns them) at positions off any grid, at the origin and 3000.37 and
/// -2511.9 away. Over ten steps the far pair reports the same impact and
/// ends within a slop (0.02 for these cars) of the near pair moved by the
/// shift: the time of impact lands anywhere within its band, half a slop
/// wide, so a rounding difference may move the contact by up to that, and
/// the rest is ten roundings of each position into world coordinates (at
/// most 2^-12 each there). A contact moved by half a slop changes the
/// spin a bump gives by at most its impulse (under the cars' mass times
/// their closing speed doubled, 1200 x 26 x 2) times that distance times
/// the inverse inertia: 0.32 rad/s, and the velocity at a corner by that
/// times its reach.
#[wasm_bindgen_test(unsupported = test)]
fn far_from_the_origin_stays_within_rounding_of_the_scene_at_it() {
    let shift = v2(3000.37, -2511.9);
    let mut near = vec![
        car(1, v2(-3.079_13, 0.271_83), v2(1.0, 0.0), v2(14.3, 0.0), 0.0),
        car(
            2,
            v2(3.333_33, 1.392_37),
            v2(0.995, 0.0998),
            v2(-11.7, 0.0),
            0.0,
        ),
    ];
    let mut far = shifted(&near, shift);
    let settings = Settings::new(0.4, 0.3);
    let (mut near_impacts, mut far_impacts) = (Vec::new(), Vec::new());
    for _ in 0..10 {
        near_impacts.extend(planar::advance(
            &mut near,
            1.0 / 30.0,
            &settings,
            everything,
        ));
        far_impacts.extend(planar::advance(&mut far, 1.0 / 30.0, &settings, everything));
    }
    assert_eq!(near_impacts.len(), 1, "{near_impacts:?}");
    assert_eq!(far_impacts.len(), 1, "{far_impacts:?}");
    assert_vec_near(far_impacts[0].normal, near_impacts[0].normal, 1e-3);
    assert!(near[0].spin.abs() > 1.0, "the bump turns the cars");
    let slop = 0.02;
    for (n, f) in near.iter().zip(&far) {
        assert_vec_near(f.position - shift, n.position, slop);
        assert_near(f.spin, n.spin, 0.32);
        assert_vec_near(f.velocity, n.velocity, 0.32 * 5.0f32.sqrt());
    }
}

/// Defect: state that does not survive the trip to another machine, or a
/// step whose result depends on something not in the state. Oracle: the
/// contract (plain data; restoring and repeating repeats the result). A
/// pile-up saved through postcard after ten steps and replayed for twenty
/// more matches the run that never stopped, bit for bit.
#[wasm_bindgen_test(unsupported = test)]
fn a_replay_from_a_saved_slice_repeats_bit_for_bit() {
    let settings = Settings::new(0.3, 0.5);
    let steps = |bodies: &mut Vec<Body>, count: usize| {
        let mut hasher = StateHasher::new();
        for _ in 0..count {
            for impact in planar::advance(bodies, 1.0 / 30.0, &settings, everything) {
                hash_impact(&mut hasher, &impact);
            }
            for body in bodies.iter() {
                body.canonical_write(&mut hasher);
            }
        }
        hasher.finish().to_hex()
    };
    let mut forward = pile_up(9, 8, 2.4);
    steps(&mut forward, 10);
    let saved = postcard::to_allocvec(&forward).expect("bodies serialise");
    let ahead = steps(&mut forward, 20);
    let mut restored: Vec<Body> = postcard::from_bytes(&saved).expect("bodies deserialise");
    assert_eq!(steps(&mut restored, 20), ahead);
    assert_eq!(restored, forward);
}

/// Defect: a field added to `Body` without its documented largest size
/// growing. Oracle: postcard's own encoder, on the largest body (the
/// largest id and an eight-sided polygon), must produce exactly the
/// documented size.
#[wasm_bindgen_test(unsupported = test)]
fn the_largest_body_encodes_at_its_documented_size() {
    let points: Vec<Vec2> = (0..8)
        .map(|i| {
            let angle = core::f32::consts::TAU * i as f32 / 8.0;
            v2(angle.cos(), angle.sin())
        })
        .collect();
    let largest = Body {
        id: u64::MAX,
        ..body(
            0,
            Shape::Convex(Convex::polygon(&points, 1.0, 0.0)),
            v2(1.5, -2.5),
        )
    };
    let encoded = postcard::to_allocvec(&largest).expect("a body serialises");
    assert_eq!(encoded.len(), Body::MAX_POSTCARD_BYTES);
}

// ------------------------------------------------------- game scenarios

/// A car: a 4 x 2 box of 1200 kg.
fn car(id: u64, position: Vec2, heading: Vec2, velocity: Vec2, spin: f32) -> Body {
    let half = v2(2.0, 1.0);
    Body {
        id,
        shape: Shape::Box { half },
        position,
        heading,
        velocity,
        spin,
        inverse_mass: 1.0 / 1200.0,
        inverse_inertia: planar::box_inverse_inertia(1200.0, half),
    }
}

/// Defect: a car spinning in place passes through the car beside it,
/// because the sweep sees only translation. Oracle: geometry by hand (#924,
/// vehicles). A 4 x 2 car spinning at 30 rad/s beside a parked one whose
/// side is 0.6 from its own: its corner, `sqrt(5)` from its centre, sweeps
/// up to 1.6 from the centre line within the first step, inside the parked
/// car's span. The step reports the impact, the parked car is pushed away,
/// and neither ends inside the other; with no spin, nothing happens.
#[wasm_bindgen_test(unsupported = test)]
fn two_cars_that_meet_only_because_one_spins_are_caught() {
    let spinner = car(1, v2(0.0, 0.0), v2(1.0, 0.0), Vec2::ZERO, 30.0);
    let parked = car(2, v2(0.0, 2.6), v2(1.0, 0.0), Vec2::ZERO, 0.0);
    let settings = Settings::new(0.2, 0.5);

    let mut calm = [
        Body {
            spin: 0.0,
            ..spinner
        },
        parked,
    ];
    assert!(planar::advance(&mut calm, 1.0 / 60.0, &settings, everything).is_empty());

    let mut bodies = [spinner, parked];
    let impacts = planar::advance(&mut bodies, 1.0 / 60.0, &settings, everything);
    assert_eq!(impacts.len(), 1, "{impacts:?}");
    assert_eq!((impacts[0].a, impacts[0].b), (1, 2));
    assert!(impacts[0].impulse > 0.0);
    assert!(bodies[1].velocity.y > 0.0, "{:?}", bodies[1]);
    assert!(bodies[0].spin < 30.0);
    assert!(deepest(&bodies) <= 1e-5, "{}", deepest(&bodies));
}

/// Defect: a car scraping a wall is stopped dead, bounced off it, or
/// pushed into it (#924, vehicles). Oracle: the requirement, with bounds
/// from the friction law: a car at 30 along a wall and 3 into it, with
/// restitution 0 and friction 0.2, loses at most what friction can take
/// (a fifth of the impulses that stop its 3 into the wall, and of the
/// turns they cause), so it keeps at least nine tenths of its speed along
/// the wall; it ends no longer moving into the wall (within a hundredth of
/// what it started with) and never inside it.
#[wasm_bindgen_test(unsupported = test)]
fn a_car_scraping_a_wall_keeps_its_speed_along_it() {
    let wall = fixed(body(1, cuboid(200.0, 0.5), v2(0.0, -0.5)));
    let scraping = car(2, v2(-20.0, 1.5), v2(1.0, 0.0), v2(30.0, -3.0), 0.0);
    let mut bodies = [wall, scraping];
    let mut impacts = 0;
    for _ in 0..60 {
        impacts += planar::advance(
            &mut bodies,
            1.0 / 60.0,
            &Settings::new(0.0, 0.2),
            everything,
        )
        .len();
        assert!(deepest(&bodies) <= 1e-5, "{}", deepest(&bodies));
    }
    assert!(impacts >= 1);
    let after = bodies[1];
    assert!(after.velocity.x >= 27.0, "{after:?}");
    assert!(after.velocity.y >= -0.03, "{after:?}");
    assert_eq!(bodies[0], wall);
}

/// Defect: height ignored, so a car on a road collides with the bridge
/// above it (#924, vehicles). Oracle: the requirement, through the filter
/// the game gives: ids below 100 are on the road, the rest on the bridge.
/// A bridge's two rails run along y; a car drives along the bridge
/// between them and another along the road beneath, across the rails'
/// footprint. Neither meets anything, and each ends exactly where its
/// velocity takes it; the same scene with every pair colliding reports
/// impacts, so it is the filter that keeps the levels apart.
#[wasm_bindgen_test(unsupported = test)]
fn a_bridge_over_a_road_does_not_meet_the_road_beneath() {
    let rail = |id, x| fixed(body(id, cuboid(0.25, 12.0), v2(x, 0.0)));
    let on_bridge = car(102, v2(0.0, -8.0), v2(0.0, 1.0), v2(0.0, 20.0), 0.0);
    let under = car(1, v2(-8.0, 0.0), v2(1.0, 0.0), v2(25.0, 0.0), 0.0);
    let level = |body: &Body| body.id >= 100;
    let same_level = |a: &Body, b: &Body| level(a) == level(b);
    let start = [rail(100, -3.0), rail(101, 3.0), on_bridge, under];

    let mut bodies = start;
    for _ in 0..30 {
        let impacts = planar::advance(
            &mut bodies,
            1.0 / 60.0,
            &Settings::new(0.2, 0.5),
            same_level,
        );
        assert!(impacts.is_empty(), "{impacts:?}");
    }
    assert_eq!(bodies[..2], start[..2]);
    assert_vec_near(bodies[2].position, v2(0.0, -8.0 + 20.0 * 0.5), 1e-3);
    assert_vec_near(bodies[3].position, v2(-8.0 + 25.0 * 0.5, 0.0), 1e-3);

    let mut together = start;
    let mut impacts = 0;
    for _ in 0..30 {
        impacts += planar::advance(
            &mut together,
            1.0 / 60.0,
            &Settings::new(0.2, 0.5),
            everything,
        )
        .len();
    }
    assert!(impacts > 0);
}

/// Defect: settings that make no sense accepted silently. Oracle: the
/// contract (settings are validated): a cap above its maximum panics.
#[wasm_bindgen_test(unsupported = test)]
#[should_panic(expected = "max_impacts")]
fn a_cap_above_its_maximum_is_refused() {
    let settings = Settings {
        max_impacts: MAX_IMPACTS + 1,
        ..Settings::new(0.5, 0.5)
    };
    planar::advance(&mut [], 0.1, &settings, everything);
}
