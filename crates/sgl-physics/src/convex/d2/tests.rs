//! Tests of the 2D kernel. Every expected value comes from geometry worked
//! in the test, or from a shape's own definition evaluated without the
//! kernel: never from the kernel's arithmetic.

use core::f32::consts::FRAC_1_SQRT_2;

use sgl_core::math::Vec2;
use wasm_bindgen_test::wasm_bindgen_test;

use super::*;
use crate::convex::testing::{Sequence, assert_contact_fraction, assert_near};
use crate::convex::{CastState, DistanceState};

/// The slop the cast and time-of-impact tests pass: a hundredth of a unit
/// for shapes a few units across.
const SLOP: f32 = 0.01;

/// Rounding allowance for shapes a few units across.
const EPS: f32 = 1e-5;

fn v(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

fn ball(x: f32, y: f32, radius: f32) -> Proxy {
    Proxy::new(&[v(x, y)], radius)
}

fn capsule(p: (f32, f32), q: (f32, f32), radius: f32) -> Proxy {
    Proxy::new(&[v(p.0, p.1), v(q.0, q.1)], radius)
}

/// An axis-aligned box by its centre and half-extents.
fn boxed(cx: f32, cy: f32, hx: f32, hy: f32) -> Proxy {
    Proxy::new(
        &[
            v(cx - hx, cy - hy),
            v(cx + hx, cy - hy),
            v(cx + hx, cy + hy),
            v(cx - hx, cy + hy),
        ],
        0.0,
    )
}

fn length(a: Vec2) -> f32 {
    (a.x * a.x + a.y * a.y).sqrt()
}

#[track_caller]
fn assert_vec_near(actual: Vec2, expected: Vec2, tolerance: f32) {
    assert!(
        length(actual - expected) <= tolerance,
        "{actual} is not within {tolerance} of {expected}"
    );
}

/// Asserts a separated result with the given distance, normal and points.
#[track_caller]
fn assert_separated(
    found: &ClosestPoints<Vec2>,
    distance: f32,
    normal: Vec2,
    point_a: Vec2,
    point_b: Vec2,
) {
    assert_eq!(found.state, DistanceState::Separated);
    assert_near(found.distance, distance, EPS);
    assert_vec_near(found.normal, normal, EPS);
    assert_vec_near(found.point_a, point_a, EPS);
    assert_vec_near(found.point_b, point_b, EPS);
}

/// Asserts a hit at the fraction geometry gives (see
/// [`assert_contact_fraction`]) with the given normal.
#[track_caller]
fn assert_hit(
    cast: &Cast<Vec2>,
    touch: f32,
    closing_speed: f32,
    total_radius: f32,
    normal: Vec2,
    slop: f32,
) {
    assert_eq!(cast.state, CastState::Hit);
    assert_contact_fraction(cast.fraction, touch, closing_speed, total_radius, slop);
    assert_vec_near(cast.normal, normal, 1e-4);
}

/// A proxy built under a rotation holds each point turned counter-clockwise
/// by the angle and then moved. Oracle: a quarter turn sends `(x, y)` to
/// `(-y, x)`, so `(1, 0)` and `(0, 2)` land on `(0, 1)` and `(-2, 0)`, and
/// the position `(10, 20)` is added after.
#[wasm_bindgen_test(unsupported = test)]
fn an_offset_proxy_turns_its_points_and_then_moves_them() {
    let quarter_turn = Rot { c: 0.0, s: 1.0 };
    let proxy = Proxy::offset(
        &[v(1.0, 0.0), v(0.0, 2.0)],
        0.25,
        v(10.0, 20.0),
        quarter_turn,
    );
    assert_eq!(proxy.points(), &[v(10.0, 21.0), v(8.0, 20.0)]);
    assert_eq!(proxy.radius().to_bits(), 0.25f32.to_bits());

    let unturned = Proxy::offset(&[v(1.0, 0.0)], 0.0, v(10.0, 20.0), Rot::IDENTITY);
    assert_eq!(unturned.points(), &[v(11.0, 20.0)]);
}

/// A proxy keeps at most `MAX_POINTS` points, the first ones. Oracle: the
/// capacity the crate states for a 2D polygon, eight vertices (#919).
#[wasm_bindgen_test(unsupported = test)]
fn a_proxy_keeps_its_first_eight_points() {
    let points: Vec<Vec2> = (0u8..12).map(|i| v(f32::from(i), 0.0)).collect();
    let proxy = Proxy::new(&points, 0.0);
    assert_eq!(proxy.points(), &points[..8]);
}

/// Two balls are apart by the distance between their centres less both
/// radii, along the line of centres. Oracle: centres `(0, 0)` and `(3, 4)`
/// are 5 apart (3-4-5), radii 1 and 1.5 leave 2.5, the normal is
/// `(0.6, 0.8)` and each point is its centre moved a radius along it.
#[wasm_bindgen_test(unsupported = test)]
fn balls_are_apart_by_their_centres_less_their_radii() {
    let a = ball(0.0, 0.0, 1.0);
    let b = ball(3.0, 4.0, 1.5);
    assert_separated(
        &distance(&a, &b, true),
        2.5,
        v(0.6, 0.8),
        v(0.6, 0.8),
        v(3.0 - 0.9, 4.0 - 1.2),
    );

    // Without the radii: the centres themselves, 5 apart.
    assert_separated(
        &distance(&a, &b, false),
        5.0,
        v(0.6, 0.8),
        v(0.0, 0.0),
        v(3.0, 4.0),
    );
}

/// A ball above a box's face is apart by its height over the face less its
/// radius; off a corner, by its distance to the corner less its radius.
/// Oracle: the box spans `[-2, 2] x [-1, 1]`. A ball of radius 0.5 centred
/// `(0.5, 3)` is 2 above the top face, so 1.5 apart, the points `(0.5, 1)`
/// and `(0.5, 2.5)`. A ball of radius 1 centred `(5, 5)` is `(3, 4)` from
/// the corner `(2, 1)`: 5 away, so 4 apart, along `(0.6, 0.8)`.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_meets_a_box_at_its_face_and_at_its_corner() {
    let block = boxed(0.0, 0.0, 2.0, 1.0);

    assert_separated(
        &distance(&block, &ball(0.5, 3.0, 0.5), true),
        1.5,
        v(0.0, 1.0),
        v(0.5, 1.0),
        v(0.5, 2.5),
    );

    assert_separated(
        &distance(&block, &ball(5.0, 5.0, 1.0), true),
        4.0,
        v(0.6, 0.8),
        v(2.0, 1.0),
        v(4.4, 4.2),
    );
}

/// A capsule is its segment grown by its radius. Oracle: the box spans
/// `[-2, 2] x [-1, 1]`. A capsule of radius 0.5 lying along `y = 3` over
/// the face is 2 above it, so 1.5 apart along `(0, 1)`; every point of the
/// segment is equally near, so only the heights of the points are fixed.
/// One from `(5, 5)` to `(8, 9)` of radius 1 has its near end `(3, 4)` from
/// the corner `(2, 1)`: 5 away, so 4 apart along `(0.6, 0.8)`.
#[wasm_bindgen_test(unsupported = test)]
fn a_capsule_meets_a_box_along_its_side_and_at_its_end() {
    let block = boxed(0.0, 0.0, 2.0, 1.0);

    let along = distance(&block, &capsule((-1.0, 3.0), (1.0, 3.0), 0.5), true);
    assert_eq!(along.state, DistanceState::Separated);
    assert_near(along.distance, 1.5, EPS);
    assert_vec_near(along.normal, v(0.0, 1.0), EPS);
    assert_near(along.point_a.y, 1.0, EPS);
    assert_near(along.point_b.y, 2.5, EPS);
    assert_near(along.point_a.x, along.point_b.x, EPS);
    assert!((-1.0..=1.0).contains(&along.point_a.x));

    assert_separated(
        &distance(&block, &capsule((5.0, 5.0), (8.0, 9.0), 1.0), true),
        4.0,
        v(0.6, 0.8),
        v(2.0, 1.0),
        v(4.4, 4.2),
    );
}

/// Two boxes side by side are apart by the gap between their faces, and
/// two boxes corner to corner by the distance between the corners. Oracle:
/// unit boxes centred `(0, 0)` and `(4, 0.5)` have faces at `x = 1` and
/// `x = 3`, 2 apart along `(1, 0)`, facing each other for `y` in
/// `[-0.5, 1]`. Centred `(0, 0)` and `(5, 6)` their nearest corners are
/// `(1, 1)` and `(4, 5)`: `(3, 4)` apart, so 5 along `(0.6, 0.8)`.
#[wasm_bindgen_test(unsupported = test)]
fn boxes_are_apart_across_a_face_and_across_a_corner() {
    let a = boxed(0.0, 0.0, 1.0, 1.0);

    let face = distance(&a, &boxed(4.0, 0.5, 1.0, 1.0), true);
    assert_eq!(face.state, DistanceState::Separated);
    assert_near(face.distance, 2.0, EPS);
    assert_vec_near(face.normal, v(1.0, 0.0), EPS);
    assert_near(face.point_a.x, 1.0, EPS);
    assert_near(face.point_b.x, 3.0, EPS);
    assert_near(face.point_a.y, face.point_b.y, EPS);
    assert!((-0.5..=1.0).contains(&face.point_a.y));

    assert_separated(
        &distance(&a, &boxed(5.0, 6.0, 1.0, 1.0), true),
        5.0,
        v(0.6, 0.8),
        v(1.0, 1.0),
        v(4.0, 5.0),
    );
}

/// A box turned an eighth of a turn presents its corners along the axes
/// and its faces along the diagonals. Oracle: a square of half-extent 1
/// turned 45 degrees has corners at distance sqrt(2) on the axes and faces
/// at distance 1 along the diagonals. A ball of radius 0.5 centred `(4, 0)`
/// is `4 - sqrt(2) - 0.5` from the corner; one centred 3 along the diagonal
/// is `3 - 1 - 0.5 = 1.5` from the face.
#[wasm_bindgen_test(unsupported = test)]
fn a_turned_box_is_measured_where_its_rotation_puts_it() {
    let eighth_turn = Rot {
        c: FRAC_1_SQRT_2,
        s: FRAC_1_SQRT_2,
    };
    let corners = [v(-1.0, -1.0), v(1.0, -1.0), v(1.0, 1.0), v(-1.0, 1.0)];
    let diamond = Proxy::offset(&corners, 0.0, Vec2::ZERO, eighth_turn);
    let sqrt2 = 2.0f32.sqrt();

    assert_separated(
        &distance(&diamond, &ball(4.0, 0.0, 0.5), true),
        4.0 - sqrt2 - 0.5,
        v(1.0, 0.0),
        v(sqrt2, 0.0),
        v(3.5, 0.0),
    );

    let diagonal = v(FRAC_1_SQRT_2, FRAC_1_SQRT_2);
    assert_separated(
        &distance(
            &diamond,
            &ball(3.0 * FRAC_1_SQRT_2, 3.0 * FRAC_1_SQRT_2, 0.5),
            true,
        ),
        1.5,
        diagonal,
        diagonal,
        diagonal * 2.5,
    );
}

/// A polygon's slanted face is met along that face's normal: the slope of
/// any ramp. Oracle: the triangle `(0, 0)`, `(4, 0)`, `(0, 3)` has a
/// hypotenuse with outward normal `(0.6, 0.8)` (3-4-5) and midpoint
/// `(2, 1.5)`. A ball of radius 0.5 centred 2 along the normal from the
/// midpoint, at `(3.2, 3.1)`, is 1.5 from the face.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_meets_a_slanted_face_along_its_normal() {
    let ramp = Proxy::new(&[v(0.0, 0.0), v(4.0, 0.0), v(0.0, 3.0)], 0.0);
    assert_separated(
        &distance(&ramp, &ball(3.2, 3.1, 0.5), true),
        1.5,
        v(0.6, 0.8),
        v(2.0, 1.5),
        v(2.9, 2.7),
    );
}

/// Cores that overlap report zero distance and say so, and shapes whose
/// radii overlap while their cores do not report zero distance but still a
/// normal. Oracle: a ball's centre inside a box, and two boxes sharing
/// area, have overlapping cores, and the one point reported lies in both:
/// at the ball's centre, or in the rectangle `[0.5, 2] x [-0.5, 1]` the
/// boxes `[-2, 2] x [-1, 1]` and `[0.5, 2.5] x [-0.5, 1.5]` share. Balls of
/// radius 1 centred 1.5 apart overlap by 0.5, but their centres are
/// distinct points 1.5 apart along `(1, 0)`.
#[wasm_bindgen_test(unsupported = test)]
fn overlapping_cores_report_zero_distance_and_say_so() {
    let block = boxed(0.0, 0.0, 2.0, 1.0);

    // Each shape with the corners of the region its core shares with the
    // box's.
    for (inside, low, high) in [
        (ball(0.5, 0.25, 0.5), v(0.5, 0.25), v(0.5, 0.25)),
        (ball(-1.9, 0.9, 0.0), v(-1.9, 0.9), v(-1.9, 0.9)),
        (boxed(1.5, 0.5, 1.0, 1.0), v(0.5, -0.5), v(2.0, 1.0)),
    ] {
        for use_radii in [false, true] {
            let found = distance(&block, &inside, use_radii);
            assert_eq!(found.state, DistanceState::Overlapped);
            assert_eq!(found.distance.to_bits(), 0.0f32.to_bits());
            assert_eq!(found.normal, Vec2::ZERO);
            // One point, shared by both cores.
            assert_eq!(found.point_a, found.point_b);
            let shared = found.point_a;
            assert!(
                (low - shared).max_element() <= EPS && (shared - high).max_element() <= EPS,
                "{shared} is not in both cores"
            );
        }
    }

    let a = ball(0.0, 0.0, 1.0);
    let b = ball(1.5, 0.0, 1.0);
    let rounded = distance(&a, &b, true);
    assert_eq!(rounded.state, DistanceState::Separated);
    assert_eq!(rounded.distance.to_bits(), 0.0f32.to_bits());
    assert_vec_near(rounded.normal, v(1.0, 0.0), EPS);
    let cores = distance(&a, &b, false);
    assert_near(cores.distance, 1.5, EPS);
}

/// A ball cast at a ball meets it when their centres are the sum of the
/// radii apart. Oracle: radius-1 balls with centres 10 apart, the mover
/// travelling 10 straight at the other, touch after 8: fraction 0.8, the
/// normal back along the motion and the point on the fixed ball's surface.
/// The same along `(0.6, 0.8)`.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_cast_at_a_ball_meets_it_a_radius_sum_away() {
    let fixed = ball(0.0, 0.0, 1.0);

    let head_on = shape_cast(
        &fixed,
        &ball(10.0, 0.0, 1.0),
        v(-10.0, 0.0),
        1.0,
        false,
        SLOP,
    );
    assert_hit(&head_on, 0.8, 10.0, 2.0, v(1.0, 0.0), SLOP);
    assert_vec_near(head_on.point, v(1.0, 0.0), 1e-4);

    let diagonal = shape_cast(
        &fixed,
        &ball(6.0, 8.0, 1.0),
        v(-6.0, -8.0),
        1.0,
        false,
        SLOP,
    );
    assert_hit(&diagonal, 0.8, 10.0, 2.0, v(0.6, 0.8), SLOP);
    assert_vec_near(diagonal.point, v(0.6, 0.8), 1e-4);
}

/// A ball cast at a box meets its face when its centre is a radius above
/// it, and its corner when its centre is a radius from it. Oracle: the box
/// spans `[-2, 2] x [-1, 1]`. A ball of radius 0.5 from `(0.5, 6)` moving
/// 10 down touches the top at centre height 1.5, after 4.5: fraction 0.45
/// at `(0.5, 1)`. A ball of radius 1 from `(8, 9)`, 10 from the corner
/// `(2, 1)` along `(0.6, 0.8)`, moving 20 straight at it touches after 9:
/// fraction 0.45 at the corner.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_cast_at_a_box_meets_its_face_and_its_corner() {
    let block = boxed(0.0, 0.0, 2.0, 1.0);

    let face = shape_cast(
        &block,
        &ball(0.5, 6.0, 0.5),
        v(0.0, -10.0),
        1.0,
        false,
        SLOP,
    );
    assert_hit(&face, 0.45, 10.0, 0.5, v(0.0, 1.0), SLOP);
    assert_vec_near(face.point, v(0.5, 1.0), 1e-4);

    let corner = shape_cast(
        &block,
        &ball(8.0, 9.0, 1.0),
        v(-12.0, -16.0),
        1.0,
        false,
        SLOP,
    );
    assert_hit(&corner, 0.45, 20.0, 1.0, v(0.6, 0.8), SLOP);
    assert_vec_near(corner.point, v(2.0, 1.0), 1e-4);
}

/// A capsule cast flat onto a box lands when its segment is a radius above
/// the face. Oracle: a capsule of radius 0.5 along `y = 5` moving 8 down
/// onto the top face `y = 1` touches at segment height 1.5, after 3.5:
/// fraction 0.4375, on the face under the segment.
#[wasm_bindgen_test(unsupported = test)]
fn a_capsule_cast_at_a_box_lands_a_radius_above_its_face() {
    let block = boxed(0.0, 0.0, 2.0, 1.0);
    let landed = shape_cast(
        &block,
        &capsule((-1.0, 5.0), (1.0, 5.0), 0.5),
        v(0.0, -8.0),
        1.0,
        false,
        SLOP,
    );
    assert_hit(&landed, 0.4375, 8.0, 0.5, v(0.0, 1.0), SLOP);
    assert_near(landed.point.y, 1.0, 1e-4);
    assert!((-1.0..=1.0).contains(&landed.point.x));
}

/// A box cast at a box meets it when the gap between them closes. Oracle:
/// unit boxes with faces 4 apart, the mover travelling 8, meet at fraction
/// 0.5 on the fixed box's face `x = 1`. Corner to corner, `(3, 4)` apart,
/// the mover travelling `(-6, -8)` meets at fraction 0.5 at the corner
/// `(1, 1)` along `(0.6, 0.8)`.
#[wasm_bindgen_test(unsupported = test)]
fn a_box_cast_at_a_box_meets_it_when_the_gap_closes() {
    let fixed = boxed(0.0, 0.0, 1.0, 1.0);

    let face = shape_cast(
        &fixed,
        &boxed(6.0, 0.5, 1.0, 1.0),
        v(-8.0, 0.0),
        1.0,
        false,
        SLOP,
    );
    assert_hit(&face, 0.5, 8.0, 0.0, v(1.0, 0.0), SLOP);
    assert_near(face.point.x, 1.0, 1e-4);
    assert!((-0.5..=1.0).contains(&face.point.y));

    let corner = shape_cast(
        &fixed,
        &boxed(5.0, 6.0, 1.0, 1.0),
        v(-6.0, -8.0),
        1.0,
        false,
        SLOP,
    );
    assert_hit(&corner, 0.5, 10.0, 0.0, v(0.6, 0.8), SLOP);
    assert_vec_near(corner.point, v(1.0, 1.0), 1e-4);
}

/// A ball cast at a turned box meets the corner the rotation put in its
/// way. Oracle: the square of half-extent 1 turned 45 degrees has a corner
/// at `(sqrt(2), 0)`. A ball of radius 0.5 from `(6, 0)` moving 10 toward
/// it touches when its centre is at `sqrt(2) + 0.5`: fraction
/// `(6 - sqrt(2) - 0.5) / 10`.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_cast_at_a_turned_box_meets_its_corner() {
    let eighth_turn = Rot {
        c: FRAC_1_SQRT_2,
        s: FRAC_1_SQRT_2,
    };
    let corners = [v(-1.0, -1.0), v(1.0, -1.0), v(1.0, 1.0), v(-1.0, 1.0)];
    let diamond = Proxy::offset(&corners, 0.0, Vec2::ZERO, eighth_turn);
    let sqrt2 = 2.0f32.sqrt();

    let met = shape_cast(
        &diamond,
        &ball(6.0, 0.0, 0.5),
        v(-10.0, 0.0),
        1.0,
        false,
        SLOP,
    );
    assert_hit(
        &met,
        (6.0 - sqrt2 - 0.5) / 10.0,
        10.0,
        0.5,
        v(1.0, 0.0),
        SLOP,
    );
    assert_vec_near(met.point, v(sqrt2, 0.0), 1e-4);
}

/// A cast that passes beside a shape, moves away from it, stops short of
/// it, or is cut off by `max_fraction` before reaching it, misses. Oracle:
/// the box's top face is at `y = 1`. A ball of radius 0.5 crossing at
/// height 3 stays 1.5 clear; one moving up from above never nears; one
/// from height 6 moving 3 down stops at 3, 1.5 clear; and the ball that
/// meets the face at fraction 0.45 is not met when only 0.4 of its motion
/// counts.
#[wasm_bindgen_test(unsupported = test)]
fn a_cast_that_never_reaches_a_shape_misses() {
    let block = boxed(0.0, 0.0, 2.0, 1.0);
    let cases = [
        (ball(-5.0, 3.0, 0.5), v(10.0, 0.0), 1.0),
        (ball(0.0, 3.0, 0.5), v(0.0, 10.0), 1.0),
        (ball(0.5, 6.0, 0.5), v(0.0, -3.0), 1.0),
        (ball(0.5, 6.0, 0.5), v(0.0, -10.0), 0.4),
    ];
    for (mover, translation, max_fraction) in cases {
        let cast = shape_cast(&block, &mover, translation, max_fraction, false, SLOP);
        assert_eq!(cast.state, CastState::Miss);
    }

    // The last one does meet the face once its whole motion counts.
    let whole = shape_cast(
        &block,
        &ball(0.5, 6.0, 0.5),
        v(0.0, -10.0),
        0.5,
        false,
        SLOP,
    );
    assert_eq!(whole.state, CastState::Hit);
}

/// A round shape that starts exactly touching a face meets it at once when
/// it moves into the face, and not at all when it moves along it. Oracle:
/// a ball of radius 0.5 centred 0.5 above the face touches it. Moving 1
/// into the face, contact is within `slop` of travel, so the fraction is no
/// more than `SLOP`, with the face's normal. Moving along the face it never
/// comes closer.
#[wasm_bindgen_test(unsupported = test)]
fn a_round_shape_touching_a_face_meets_it_only_when_moving_into_it() {
    let block = boxed(0.0, 0.0, 2.0, 1.0);
    let resting = ball(0.0, 1.5, 0.5);

    let into = shape_cast(&block, &resting, v(0.0, -1.0), 1.0, false, SLOP);
    assert_hit(&into, 0.0, 1.0, 0.5, v(0.0, 1.0), SLOP);

    let along = shape_cast(&block, &resting, v(1.0, 0.0), 1.0, false, SLOP);
    assert_eq!(along.state, CastState::Miss);
}

/// A cast that starts closer than the target distance says so, with the
/// normal between the cores when they are apart and none when they
/// overlap. Oracle: a ball of radius 0.5 centred 0.48 above the face is
/// 0.02 into it, deeper than `SLOP`: its surface is at height 0.98 and the
/// face at 1, so the point between them is at 0.99, and the face's normal
/// is `(0, 1)`. A ball centred inside the box has no normal to give. Two
/// boxes resting face to face have cores that touch, which is overlap for
/// shapes with no radius.
#[wasm_bindgen_test(unsupported = test)]
fn a_cast_that_starts_touching_or_overlapping_says_so() {
    let block = boxed(0.0, 0.0, 2.0, 1.0);

    let sunk = shape_cast(
        &block,
        &ball(0.0, 1.48, 0.5),
        v(0.0, -1.0),
        1.0,
        false,
        SLOP,
    );
    assert_eq!(sunk.state, CastState::InitialOverlap);
    assert_eq!(sunk.fraction.to_bits(), 0.0f32.to_bits());
    assert_vec_near(sunk.normal, v(0.0, 1.0), EPS);
    assert_vec_near(sunk.point, v(0.0, 0.99), EPS);

    let inside = shape_cast(
        &block,
        &ball(0.5, 0.25, 0.5),
        v(0.0, -1.0),
        1.0,
        false,
        SLOP,
    );
    assert_eq!(inside.state, CastState::InitialOverlap);
    assert_eq!(inside.fraction.to_bits(), 0.0f32.to_bits());
    assert_eq!(inside.normal, Vec2::ZERO);

    let resting = shape_cast(
        &block,
        &boxed(0.0, 2.0, 1.0, 1.0),
        v(0.0, -1.0),
        1.0,
        false,
        SLOP,
    );
    assert_eq!(resting.state, CastState::InitialOverlap);
    assert_eq!(resting.fraction.to_bits(), 0.0f32.to_bits());
}

/// With `can_encroach`, a round shape that starts closer than the target
/// may still move `slop` closer before it is stopped. Oracle: the ball
/// 0.02 into the face has its core 0.48 from it; allowed `SLOP` closer, it
/// stops at 0.47, after moving 0.01 of a motion of 1: a fraction of at
/// most 0.01 and more than three quarters of it, with the face's normal.
#[wasm_bindgen_test(unsupported = test)]
fn a_round_shape_that_starts_close_may_encroach_by_the_slop() {
    let block = boxed(0.0, 0.0, 2.0, 1.0);
    let cast = shape_cast(&block, &ball(0.0, 1.48, 0.5), v(0.0, -1.0), 1.0, true, SLOP);
    assert_eq!(cast.state, CastState::Hit);
    assert!(
        cast.fraction > 0.0075 - EPS && cast.fraction <= 0.01 + EPS,
        "fraction {}",
        cast.fraction
    );
    assert_vec_near(cast.normal, v(0.0, 1.0), EPS);
}

/// The slop is the caller's length, not a fixed one: the same cast with a
/// slop ten times larger stops ten times further from exact contact.
/// Oracle: the ball that touches the face at fraction 0.45 of a motion of
/// 10 stops `slop` past touching, so at `0.45 + slop / 10`, within a
/// quarter of that step.
#[wasm_bindgen_test(unsupported = test)]
fn a_cast_stops_within_the_slop_it_is_given() {
    let block = boxed(0.0, 0.0, 2.0, 1.0);
    for slop in [0.001, 0.01, 0.1] {
        let cast = shape_cast(
            &block,
            &ball(0.5, 6.0, 0.5),
            v(0.0, -10.0),
            1.0,
            false,
            slop,
        );
        assert_eq!(cast.state, CastState::Hit);
        let past_touching = (cast.fraction - 0.45) * 10.0;
        assert!(
            past_touching <= slop + EPS && past_touching >= 0.75 * slop - EPS,
            "slop {slop}: stopped {past_touching} past touching"
        );
    }
}

/// A cast whose distance never closes ends `Failed`, not as a miss, after
/// no more than the cap's worth of queries, at the fraction it had reached.
/// Oracle: a stand-in distance that always answers "one beyond the target,
/// straight ahead" lets every step advance `1 / 1000` of a motion 1000
/// long, so a cast that stops by its cap, and not by its data, has asked
/// `MAX_CAST_ITERATIONS` times and stands at that many thousandths.
#[wasm_bindgen_test(unsupported = test)]
fn a_cast_that_cannot_converge_fails_at_its_cap() {
    use core::cell::Cell;

    use crate::convex::{MAX_CAST_ITERATIONS, cast_by_advancement};

    let queries = Cell::new(0u16);
    let fixed = ball(0.0, 0.0, 0.0);
    let mover = ball(500.0, 0.0, 0.0);
    let cast = cast_by_advancement(&fixed, &mover, v(-1000.0, 0.0), 1.0, false, SLOP, |_, _| {
        queries.set(queries.get() + 1);
        ClosestPoints {
            state: DistanceState::Separated,
            point_a: Vec2::ZERO,
            point_b: v(SLOP + 1.0, 0.0),
            normal: v(1.0, 0.0),
            distance: SLOP + 1.0,
            iterations: 1,
        }
    });

    assert_eq!(cast.state, CastState::Failed);
    assert_eq!(u32::from(queries.get()), MAX_CAST_ITERATIONS);
    assert_near(cast.fraction, f32::from(queries.get()) / 1000.0, EPS);
    assert_vec_near(cast.normal, v(1.0, 0.0), EPS);
}

/// A distance query cut off before it settles says so and still answers
/// with points that are on the shapes, no nearer each other than the true
/// closest pair. Oracle: unit boxes centred `(0, 0)` and `(4, 0.5)` are 2
/// apart across their faces, and no pair of their points is nearer. The
/// full query takes more support points than the cut-off one is given.
#[wasm_bindgen_test(unsupported = test)]
fn a_distance_query_cut_off_early_answers_from_its_last_simplex() {
    let a = boxed(0.0, 0.0, 1.0, 1.0);
    let b = boxed(4.0, 0.5, 1.0, 1.0);

    let (full, _) = gjk(&a, &b, false, MAX_GJK_ITERATIONS);
    assert_eq!(full.state, DistanceState::Separated);
    assert!(full.iterations > 2);

    for cap in 1..full.iterations {
        let (cut, _) = gjk(&a, &b, false, cap);
        assert_eq!(cut.state, DistanceState::IterationCap);
        assert!((-1.0..=1.0).contains(&cut.point_a.x) && (-1.0..=1.0).contains(&cut.point_a.y));
        assert!((3.0..=5.0).contains(&cut.point_b.x) && (-0.5..=1.5).contains(&cut.point_b.y));
        assert_near(cut.distance, length(cut.point_b - cut.point_a), EPS);
        assert!(cut.distance >= 2.0 - EPS);
    }
}

/// The solvers find the part of a simplex nearest the origin, whichever
/// order its vertices come in, and point from it toward the origin.
/// Oracle: each simplex is placed so its nearest point is plain. One whose
/// points all have coordinates of 1 or more is nearest the origin at its
/// corner `(1, 1)`. One that lies in `x >= 1` and touches the line `x = 1`
/// only along the edge from `(1, -1)` to `(1, 1)` is nearest at that
/// edge's midpoint `(1, 0)`. The triangle with corner `(-1, -1)` and sides
/// of 4 along each axis holds the origin: nothing is nearer, and there is
/// no direction left to search. The number of vertices kept is the size of
/// that feature.
#[wasm_bindgen_test(unsupported = test)]
fn the_solvers_find_the_nearest_part_of_a_simplex_in_any_vertex_order() {
    let cases: [(&[Vec2], Vec2, usize); 5] = [
        // A segment: nearest at an end, and in its middle.
        (&[v(1.0, 1.0), v(3.0, 2.0)], v(1.0, 1.0), 1),
        (&[v(1.0, -1.0), v(1.0, 1.0)], v(1.0, 0.0), 2),
        // A triangle: nearest at a corner, on an edge, and holding the
        // origin.
        (&[v(1.0, 1.0), v(3.0, 1.0), v(1.0, 3.0)], v(1.0, 1.0), 1),
        (&[v(1.0, -1.0), v(1.0, 1.0), v(3.0, 0.0)], v(1.0, 0.0), 2),
        (&[v(-1.0, -1.0), v(3.0, -1.0), v(-1.0, 3.0)], Vec2::ZERO, 3),
    ];
    let orders: [&[usize]; 8] = [
        &[0, 1],
        &[1, 0],
        &[0, 1, 2],
        &[0, 2, 1],
        &[1, 0, 2],
        &[1, 2, 0],
        &[2, 0, 1],
        &[2, 1, 0],
    ];

    // Shape B is the origin alone, so a vertex `wA - wB` is a point of A
    // and the witness point on A is the simplex's nearest point.
    let origin = [Vec2::ZERO];
    for (points, nearest, kept) in cases {
        for order in orders.iter().filter(|order| order.len() == points.len()) {
            let mut simplex = Simplex {
                v: [SimplexVertex::new(points, &origin, order[0], 0); 3],
                count: order.len(),
            };
            for (slot, &index) in order.iter().enumerate() {
                simplex.v[slot] = SimplexVertex::new(points, &origin, index, 0);
            }
            let direction = if order.len() == 2 {
                solve_simplex2(&mut simplex)
            } else {
                solve_simplex3(&mut simplex)
            };

            assert_eq!(simplex.count, kept, "{points:?} in order {order:?}");
            assert_vec_near(witness_points(&simplex).0, nearest, EPS);
            if kept == 3 {
                assert_eq!(direction, Vec2::ZERO);
            } else {
                // Along the line from the nearest point to the origin, and
                // toward the origin.
                assert_near(cross(direction, nearest), 0.0, EPS);
                assert!(dot(direction, -nearest) > 0.0);
            }
        }
    }
}

// --- Time of impact -------------------------------------------------------

/// A sweep that neither moves nor turns.
fn still(x: f32, y: f32) -> Sweep {
    travelling((x, y), (x, y))
}

/// A sweep that moves without turning.
fn travelling(from: (f32, f32), to: (f32, f32)) -> Sweep {
    Sweep {
        local_center: Vec2::ZERO,
        c1: v(from.0, from.1),
        c2: v(to.0, to.1),
        q1: Rot::IDENTITY,
        q2: Rot::IDENTITY,
    }
}

/// Two boxes closing head on meet when the gap between them is gone.
/// Oracle: unit boxes with faces 4 apart, one travelling 8 toward the
/// other, touch at half the step. Shapes with no radius are met `slop`
/// short of touching, give or take a quarter of it, so the fraction is
/// `0.5 - slop / 8` within `0.25 slop / 8`; the normal runs from the still
/// box to the moving one and the point lies between the two faces.
#[wasm_bindgen_test(unsupported = test)]
fn boxes_closing_head_on_meet_when_the_gap_is_gone() {
    let unit = boxed(0.0, 0.0, 1.0, 1.0);
    let toi = time_of_impact(
        &unit,
        &still(0.0, 0.0),
        &unit,
        &travelling((6.0, 0.0), (-2.0, 0.0)),
        1.0,
        SLOP,
    );

    assert_eq!(toi.state, ToiState::Hit);
    let short_of_touching = (0.5 - toi.fraction) * 8.0;
    assert!(
        (short_of_touching - SLOP).abs() <= 0.25 * SLOP + EPS,
        "stopped {short_of_touching} short of touching"
    );
    assert_vec_near(toi.normal, v(1.0, 0.0), EPS);
    assert!(toi.point.x >= 1.0 - EPS && toi.point.x <= 1.0 + 1.25 * SLOP + EPS);
    assert!((-1.0..=1.0).contains(&toi.point.y));

    // The same motion described by another point of the box: the point
    // `(1, 2)` of the moving box travels from `(7, 2)` to `(-1, 2)`.
    let about_a_corner = Sweep {
        local_center: v(1.0, 2.0),
        ..travelling((7.0, 2.0), (-1.0, 2.0))
    };
    let same = time_of_impact(&unit, &still(0.0, 0.0), &unit, &about_a_corner, 1.0, SLOP);
    assert_eq!(same, toi);
}

/// A box that passes clear of another is separated for the whole step.
/// Oracle: unit boxes, one crossing 2.5 above the other's centre, keep a
/// gap of 0.5 throughout, far more than the slop.
#[wasm_bindgen_test(unsupported = test)]
fn a_box_passing_clear_of_another_never_meets_it() {
    let unit = boxed(0.0, 0.0, 1.0, 1.0);
    let toi = time_of_impact(
        &unit,
        &still(0.0, 0.0),
        &unit,
        &travelling((6.0, 2.5), (-6.0, 2.5)),
        1.0,
        SLOP,
    );
    assert_eq!(toi.state, ToiState::Separated);
    assert_eq!(toi.fraction.to_bits(), 1.0f32.to_bits());
}

/// Asserts a hit part way through the step, at a moment the hand's corner
/// is `gap` above the slab having been `clear_before` above it earlier.
#[track_caller]
fn assert_met(toi: &Toi, gap: f32, clear_before: f32, normal: Vec2) {
    assert_eq!(toi.state, ToiState::Hit);
    assert!(toi.fraction > 0.0 && toi.fraction < 1.0);
    assert!(
        (gap - SLOP).abs() <= 0.25 * SLOP + 1e-4,
        "the corner is {gap} above the slab at fraction {}",
        toi.fraction
    );
    assert!(clear_before > 2.0 * SLOP);
    assert_vec_near(toi.normal, normal, 1e-4);
}

/// A box that only turns reaches another by turning, and only when it
/// turns toward it. Oracle: a hand of half-extents `(2, 0.1)` is pinned by
/// the middle of its left end, 2.5 above the origin, over a slab whose top
/// face is at height 1; its far corners are `(4, ±0.1)` from the pin.
/// Turned clockwise to cosine `c` and sine `-s`, its lower far corner is at
/// height `2.5 - 4 s - 0.1 c`. A sweep's rotation at time `t` is the
/// normalised blend of its end rotations: for a quarter turn clockwise,
/// `(1 - t, -t)` normalised. So at the fraction reported that corner is
/// `slop` above the slab, give or take a quarter of it, and at half that
/// fraction it is well clear. Naming the shapes the other way round is the
/// same meeting with the normal reversed. Turning the other way the hand
/// swings up and away, and pinned 6 above the origin it reaches down only
/// to 2: neither meets the slab.
#[wasm_bindgen_test(unsupported = test)]
fn a_box_that_only_turns_reaches_another_by_turning() {
    let slab = boxed(0.0, 0.0, 6.0, 1.0);
    let hand = boxed(0.0, 0.0, 2.0, 0.1);
    let pinned = |height: f32, sine: f32| Sweep {
        local_center: v(-2.0, 0.0),
        c1: v(0.0, height),
        c2: v(0.0, height),
        q1: Rot::IDENTITY,
        q2: Rot { c: 0.0, s: sine },
    };
    let gap_at = |t: f32| {
        let blend = length(v(1.0 - t, t));
        let (c, s) = ((1.0 - t) / blend, t / blend);
        2.5 - 4.0 * s - 0.1 * c - 1.0
    };
    let clockwise = pinned(2.5, -1.0);
    let toi = time_of_impact(&slab, &still(0.0, 0.0), &hand, &clockwise, 1.0, SLOP);
    assert_met(
        &toi,
        gap_at(toi.fraction),
        gap_at(0.5 * toi.fraction),
        v(0.0, 1.0),
    );

    let swapped = time_of_impact(&hand, &clockwise, &slab, &still(0.0, 0.0), 1.0, SLOP);
    assert_met(
        &swapped,
        gap_at(swapped.fraction),
        gap_at(0.5 * swapped.fraction),
        v(0.0, -1.0),
    );

    let away = time_of_impact(&slab, &still(0.0, 0.0), &hand, &pinned(2.5, 1.0), 1.0, SLOP);
    assert_eq!(away.state, ToiState::Separated);

    let high = time_of_impact(
        &slab,
        &still(0.0, 0.0),
        &hand,
        &pinned(6.0, -1.0),
        1.0,
        SLOP,
    );
    assert_eq!(high.state, ToiState::Separated);
}

/// Shapes that overlap at the start are reported as overlapped, at the
/// start. Oracle: a unit box centred `(0.5, 0.5)` shares area with one
/// centred at the origin, wherever it is going.
#[wasm_bindgen_test(unsupported = test)]
fn shapes_overlapping_at_the_start_are_reported_overlapped() {
    let unit = boxed(0.0, 0.0, 1.0, 1.0);
    let toi = time_of_impact(
        &unit,
        &still(0.0, 0.0),
        &unit,
        &travelling((0.5, 0.5), (8.0, 0.5)),
        1.0,
        SLOP,
    );
    assert_eq!(toi.state, ToiState::Overlapped);
    assert_eq!(toi.fraction.to_bits(), 0.0f32.to_bits());
}

// --- Properties -----------------------------------------------------------

/// A shape by its own definition, for oracles that use no kernel maths.
#[derive(Clone, Copy, Debug)]
enum Solid {
    Ball {
        center: Vec2,
        radius: f32,
    },
    Capsule {
        p: Vec2,
        q: Vec2,
        radius: f32,
    },
    /// A box of half-extents `half`, turned by the unit `(cos, sin)` about
    /// its centre.
    Box {
        center: Vec2,
        half: Vec2,
        turn: (f32, f32),
    },
}

impl Solid {
    fn proxy(&self) -> Proxy {
        match *self {
            Solid::Ball { center, radius } => Proxy::new(&[center], radius),
            Solid::Capsule { p, q, radius } => Proxy::new(&[p, q], radius),
            Solid::Box { center, half, turn } => Proxy::offset(
                &[
                    v(-half.x, -half.y),
                    v(half.x, -half.y),
                    v(half.x, half.y),
                    v(-half.x, half.y),
                ],
                0.0,
                center,
                Rot {
                    c: turn.0,
                    s: turn.1,
                },
            ),
        }
    }

    fn radius(&self) -> f32 {
        match *self {
            Solid::Ball { radius, .. } | Solid::Capsule { radius, .. } => radius,
            Solid::Box { .. } => 0.0,
        }
    }

    /// The point of the shape's core nearest `point`: the centre of a
    /// ball, the nearest point of a capsule's segment, and for a box the
    /// point clamped to the box in the box's own frame.
    fn nearest_core_point(&self, point: Vec2) -> Vec2 {
        match *self {
            Solid::Ball { center, .. } => center,
            Solid::Capsule { p, q, .. } => {
                let along = q - p;
                let t = ((point - p).dot(along) / along.dot(along)).clamp(0.0, 1.0);
                p + along * t
            }
            Solid::Box { center, half, turn } => {
                let (c, s) = turn;
                let rel = point - center;
                // Into the box's frame by the inverse turn, clamp, and back.
                let local = v(c * rel.x + s * rel.y, c * rel.y - s * rel.x);
                let held = v(
                    local.x.clamp(-half.x, half.x),
                    local.y.clamp(-half.y, half.y),
                );
                center + v(c * held.x - s * held.y, s * held.x + c * held.y)
            }
        }
    }

    /// How far `point` is from the shape's core.
    fn core_distance(&self, point: Vec2) -> f32 {
        length(point - self.nearest_core_point(point))
    }

    /// A box's own two axes and its four corners, where it stands.
    fn box_frame(&self) -> ([Vec2; 2], [Vec2; 4]) {
        let Solid::Box { center, half, turn } = *self else {
            panic!("{self:?} is not a box");
        };
        let axes = [v(turn.0, turn.1), v(-turn.1, turn.0)];
        let corner = |x: f32, y: f32| center + axes[0] * (x * half.x) + axes[1] * (y * half.y);
        (
            axes,
            [
                corner(-1.0, -1.0),
                corner(1.0, -1.0),
                corner(1.0, 1.0),
                corner(-1.0, 1.0),
            ],
        )
    }
}

/// How far apart two boxes are along the best of their face normals:
/// positive when one of those axes separates them, and by the separating
/// axis theorem negative only when the boxes overlap.
fn separation_along_face_normals(a: &Solid, b: &Solid) -> f32 {
    let (axes_a, corners_a) = a.box_frame();
    let (axes_b, corners_b) = b.box_frame();
    let span = |corners: &[Vec2; 4], axis: Vec2| {
        corners
            .iter()
            .map(|corner| corner.dot(axis))
            .fold((f32::MAX, f32::MIN), |(low, high), along| {
                (low.min(along), high.max(along))
            })
    };
    axes_a
        .into_iter()
        .chain(axes_b)
        .map(|axis| {
            let (low_a, high_a) = span(&corners_a, axis);
            let (low_b, high_b) = span(&corners_b, axis);
            (low_b - high_a).max(low_a - high_b)
        })
        .fold(f32::MIN, f32::max)
}

/// A unit `(cos, sin)` from the sequence, without a trigonometric function.
fn random_turn(sequence: &mut Sequence) -> (f32, f32) {
    let x = sequence.range(-1.0, 1.0);
    let y = sequence.range(-1.0, 1.0);
    let norm = length(v(x, y));
    if norm < 0.1 {
        (1.0, 0.0)
    } else {
        (x / norm, y / norm)
    }
}

fn random_ball(sequence: &mut Sequence, reach: f32) -> Solid {
    Solid::Ball {
        center: v(sequence.range(-reach, reach), sequence.range(-reach, reach)),
        radius: sequence.range(0.2, 1.0),
    }
}

fn random_box(sequence: &mut Sequence, reach: f32) -> Solid {
    Solid::Box {
        center: v(sequence.range(-reach, reach), sequence.range(-reach, reach)),
        half: v(sequence.range(0.3, 1.5), sequence.range(0.3, 1.5)),
        turn: random_turn(sequence),
    }
}

fn random_solid(sequence: &mut Sequence, reach: f32) -> Solid {
    let pick = sequence.unit();
    if pick < 0.3 {
        random_ball(sequence, reach)
    } else if pick < 0.6 {
        let p = v(sequence.range(-reach, reach), sequence.range(-reach, reach));
        let (c, s) = random_turn(sequence);
        let span = sequence.range(0.5, 2.0);
        Solid::Capsule {
            p,
            q: p + v(c * span, s * span),
            radius: sequence.range(0.2, 0.8),
        }
    } else {
        random_box(sequence, reach)
    }
}

/// Swapping the two shapes negates the normal and swaps the points, and
/// the closest points lie on the shapes. Oracles: the distance between two
/// sets does not depend on which is named first; and each shape's own
/// definition says how far a point is from its core, which for a point on
/// its surface is its radius. 64 fixed pairs.
#[wasm_bindgen_test(unsupported = test)]
fn distance_is_symmetric_and_its_points_lie_on_the_shapes() {
    let mut sequence = Sequence::new(0x5eed_0001);
    let mut separated = 0;
    for _ in 0..64 {
        let a = random_solid(&mut sequence, 4.0);
        let b = random_solid(&mut sequence, 4.0);
        let forward = distance(&a.proxy(), &b.proxy(), true);
        let backward = distance(&b.proxy(), &a.proxy(), true);
        assert_ne!(forward.state, DistanceState::IterationCap);
        assert_eq!(forward.state, backward.state, "{a:?} {b:?}");
        assert_near(forward.distance, backward.distance, 1e-4);
        if forward.state != DistanceState::Separated || forward.distance == 0.0 {
            continue;
        }
        separated += 1;

        assert_vec_near(forward.normal, -backward.normal, 1e-3);
        assert_vec_near(forward.point_a, backward.point_b, 1e-3);
        assert_vec_near(forward.point_b, backward.point_a, 1e-3);

        // On the surfaces: a radius from each core.
        assert_near(a.core_distance(forward.point_a), a.radius(), 1e-4);
        assert_near(b.core_distance(forward.point_b), b.radius(), 1e-4);
        // The distance is the distance between the points, along the
        // normal from A to B.
        assert_vec_near(
            forward.point_b - forward.point_a,
            forward.normal * forward.distance,
            1e-4,
        );

        // Without the radii the points are on the cores themselves.
        let cores = distance(&a.proxy(), &b.proxy(), false);
        assert_near(a.core_distance(cores.point_a), 0.0, 1e-4);
        assert_near(b.core_distance(cores.point_b), 0.0, 1e-4);
    }
    assert!(separated >= 24, "only {separated} pairs were apart");
}

/// The distance from a turned box to a ball, a capsule or another turned
/// box is what the shapes' definitions give. Oracles: a ball is as far
/// from a box as its centre is, less its radius. A capsule is as far as
/// the nearest point of its segment, found here by sampling the segment
/// 2001 times (the box is convex, so the sampled minimum is within a
/// sample's spacing of the true one). Two boxes a face normal separates
/// are as far apart as the nearest corner of either is from the other (in
/// the plane the closest pair of two polygons always includes a corner),
/// and two that no face normal separates overlap. 96 fixed pairs.
#[wasm_bindgen_test(unsupported = test)]
fn distance_matches_the_shapes_own_definitions() {
    let mut sequence = Sequence::new(0x5eed_0002);
    let mut boxes_apart = 0;
    for _ in 0..96 {
        let block = random_box(&mut sequence, 3.0);
        let other = random_solid(&mut sequence, 4.0);
        let found = distance(&block.proxy(), &other.proxy(), true);

        let expected = match other {
            Solid::Ball { center, radius } => block.core_distance(center) - radius,
            Solid::Capsule { p, q, radius } => {
                let nearest = (0u16..=2000)
                    .map(|i| block.core_distance(p + (q - p) * (f32::from(i) / 2000.0)))
                    .fold(f32::MAX, f32::min);
                nearest - radius
            }
            Solid::Box { .. } => {
                let separation = separation_along_face_normals(&block, &other);
                if separation < -2e-3 {
                    assert_eq!(
                        found.state,
                        DistanceState::Overlapped,
                        "{block:?} {other:?}"
                    );
                    continue;
                }
                if separation < 2e-3 {
                    continue;
                }
                boxes_apart += 1;
                let nearest_corner = |of: &Solid, to: &Solid| {
                    of.box_frame()
                        .1
                        .into_iter()
                        .map(|corner| to.core_distance(corner))
                        .fold(f32::MAX, f32::min)
                };
                nearest_corner(&block, &other).min(nearest_corner(&other, &block))
            }
        };

        if expected > 2e-3 {
            assert_eq!(found.state, DistanceState::Separated);
            assert_near(found.distance, expected, 2e-3);
        } else if expected < -2e-3 {
            assert_eq!(found.distance.to_bits(), 0.0f32.to_bits());
        }
    }
    assert!(
        boxes_apart >= 8,
        "only {boxes_apart} pairs of boxes were apart"
    );
}

/// At a cast's hit the shapes are the target distance apart, before it
/// they are further, and a cast that misses never brings them that close.
/// Oracle: a ball's distance to a turned box or to another ball, from the
/// shapes' own definitions, evaluated along the motion with no kernel
/// maths. The target is the cast's contract: `max(slop, radii - slop)`
/// between the cores, reached within a quarter of `slop`. 96 fixed casts.
#[wasm_bindgen_test(unsupported = test)]
fn a_cast_stops_at_the_target_distance_and_not_before() {
    let mut sequence = Sequence::new(0x5eed_0003);
    let (mut hits, mut misses) = (0, 0);
    for _ in 0..96 {
        let fixed = if sequence.unit() < 0.7 {
            random_box(&mut sequence, 2.0)
        } else {
            random_ball(&mut sequence, 2.0)
        };
        let Solid::Ball { center, radius } = random_ball(&mut sequence, 6.0) else {
            unreachable!("random_ball makes a ball");
        };
        // Aimed near the fixed shape, and from half as far as it to half
        // as far again, so hits, near misses and motions that stop short
        // all occur.
        let aim = v(sequence.range(-2.5, 2.5), sequence.range(-2.5, 2.5));
        let translation = (aim - center) * sequence.range(0.5, 1.5);

        let total_radius = fixed.radius() + radius;
        let target = (total_radius - SLOP).max(SLOP);
        let tolerance = 0.25 * SLOP;
        let core_distance_at = |fraction: f32| fixed.core_distance(center + translation * fraction);

        let cast = shape_cast(
            &fixed.proxy(),
            &ball(center.x, center.y, radius),
            translation,
            1.0,
            false,
            SLOP,
        );
        match cast.state {
            CastState::Hit => {
                hits += 1;
                let at_hit = core_distance_at(cast.fraction);
                assert!(
                    at_hit >= target - 1e-4 && at_hit <= target + tolerance + 1e-4,
                    "{at_hit} apart at the hit, target {target}"
                );
                for step in 0u8..8 {
                    let before = core_distance_at(cast.fraction * f32::from(step) / 8.0);
                    assert!(before >= target - 1e-4, "{before} apart before the hit");
                }
                // The normal runs from the fixed shape's nearest point to
                // the ball's centre.
                let ball_at_hit = center + translation * cast.fraction;
                let outward = ball_at_hit - fixed.nearest_core_point(ball_at_hit);
                assert_vec_near(cast.normal, outward * (1.0 / length(outward)), 1e-3);
            }
            CastState::Miss => {
                misses += 1;
                for step in 0u8..=128 {
                    let along = core_distance_at(f32::from(step) / 128.0);
                    assert!(along >= target - 1e-4, "{along} apart on a miss");
                }
            }
            CastState::InitialOverlap => {
                assert!(core_distance_at(0.0) < target + tolerance + 1e-4);
            }
            CastState::Failed => panic!("a cast of well-formed shapes failed"),
        }
    }
    assert!(hits >= 16 && misses >= 16, "{hits} hits, {misses} misses");
}

/// Results do not depend on where a pair is. Both proxies moved by the
/// same offset give the same bits for the distance and the normal, the
/// same closest core points moved by that offset, and for a cast the same
/// fraction and normal.
///
/// Oracle: the shapes sit on a grid of halves, with boxes whose sides are
/// powers of two, and the offsets are powers of two, so every sum and
/// product the kernel forms from a position is exact wherever the pair
/// stands: equality is the correct answer, not a tolerance. A cast is
/// compared only where it advances along an axis, by a multiple of the
/// slop (a sixteenth), so the positions it advances to are on the grid too;
/// a diagonal cast advances to positions that round to the floats where the
/// pair stands, which is why the crate hands the kernel body-relative
/// shapes. A point on a round surface is not compared for the same reason.
#[wasm_bindgen_test(unsupported = test)]
fn results_are_bit_identical_when_both_shapes_move_together() {
    type Pair = (fn(Vec2) -> Proxy, fn(Vec2) -> Proxy, Option<Vec2>);
    let pairs: [Pair; 5] = [
        // Ball against ball, 3-4-5 apart.
        (
            |o| ball(o.x, o.y, 1.0),
            |o| ball(o.x + 3.0, o.y + 4.0, 0.5),
            None,
        ),
        // Ball over a box's face.
        (
            |o| boxed(o.x, o.y, 2.0, 1.0),
            |o| ball(o.x + 0.5, o.y + 6.0, 0.5),
            Some(v(0.0, -8.0)),
        ),
        // Box against box, corner to corner.
        (
            |o| boxed(o.x, o.y, 1.0, 1.0),
            |o| boxed(o.x + 5.0, o.y + 6.0, 1.0, 1.0),
            None,
        ),
        // Box against box, face to face.
        (
            |o| boxed(o.x, o.y, 1.0, 1.0),
            |o| boxed(o.x + 6.0, o.y + 0.5, 1.0, 1.0),
            Some(v(-8.0, 0.0)),
        ),
        // Capsule over a box's face.
        (
            |o| boxed(o.x, o.y, 2.0, 1.0),
            |o| capsule((o.x - 1.0, o.y + 5.0), (o.x + 1.0, o.y + 5.0), 0.5),
            Some(v(0.0, -8.0)),
        ),
    ];
    let slop = 0.0625;

    for (make_a, make_b, translation) in pairs {
        let (home_a, home_b) = (make_a(Vec2::ZERO), make_b(Vec2::ZERO));
        let home_distance = distance(&home_a, &home_b, true);
        let home_cores = distance(&home_a, &home_b, false);
        assert_eq!(home_distance.state, DistanceState::Separated);

        for offset in [v(1024.0, -2048.0), v(-4096.0, 512.0), v(8.0, 16.0)] {
            let (away_a, away_b) = (make_a(offset), make_b(offset));

            let away_distance = distance(&away_a, &away_b, true);
            assert_eq!(away_distance.state, home_distance.state);
            assert_eq!(
                away_distance.distance.to_bits(),
                home_distance.distance.to_bits()
            );
            assert_eq!(away_distance.normal, home_distance.normal);
            assert_eq!(away_distance.iterations, home_distance.iterations);

            let away_cores = distance(&away_a, &away_b, false);
            assert_eq!(away_cores.distance.to_bits(), home_cores.distance.to_bits());
            assert_eq!(away_cores.point_a - offset, home_cores.point_a);
            assert_eq!(away_cores.point_b - offset, home_cores.point_b);

            let Some(translation) = translation else {
                continue;
            };
            let home_cast = shape_cast(&home_a, &home_b, translation, 1.0, false, slop);
            let away_cast = shape_cast(&away_a, &away_b, translation, 1.0, false, slop);
            assert_eq!(home_cast.state, CastState::Hit);
            assert_eq!(away_cast.state, CastState::Hit);
            assert_eq!(away_cast.fraction.to_bits(), home_cast.fraction.to_bits());
            assert_eq!(away_cast.normal, home_cast.normal);
            assert_eq!(away_cast.iterations, home_cast.iterations);
        }
    }
}
