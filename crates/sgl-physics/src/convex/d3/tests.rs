//! Tests of the 3D kernel. Every expected value comes from geometry worked
//! in the test, or from a shape's own definition evaluated without the
//! kernel: never from the kernel's arithmetic.
//!
//! The shape cast itself is the code both dimensions share, so its slop,
//! its encroachment and its cap are tested once, in 2D.

use core::f32::consts::FRAC_1_SQRT_2;

use sgl_core::math::Vec3;
use wasm_bindgen_test::wasm_bindgen_test;

use super::*;
use crate::convex::CastState;
use crate::convex::testing::{Sequence, assert_contact_fraction, assert_near};

/// The slop the cast tests pass: a hundredth of a unit for shapes a few
/// units across.
const SLOP: f32 = 0.01;

/// Rounding allowance for shapes a few units across.
const EPS: f32 = 1e-5;

fn v(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z)
}

fn ball(center: Vec3, radius: f32) -> Proxy {
    Proxy::new(&[center], radius)
}

fn capsule(p: Vec3, q: Vec3, radius: f32) -> Proxy {
    Proxy::new(&[p, q], radius)
}

/// The eight corners of a box of half-extents `half` about the origin.
fn corners(half: Vec3) -> [Vec3; 8] {
    let mut out = [Vec3::ZERO; 8];
    for (index, corner) in out.iter_mut().enumerate() {
        let side = |bit: usize| if index & bit == 0 { -1.0 } else { 1.0 };
        *corner = v(side(1) * half.x, side(2) * half.y, side(4) * half.z);
    }
    out
}

/// An axis-aligned box by its centre and half-extents.
fn boxed(center: Vec3, half: Vec3) -> Proxy {
    Proxy::new(&corners(half).map(|corner| center + corner), 0.0)
}

/// The box most tests use: `[-2, 2] x [-1, 1] x [-3, 3]`.
fn block() -> Proxy {
    boxed(Vec3::ZERO, v(2.0, 1.0, 3.0))
}

/// The unit direction `(2, 3, 6) / 7`: `2² + 3² + 6² = 7²`.
fn n236() -> Vec3 {
    v(2.0 / 7.0, 3.0 / 7.0, 6.0 / 7.0)
}

fn length(a: Vec3) -> f32 {
    (a.x * a.x + a.y * a.y + a.z * a.z).sqrt()
}

#[track_caller]
fn assert_vec_near(actual: Vec3, expected: Vec3, tolerance: f32) {
    assert!(
        length(actual - expected) <= tolerance,
        "{actual} is not within {tolerance} of {expected}"
    );
}

/// Asserts a separated result with the given distance, normal and points.
#[track_caller]
fn assert_separated(
    found: &ClosestPoints<Vec3>,
    distance: f32,
    normal: Vec3,
    point_a: Vec3,
    point_b: Vec3,
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
fn assert_hit(cast: &Cast<Vec3>, touch: f32, closing_speed: f32, total_radius: f32, normal: Vec3) {
    assert_eq!(cast.state, CastState::Hit);
    assert_contact_fraction(cast.fraction, touch, closing_speed, total_radius, SLOP);
    assert_vec_near(cast.normal, normal, 1e-4);
}

fn cast(a: &Proxy, b: &Proxy, translation: Vec3) -> Cast<Vec3> {
    shape_cast(a, b, translation, 1.0, false, SLOP)
}

/// A rotation given as a matrix or as a quaternion turns points the way
/// the right-hand rule says, and a proxy built under it holds each point
/// turned and then moved. Oracle: a quarter turn about z sends x to y and
/// y to -x and leaves z, so `(1, 0, 0)` and `(0, 2, 5)` land on `(0, 1, 0)`
/// and `(-2, 0, 5)`; a quarter turn about x sends y to z. The quaternion of
/// a quarter turn about an axis is that axis times `sin 45°` with
/// `cos 45°`.
#[wasm_bindgen_test(unsupported = test)]
fn an_offset_proxy_turns_its_points_and_then_moves_them() {
    let points = [v(1.0, 0.0, 0.0), v(0.0, 2.0, 5.0)];
    let position = v(10.0, 20.0, 30.0);

    let about_z = Rot {
        cx: Vec3::Y,
        cy: Vec3::NEG_X,
        cz: Vec3::Z,
    };
    let by_matrix = Proxy::offset(&points, 0.25, position, &about_z);
    assert_eq!(
        by_matrix.points(),
        &[v(10.0, 21.0, 30.0), v(8.0, 20.0, 35.0)]
    );
    assert_eq!(by_matrix.radius().to_bits(), 0.25f32.to_bits());

    let by_quat = Proxy::offset(
        &points,
        0.0,
        position,
        &Rot::from_quat(0.0, 0.0, FRAC_1_SQRT_2, FRAC_1_SQRT_2),
    );
    assert_vec_near(by_quat.points()[0], v(10.0, 21.0, 30.0), EPS);
    assert_vec_near(by_quat.points()[1], v(8.0, 20.0, 35.0), EPS);

    let about_x = Rot::from_quat(FRAC_1_SQRT_2, 0.0, 0.0, FRAC_1_SQRT_2);
    let turned = Proxy::offset(&[v(0.0, 1.0, 0.0)], 0.0, Vec3::ZERO, &about_x);
    assert_vec_near(turned.points()[0], v(0.0, 0.0, 1.0), EPS);

    let unturned = Proxy::offset(&points, 0.0, position, &Rot::IDENTITY);
    assert_eq!(
        unturned.points(),
        &[v(11.0, 20.0, 30.0), v(10.0, 22.0, 35.0)]
    );
}

/// A proxy keeps at most `MAX_POINTS` points, the first ones. Oracle: the
/// capacity the crate states for a 3D hull, sixteen points (#919).
#[wasm_bindgen_test(unsupported = test)]
fn a_proxy_keeps_its_first_sixteen_points() {
    let points: Vec<Vec3> = (0u8..20).map(|i| v(f32::from(i), 0.0, 0.0)).collect();
    let proxy = Proxy::new(&points, 0.0);
    assert_eq!(proxy.points(), &points[..16]);
}

/// Two balls are apart by the distance between their centres less both
/// radii, along the line of centres. Oracle: centres `(0, 0, 0)` and
/// `(2, 3, 6)` are 7 apart, radii 1 and 2 leave 4, the normal is
/// `(2, 3, 6) / 7` and each point is its centre moved a radius along it.
#[wasm_bindgen_test(unsupported = test)]
fn balls_are_apart_by_their_centres_less_their_radii() {
    let a = ball(Vec3::ZERO, 1.0);
    let b = ball(v(2.0, 3.0, 6.0), 2.0);
    assert_separated(
        &distance(&a, &b, true),
        4.0,
        n236(),
        n236(),
        v(2.0, 3.0, 6.0) - n236() * 2.0,
    );
    assert_separated(
        &distance(&a, &b, false),
        7.0,
        n236(),
        Vec3::ZERO,
        v(2.0, 3.0, 6.0),
    );
}

/// A ball is apart from a box by its distance to the box's nearest face,
/// edge or vertex, less its radius. Oracle: the box spans
/// `[-2, 2] x [-1, 1] x [-3, 3]`. A ball of radius 0.5 centred
/// `(0.5, 4, -1)` is 3 above the top face: 2.5 apart. A ball of radius 1
/// centred `(5, 5, 0.5)` is `(3, 4, 0)` from the edge `x = 2, y = 1`: 5
/// away, 4 apart along `(0.6, 0.8, 0)`. A ball of radius 1 centred
/// `(4, 4, 9)` is `(2, 3, 6)` from the vertex `(2, 1, 3)`: 7 away, 6 apart.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_meets_a_box_at_its_face_its_edge_and_its_vertex() {
    assert_separated(
        &distance(&block(), &ball(v(0.5, 4.0, -1.0), 0.5), true),
        2.5,
        v(0.0, 1.0, 0.0),
        v(0.5, 1.0, -1.0),
        v(0.5, 3.5, -1.0),
    );

    assert_separated(
        &distance(&block(), &ball(v(5.0, 5.0, 0.5), 1.0), true),
        4.0,
        v(0.6, 0.8, 0.0),
        v(2.0, 1.0, 0.5),
        v(4.4, 4.2, 0.5),
    );

    assert_separated(
        &distance(&block(), &ball(v(4.0, 4.0, 9.0), 1.0), true),
        6.0,
        n236(),
        v(2.0, 1.0, 3.0),
        v(4.0, 4.0, 9.0) - n236(),
    );
}

/// A capsule is its segment grown by its radius. Oracle: a capsule of
/// radius 0.5 lying along `y = 4` over the box's top face is 3 above it:
/// 2.5 apart along `(0, 1, 0)`, every point of the segment equally near.
/// One from `(4, 4, 9)` to `(6, 7, 15)` of radius 1 has its near end
/// `(2, 3, 6)` from the vertex `(2, 1, 3)`: 7 away, 6 apart. Two capsules
/// of radius 0.25 crossing at right angles, one along x through the origin
/// and one along y at height 3, are nearest where they cross: 3 between
/// the segments, 2.5 apart along `(0, 0, 1)`.
#[wasm_bindgen_test(unsupported = test)]
fn a_capsule_meets_a_box_and_another_capsule() {
    let along = distance(
        &block(),
        &capsule(v(-1.0, 4.0, 0.0), v(1.0, 4.0, 0.0), 0.5),
        true,
    );
    assert_eq!(along.state, DistanceState::Separated);
    assert_near(along.distance, 2.5, EPS);
    assert_vec_near(along.normal, v(0.0, 1.0, 0.0), EPS);
    assert_near(along.point_a.y, 1.0, EPS);
    assert_near(along.point_b.y, 3.5, EPS);
    assert_near(along.point_a.x, along.point_b.x, EPS);
    assert!((-1.0..=1.0).contains(&along.point_a.x));
    assert_near(along.point_a.z, 0.0, EPS);

    assert_separated(
        &distance(
            &block(),
            &capsule(v(4.0, 4.0, 9.0), v(6.0, 7.0, 15.0), 1.0),
            true,
        ),
        6.0,
        n236(),
        v(2.0, 1.0, 3.0),
        v(4.0, 4.0, 9.0) - n236(),
    );

    assert_separated(
        &distance(
            &capsule(v(-2.0, 0.0, 0.0), v(2.0, 0.0, 0.0), 0.25),
            &capsule(v(0.0, -2.0, 3.0), v(0.0, 2.0, 3.0), 0.25),
            true,
        ),
        2.5,
        v(0.0, 0.0, 1.0),
        v(0.0, 0.0, 0.25),
        v(0.0, 0.0, 2.75),
    );
}

/// Two boxes side by side are apart by the gap between their faces, and
/// two boxes vertex to vertex by the distance between the vertices.
/// Oracle: unit cubes centred at the origin and `(4, 0.5, -0.5)` have
/// faces at `x = 1` and `x = 3`, 2 apart along `(1, 0, 0)`, facing each
/// other for `y` in `[-0.5, 1]` and `z` in `[-1, 0.5]`. Centred at the
/// origin and `(4, 5, 8)` their nearest vertices are `(1, 1, 1)` and
/// `(3, 4, 7)`: `(2, 3, 6)` apart, so 7.
#[wasm_bindgen_test(unsupported = test)]
fn boxes_are_apart_across_a_face_and_across_a_vertex() {
    let a = boxed(Vec3::ZERO, Vec3::ONE);

    let face = distance(&a, &boxed(v(4.0, 0.5, -0.5), Vec3::ONE), true);
    assert_eq!(face.state, DistanceState::Separated);
    assert_near(face.distance, 2.0, EPS);
    assert_vec_near(face.normal, v(1.0, 0.0, 0.0), EPS);
    assert_near(face.point_a.x, 1.0, EPS);
    assert_near(face.point_b.x, 3.0, EPS);
    assert_near(face.point_a.y, face.point_b.y, EPS);
    assert_near(face.point_a.z, face.point_b.z, EPS);
    assert!((-0.5..=1.0).contains(&face.point_a.y));
    assert!((-1.0..=0.5).contains(&face.point_a.z));

    assert_separated(
        &distance(&a, &boxed(v(4.0, 5.0, 8.0), Vec3::ONE), true),
        7.0,
        n236(),
        v(1.0, 1.0, 1.0),
        v(3.0, 4.0, 7.0),
    );
}

/// Two boxes can be nearest along a pair of crossing edges, which only a
/// turned box brings about. Oracle: the unit cube's edge `x = 1, y = 1`
/// runs along z. With `n = (0.6, 0.8, 0)`, a second unit cube is turned so
/// its x axis lies along `u = (-0.8, 0.6, 0)` and its other two axes along
/// `(n - z) / sqrt(2)` and `-(n + z) / sqrt(2)`: one of its edges then runs
/// along `u`, at `sqrt(2)` from its centre toward `-n`. Centred
/// `(1, 1, 0) + (2 + sqrt(2)) n`, that edge passes through
/// `(1, 1, 0) + 2 n = (2.2, 2.6, 0)`, and `n` is perpendicular to both
/// edges: they are 2 apart at `(1, 1, 0)` and `(2.2, 2.6, 0)`. (Along `n`
/// the first cube reaches 1.4 and the second, whose reach along `n` is
/// `sqrt(2)`, comes down to 3.4: the same 2.)
#[wasm_bindgen_test(unsupported = test)]
fn turned_boxes_are_apart_across_crossing_edges() {
    let n = v(0.6, 0.8, 0.0);
    let turn = Rot {
        cx: v(-0.8, 0.6, 0.0),
        cy: v(0.6, 0.8, -1.0) * FRAC_1_SQRT_2,
        cz: v(-0.6, -0.8, -1.0) * FRAC_1_SQRT_2,
    };
    let center = v(1.0, 1.0, 0.0) + n * (2.0 + 2.0f32.sqrt());
    let turned = Proxy::offset(&corners(Vec3::ONE), 0.0, center, &turn);

    let found = distance(&boxed(Vec3::ZERO, Vec3::ONE), &turned, true);
    assert_eq!(found.state, DistanceState::Separated);
    assert_near(found.distance, 2.0, 1e-4);
    assert_vec_near(found.normal, n, 1e-4);
    assert_vec_near(found.point_a, v(1.0, 1.0, 0.0), 1e-4);
    assert_vec_near(found.point_b, v(2.2, 2.6, 0.0), 1e-4);
}

/// A box turned an eighth of a turn about z presents its upright edges
/// along the x and y axes and its faces along the diagonals. Oracle: a
/// cube of half-extent 1 turned 45 degrees about z (the quaternion
/// `(0, 0, sin 22.5°, cos 22.5°)`) has upright edges at distance sqrt(2)
/// on the axes and faces at distance 1 along the diagonals. A ball of
/// radius 0.5 centred `(4, 0, 0.3)` is `4 - sqrt(2) - 0.5` from the edge;
/// one centred 3 along the diagonal is `3 - 1 - 0.5 = 1.5` from the face.
#[wasm_bindgen_test(unsupported = test)]
fn a_turned_box_is_measured_where_its_rotation_puts_it() {
    let eighth_turn = Rot::from_quat(0.0, 0.0, 0.382_683_43, 0.923_879_5);
    let turned = Proxy::offset(&corners(Vec3::ONE), 0.0, Vec3::ZERO, &eighth_turn);
    let sqrt2 = 2.0f32.sqrt();

    assert_separated(
        &distance(&turned, &ball(v(4.0, 0.0, 0.3), 0.5), true),
        4.0 - sqrt2 - 0.5,
        v(1.0, 0.0, 0.0),
        v(sqrt2, 0.0, 0.3),
        v(3.5, 0.0, 0.3),
    );

    let diagonal = v(FRAC_1_SQRT_2, FRAC_1_SQRT_2, 0.0);
    let lift = v(0.0, 0.0, 0.3);
    assert_separated(
        &distance(&turned, &ball(diagonal * 3.0 + lift, 0.5), true),
        1.5,
        diagonal,
        diagonal + lift,
        diagonal * 2.5 + lift,
    );
}

/// A hull's slanted face is met along that face's normal: the slope of any
/// ramp. Oracle: the prism over the triangle `(0, 0)`, `(4, 0)`, `(0, 3)`,
/// for `z` in `[-1, 1]`, has a slanted face with outward normal
/// `(0.6, 0.8, 0)` (3-4-5) through `(2, 1.5, z)`. A ball of radius 0.5
/// centred 2 along the normal from `(2, 1.5, 0.25)`, at `(3.2, 3.1, 0.25)`,
/// is 1.5 from the face.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_meets_a_slanted_face_along_its_normal() {
    let ramp = Proxy::new(
        &[
            v(0.0, 0.0, -1.0),
            v(4.0, 0.0, -1.0),
            v(0.0, 3.0, -1.0),
            v(0.0, 0.0, 1.0),
            v(4.0, 0.0, 1.0),
            v(0.0, 3.0, 1.0),
        ],
        0.0,
    );
    assert_separated(
        &distance(&ramp, &ball(v(3.2, 3.1, 0.25), 0.5), true),
        1.5,
        v(0.6, 0.8, 0.0),
        v(2.0, 1.5, 0.25),
        v(2.9, 2.7, 0.25),
    );
}

/// Cores that overlap report zero distance and say so, and shapes whose
/// radii overlap while their cores do not report zero distance but still a
/// normal. Oracle: a ball's centre inside a box, and two boxes sharing
/// volume, have overlapping cores, and the one point reported lies in
/// both: at the ball's centre, or in the box
/// `[0.5, 2] x [-0.5, 1] x [1.5, 3]` that `[-2, 2] x [-1, 1] x [-3, 3]`
/// and the unit cube about `(1.5, 0.5, 2.5)` share. Balls of radius 1
/// centred 1.5 apart overlap by 0.5, but their centres are distinct points
/// 1.5 apart along `(1, 0, 0)`.
#[wasm_bindgen_test(unsupported = test)]
fn overlapping_cores_report_zero_distance_and_say_so() {
    // Each shape with the corners of the region its core shares with the
    // box's.
    for (inside, low, high) in [
        (
            ball(v(0.5, 0.25, -1.0), 0.5),
            v(0.5, 0.25, -1.0),
            v(0.5, 0.25, -1.0),
        ),
        (
            ball(v(-1.9, 0.9, 2.9), 0.0),
            v(-1.9, 0.9, 2.9),
            v(-1.9, 0.9, 2.9),
        ),
        (
            boxed(v(1.5, 0.5, 2.5), Vec3::ONE),
            v(0.5, -0.5, 1.5),
            v(2.0, 1.0, 3.0),
        ),
    ] {
        for use_radii in [false, true] {
            let found = distance(&block(), &inside, use_radii);
            assert_eq!(found.state, DistanceState::Overlapped);
            assert_eq!(found.distance.to_bits(), 0.0f32.to_bits());
            assert_eq!(found.normal, Vec3::ZERO);
            // One point, shared by both cores.
            assert_eq!(found.point_a, found.point_b);
            let shared = found.point_a;
            assert!(
                (low - shared).max_element() <= EPS && (shared - high).max_element() <= EPS,
                "{shared} is not in both cores"
            );
        }
    }

    let a = ball(Vec3::ZERO, 1.0);
    let b = ball(v(1.5, 0.0, 0.0), 1.0);
    let rounded = distance(&a, &b, true);
    assert_eq!(rounded.state, DistanceState::Separated);
    assert_eq!(rounded.distance.to_bits(), 0.0f32.to_bits());
    assert_vec_near(rounded.normal, v(1.0, 0.0, 0.0), EPS);
    let cores = distance(&a, &b, false);
    assert_near(cores.distance, 1.5, EPS);
}

/// A ball cast at a ball meets it when their centres are the sum of the
/// radii apart. Oracle: radius-1 balls with centres 14 apart along
/// `(2, 3, 6)`, the mover travelling 14 straight at the other, touch after
/// 12: fraction `12 / 14`, the normal back along the motion and the point
/// on the fixed ball's surface.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_cast_at_a_ball_meets_it_a_radius_sum_away() {
    let met = cast(
        &ball(Vec3::ZERO, 1.0),
        &ball(v(4.0, 6.0, 12.0), 1.0),
        v(-4.0, -6.0, -12.0),
    );
    assert_hit(&met, 12.0 / 14.0, 14.0, 2.0, n236());
    assert_vec_near(met.point, n236(), 1e-4);
}

/// A ball cast at a box meets its face, its edge or its vertex when its
/// centre is a radius from it. Oracle: the box spans
/// `[-2, 2] x [-1, 1] x [-3, 3]`. A ball of radius 0.5 from `(0.5, 6, -1)`
/// moving 10 down touches the top at centre height 1.5, after 4.5:
/// fraction 0.45. A ball of radius 1 from `(8, 9, 0.5)`, 10 from the edge
/// `x = 2, y = 1` along `(0.6, 0.8, 0)`, moving 20 straight at it touches
/// after 9: fraction 0.45. A ball of radius 1 from `(6, 7, 15)`, 14 from
/// the vertex `(2, 1, 3)` along `(2, 3, 6)`, moving 28 straight at it
/// touches after 13: fraction `13 / 28`.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_cast_at_a_box_meets_its_face_its_edge_and_its_vertex() {
    let face = cast(&block(), &ball(v(0.5, 6.0, -1.0), 0.5), v(0.0, -10.0, 0.0));
    assert_hit(&face, 0.45, 10.0, 0.5, v(0.0, 1.0, 0.0));
    assert_vec_near(face.point, v(0.5, 1.0, -1.0), 1e-4);

    let edge = cast(&block(), &ball(v(8.0, 9.0, 0.5), 1.0), v(-12.0, -16.0, 0.0));
    assert_hit(&edge, 0.45, 20.0, 1.0, v(0.6, 0.8, 0.0));
    assert_vec_near(edge.point, v(2.0, 1.0, 0.5), 1e-4);

    let vertex = cast(
        &block(),
        &ball(v(6.0, 7.0, 15.0), 1.0),
        v(-8.0, -12.0, -24.0),
    );
    assert_hit(&vertex, 13.0 / 28.0, 28.0, 1.0, n236());
    assert_vec_near(vertex.point, v(2.0, 1.0, 3.0), 1e-4);
}

/// A capsule cast flat onto a box lands when its segment is a radius above
/// the face. Oracle: a capsule of radius 0.5 along `y = 5` moving 8 down
/// onto the top face `y = 1` touches at segment height 1.5, after 3.5:
/// fraction 0.4375, on the face under the segment.
#[wasm_bindgen_test(unsupported = test)]
fn a_capsule_cast_at_a_box_lands_a_radius_above_its_face() {
    let landed = cast(
        &block(),
        &capsule(v(-1.0, 5.0, 0.0), v(1.0, 5.0, 0.0), 0.5),
        v(0.0, -8.0, 0.0),
    );
    assert_hit(&landed, 0.4375, 8.0, 0.5, v(0.0, 1.0, 0.0));
    assert_near(landed.point.y, 1.0, 1e-4);
    assert!((-1.0..=1.0).contains(&landed.point.x));
    assert_near(landed.point.z, 0.0, 1e-4);
}

/// A box cast at a box meets it when the gap between them closes. Oracle:
/// unit cubes with faces 4 apart, the mover travelling 8, meet at fraction
/// 0.5 on the fixed cube's face `x = 1`. Vertex to vertex, `(2, 3, 6)`
/// apart, the mover travelling `(-4, -6, -12)` meets at fraction 0.5 at the
/// vertex `(1, 1, 1)`.
#[wasm_bindgen_test(unsupported = test)]
fn a_box_cast_at_a_box_meets_it_when_the_gap_closes() {
    let fixed = boxed(Vec3::ZERO, Vec3::ONE);

    let face = cast(
        &fixed,
        &boxed(v(6.0, 0.5, -0.5), Vec3::ONE),
        v(-8.0, 0.0, 0.0),
    );
    assert_hit(&face, 0.5, 8.0, 0.0, v(1.0, 0.0, 0.0));
    assert_near(face.point.x, 1.0, 1e-4);
    assert!((-0.5..=1.0).contains(&face.point.y));
    assert!((-1.0..=0.5).contains(&face.point.z));

    let vertex = cast(
        &fixed,
        &boxed(v(4.0, 5.0, 8.0), Vec3::ONE),
        v(-4.0, -6.0, -12.0),
    );
    assert_hit(&vertex, 0.5, 14.0, 0.0, n236());
    assert_vec_near(vertex.point, v(1.0, 1.0, 1.0), 1e-4);
}

/// A ball cast at a turned box meets the edge the rotation put in its way.
/// Oracle: the cube of half-extent 1 turned 45 degrees about z has an
/// upright edge at `(sqrt(2), 0, z)`. A ball of radius 0.5 from
/// `(6, 0, 0.3)` moving 10 toward it touches when its centre is at
/// `sqrt(2) + 0.5`: fraction `(6 - sqrt(2) - 0.5) / 10`.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_cast_at_a_turned_box_meets_its_edge() {
    let eighth_turn = Rot::from_quat(0.0, 0.0, 0.382_683_43, 0.923_879_5);
    let turned = Proxy::offset(&corners(Vec3::ONE), 0.0, Vec3::ZERO, &eighth_turn);
    let sqrt2 = 2.0f32.sqrt();

    let met = cast(&turned, &ball(v(6.0, 0.0, 0.3), 0.5), v(-10.0, 0.0, 0.0));
    assert_hit(
        &met,
        (6.0 - sqrt2 - 0.5) / 10.0,
        10.0,
        0.5,
        v(1.0, 0.0, 0.0),
    );
    assert_vec_near(met.point, v(sqrt2, 0.0, 0.3), 1e-4);
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
    let cases = [
        (v(-5.0, 3.0, 0.0), v(10.0, 0.0, 4.0), 1.0),
        (v(0.0, 3.0, 0.0), v(0.0, 10.0, 0.0), 1.0),
        (v(0.5, 6.0, -1.0), v(0.0, -3.0, 0.0), 1.0),
        (v(0.5, 6.0, -1.0), v(0.0, -10.0, 0.0), 0.4),
    ];
    for (center, translation, max_fraction) in cases {
        let missed = shape_cast(
            &block(),
            &ball(center, 0.5),
            translation,
            max_fraction,
            false,
            SLOP,
        );
        assert_eq!(missed.state, CastState::Miss);
    }
}

/// A cast that starts touching or overlapping says so. Oracle: a ball of
/// radius 0.5 centred 0.5 above the top face touches it: moving 1 into the
/// face it is met within `SLOP` of travel with the face's normal, and
/// moving along the face it is not met. Centred 0.48 above the face it is
/// 0.02 into it, deeper than `SLOP`: its surface is at height 0.98 and the
/// face at 1, so the point between them is at 0.99. A ball centred inside
/// the box has no normal to give, and two boxes resting face to face have
/// cores that touch, which is overlap for shapes with no radius.
#[wasm_bindgen_test(unsupported = test)]
fn a_cast_that_starts_touching_or_overlapping_says_so() {
    let resting = ball(v(0.0, 1.5, 0.0), 0.5);
    let into = cast(&block(), &resting, v(0.0, -1.0, 0.0));
    assert_hit(&into, 0.0, 1.0, 0.5, v(0.0, 1.0, 0.0));
    let along = cast(&block(), &resting, v(1.0, 0.0, 1.0));
    assert_eq!(along.state, CastState::Miss);

    let sunk = cast(&block(), &ball(v(0.0, 1.48, 0.0), 0.5), v(0.0, -1.0, 0.0));
    assert_eq!(sunk.state, CastState::InitialOverlap);
    assert_eq!(sunk.fraction.to_bits(), 0.0f32.to_bits());
    assert_vec_near(sunk.normal, v(0.0, 1.0, 0.0), EPS);
    assert_vec_near(sunk.point, v(0.0, 0.99, 0.0), EPS);

    let inside = cast(&block(), &ball(v(0.5, 0.25, -1.0), 0.5), v(0.0, -1.0, 0.0));
    assert_eq!(inside.state, CastState::InitialOverlap);
    assert_eq!(inside.fraction.to_bits(), 0.0f32.to_bits());
    assert_eq!(inside.normal, Vec3::ZERO);

    let stacked = cast(
        &block(),
        &boxed(v(0.0, 2.0, 0.0), Vec3::ONE),
        v(0.0, -1.0, 0.0),
    );
    assert_eq!(stacked.state, CastState::InitialOverlap);
    assert_eq!(stacked.fraction.to_bits(), 0.0f32.to_bits());
}

/// A distance query cut off before it settles says so and still answers
/// with points that are on the shapes, no nearer each other than the true
/// closest pair. Oracle: a point 3 above the top face of the box
/// `[-2, 2] x [-1, 1] x [-3, 3]` is 3 from the box, and from no point of
/// the box less. The full query takes more support points than the
/// cut-off one is given.
#[wasm_bindgen_test(unsupported = test)]
fn a_distance_query_cut_off_early_answers_from_its_last_simplex() {
    let mut cut_off = 0;
    for (x, z) in [
        (1.5, 2.5),
        (-1.5, 2.5),
        (1.5, -2.5),
        (-1.5, -2.5),
        (0.5, 0.25),
    ] {
        let above = v(x, 4.0, z);
        let full = gjk(&block(), &ball(above, 0.0), false, MAX_GJK_ITERATIONS);
        assert_eq!(full.state, DistanceState::Separated);
        assert_near(full.distance, 3.0, EPS);

        for cap in 1..full.iterations {
            cut_off += 1;
            let cut = gjk(&block(), &ball(above, 0.0), false, cap);
            assert_eq!(cut.state, DistanceState::IterationCap);
            let on_box = cut.point_a.abs() - v(2.0, 1.0, 3.0);
            assert!(
                on_box.max_element() <= EPS,
                "{} is not on the box",
                cut.point_a
            );
            assert_vec_near(cut.point_b, above, EPS);
            assert_near(cut.distance, length(cut.point_b - cut.point_a), EPS);
            assert!(cut.distance >= 3.0 - EPS);
        }
    }
    assert!(cut_off >= 8, "only {cut_off} queries were cut off");
}

/// Every order of `count` distinct indices below `count`.
fn orders(count: usize) -> Vec<Vec<usize>> {
    let mut out = vec![Vec::new()];
    for _ in 0..count {
        let mut longer = Vec::new();
        for order in &out {
            for index in (0..count).filter(|index| !order.contains(index)) {
                let mut next = order.clone();
                next.push(index);
                longer.push(next);
            }
        }
        out = longer;
    }
    out
}

/// The solvers find the part of a simplex nearest the origin, whichever
/// order its vertices come in. Oracle: each simplex is placed so its
/// nearest point is plain. One whose points all have coordinates of 1 or
/// more is nearest the origin at its corner `(1, 1, 1)`. One that lies in
/// `x >= 1` and touches the plane `x = 1` only along an edge through
/// `(1, 0, 0)`, or only on a face that holds `(1, 0, 0)`, is nearest there:
/// on the edge from `(1, -1, 0)` to `(1, 1, 0)` it is the midpoint, and in
/// the triangle `(1, -1, -1)`, `(1, 2, -1)`, `(1, -1, 2)` it is the corner
/// plus a third of each side. The tetrahedron with corner `(-1, -1, -1)`
/// and sides of 4 along each axis holds the origin a quarter of the way
/// along each. The number of vertices kept is the size of that feature.
#[wasm_bindgen_test(unsupported = test)]
fn the_solvers_find_the_nearest_part_of_a_simplex_in_any_vertex_order() {
    let cases: [(&[Vec3], Vec3, usize); 9] = [
        // A segment: nearest at an end, and in its middle.
        (&[v(1.0, 1.0, 1.0), v(3.0, 2.0, 1.0)], v(1.0, 1.0, 1.0), 1),
        (&[v(1.0, -1.0, 0.0), v(1.0, 1.0, 0.0)], v(1.0, 0.0, 0.0), 2),
        // A triangle: nearest at a corner, on an edge, and inside.
        (
            &[v(1.0, 1.0, 1.0), v(3.0, 1.0, 1.0), v(1.0, 3.0, 1.0)],
            v(1.0, 1.0, 1.0),
            1,
        ),
        (
            &[v(1.0, -1.0, 0.0), v(1.0, 1.0, 0.0), v(3.0, 0.0, 2.0)],
            v(1.0, 0.0, 0.0),
            2,
        ),
        (
            &[v(1.0, -1.0, -1.0), v(1.0, 2.0, -1.0), v(1.0, -1.0, 2.0)],
            v(1.0, 0.0, 0.0),
            3,
        ),
        // A tetrahedron: nearest at a corner, on an edge, on a face, and
        // holding the origin.
        (
            &[
                v(1.0, 1.0, 1.0),
                v(3.0, 1.0, 1.0),
                v(1.0, 3.0, 1.0),
                v(1.0, 1.0, 3.0),
            ],
            v(1.0, 1.0, 1.0),
            1,
        ),
        (
            &[
                v(1.0, -1.0, 0.0),
                v(1.0, 1.0, 0.0),
                v(3.0, 0.0, 2.0),
                v(3.0, 0.0, -2.0),
            ],
            v(1.0, 0.0, 0.0),
            2,
        ),
        (
            &[
                v(1.0, -1.0, -1.0),
                v(1.0, 2.0, -1.0),
                v(1.0, -1.0, 2.0),
                v(4.0, 0.0, 0.0),
            ],
            v(1.0, 0.0, 0.0),
            3,
        ),
        (
            &[
                v(-1.0, -1.0, -1.0),
                v(3.0, -1.0, -1.0),
                v(-1.0, 3.0, -1.0),
                v(-1.0, -1.0, 3.0),
            ],
            Vec3::ZERO,
            4,
        ),
    ];

    // Shape A is the origin alone, so a vertex `wB - wA` is a point of B
    // and the witness point on B is the simplex's nearest point.
    let origin = [Vec3::ZERO];
    for (points, nearest, kept) in cases {
        for order in orders(points.len()) {
            let mut simplex = Simplex::new(SimplexVertex::new(&origin, points, 0, order[0]));
            for &index in &order[1..] {
                simplex.push(SimplexVertex::new(&origin, points, 0, index));
            }
            assert!(simplex.solve(), "{points:?} in order {order:?}");
            assert_eq!(simplex.count, kept, "{points:?} in order {order:?}");
            assert_vec_near(simplex.witness_points().1, nearest, EPS);
        }
    }
}

// --- Properties -----------------------------------------------------------

/// A shape by its own definition, for oracles that use no kernel maths.
#[derive(Clone, Copy, Debug)]
enum Solid {
    Ball {
        center: Vec3,
        radius: f32,
    },
    Capsule {
        p: Vec3,
        q: Vec3,
        radius: f32,
    },
    /// A box of half-extents `half` whose own x, y and z axes point along
    /// the three unit, mutually perpendicular `axes`.
    Box {
        center: Vec3,
        half: Vec3,
        axes: [Vec3; 3],
    },
}

impl Solid {
    fn proxy(&self) -> Proxy {
        match *self {
            Solid::Ball { center, radius } => Proxy::new(&[center], radius),
            Solid::Capsule { p, q, radius } => Proxy::new(&[p, q], radius),
            Solid::Box { center, half, axes } => Proxy::offset(
                &corners(half),
                0.0,
                center,
                &Rot {
                    cx: axes[0],
                    cy: axes[1],
                    cz: axes[2],
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
    /// point clamped to the box along each of the box's own axes.
    fn nearest_core_point(&self, point: Vec3) -> Vec3 {
        match *self {
            Solid::Ball { center, .. } => center,
            Solid::Capsule { p, q, .. } => {
                let along = q - p;
                let t = ((point - p).dot(along) / along.dot(along)).clamp(0.0, 1.0);
                p + along * t
            }
            Solid::Box { center, half, axes } => {
                let rel = point - center;
                (0..3).fold(center, |nearest, axis| {
                    let along = rel.dot(axes[axis]).clamp(-half[axis], half[axis]);
                    nearest + axes[axis] * along
                })
            }
        }
    }

    /// How far `point` is from the shape's core.
    fn core_distance(&self, point: Vec3) -> f32 {
        length(point - self.nearest_core_point(point))
    }

    /// A box's own three axes and its eight corners, where it stands,
    /// numbered so that two corners whose numbers differ in one bit share
    /// an edge.
    fn box_frame(&self) -> ([Vec3; 3], [Vec3; 8]) {
        let Solid::Box { center, half, axes } = *self else {
            panic!("{self:?} is not a box");
        };
        let placed = corners(half)
            .map(|corner| center + axes[0] * corner.x + axes[1] * corner.y + axes[2] * corner.z);
        (axes, placed)
    }

    /// A box's twelve edges, each as its two ends.
    fn box_edges(&self) -> Vec<(Vec3, Vec3)> {
        let (_, placed) = self.box_frame();
        let mut edges = Vec::new();
        for from in 0..8 {
            for bit in [1, 2, 4] {
                if from & bit == 0 {
                    edges.push((placed[from], placed[from | bit]));
                }
            }
        }
        edges
    }
}

/// How far apart two boxes are along the best of the fifteen axes that can
/// separate two boxes (each one's three face normals and the nine cross
/// products of their edges): positive when one of them separates the two,
/// and by the separating axis theorem negative only when the boxes
/// overlap.
fn separation_along_box_axes(a: &Solid, b: &Solid) -> f32 {
    let (axes_a, corners_a) = a.box_frame();
    let (axes_b, corners_b) = b.box_frame();
    let mut axes = Vec::from(axes_a);
    axes.extend(axes_b);
    for edge_a in axes_a {
        for edge_b in axes_b {
            let across = edge_a.cross(edge_b);
            if length(across) > 1e-3 {
                axes.push(across * (1.0 / length(across)));
            }
        }
    }
    let span = |corners: &[Vec3; 8], axis: Vec3| {
        corners
            .iter()
            .map(|corner| corner.dot(axis))
            .fold((f32::MAX, f32::MIN), |(low, high), along| {
                (low.min(along), high.max(along))
            })
    };
    axes.into_iter()
        .map(|axis| {
            let (low_a, high_a) = span(&corners_a, axis);
            let (low_b, high_b) = span(&corners_b, axis);
            (low_b - high_a).max(low_a - high_b)
        })
        .fold(f32::MIN, f32::max)
}

/// The distance between the segments `p1 q1` and `p2 q2`, neither a single
/// point: the closest points of two segments in closed form (Ericson,
/// Real-Time Collision Detection, 5.1.9), by clamping the closest points
/// of the two lines to the segments.
fn segment_distance((p1, q1): (Vec3, Vec3), (p2, q2): (Vec3, Vec3)) -> f32 {
    let d1 = q1 - p1;
    let d2 = q2 - p2;
    let r = p1 - p2;
    let a = d1.dot(d1);
    let e = d2.dot(d2);
    let f = d2.dot(r);
    let c = d1.dot(r);
    let b = d1.dot(d2);
    let denominator = a * e - b * b;
    // Parallel segments are equally near at every point: any one will do.
    let mut s = if denominator > 1e-6 * a * e {
        ((b * f - c * e) / denominator).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut t = (b * s + f) / e;
    if t < 0.0 {
        t = 0.0;
        s = (-c / a).clamp(0.0, 1.0);
    } else if t > 1.0 {
        t = 1.0;
        s = ((b - c) / a).clamp(0.0, 1.0);
    }
    length((p1 + d1 * s) - (p2 + d2 * t))
}

fn random_point(sequence: &mut Sequence, reach: f32) -> Vec3 {
    v(
        sequence.range(-reach, reach),
        sequence.range(-reach, reach),
        sequence.range(-reach, reach),
    )
}

/// A random direction, never near zero.
fn random_direction(sequence: &mut Sequence) -> Vec3 {
    loop {
        let candidate = random_point(sequence, 1.0);
        let norm = length(candidate);
        if norm > 0.2 {
            return candidate * (1.0 / norm);
        }
    }
}

/// Three unit, mutually perpendicular axes, right-handed, from two random
/// directions.
fn random_axes(sequence: &mut Sequence) -> [Vec3; 3] {
    loop {
        let x = random_direction(sequence);
        let other = random_direction(sequence);
        let z = x.cross(other);
        if length(z) > 0.2 {
            let z = z * (1.0 / length(z));
            return [x, z.cross(x), z];
        }
    }
}

fn random_ball(sequence: &mut Sequence, reach: f32) -> Solid {
    Solid::Ball {
        center: random_point(sequence, reach),
        radius: sequence.range(0.2, 1.0),
    }
}

fn random_box(sequence: &mut Sequence, reach: f32) -> Solid {
    Solid::Box {
        center: random_point(sequence, reach),
        half: v(
            sequence.range(0.3, 1.5),
            sequence.range(0.3, 1.5),
            sequence.range(0.3, 1.5),
        ),
        axes: random_axes(sequence),
    }
}

fn random_solid(sequence: &mut Sequence, reach: f32) -> Solid {
    let pick = sequence.unit();
    if pick < 0.3 {
        random_ball(sequence, reach)
    } else if pick < 0.6 {
        let p = random_point(sequence, reach);
        Solid::Capsule {
            p,
            q: p + random_direction(sequence) * sequence.range(0.5, 2.0),
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
    let mut sequence = Sequence::new(0x5eed_0011);
    let mut separated = 0;
    for _ in 0..64 {
        let a = random_solid(&mut sequence, 3.0);
        let b = random_solid(&mut sequence, 3.0);
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
/// sample's spacing of the true one). Two boxes one of their fifteen axes
/// separates are as far apart as the nearest of: a corner of either from
/// the other, and an edge of one from an edge of the other (the closest
/// pair of two polyhedra lies on a corner and a face, or on two edges);
/// two that no axis separates overlap. 96 fixed pairs.
#[wasm_bindgen_test(unsupported = test)]
fn distance_matches_the_shapes_own_definitions() {
    let mut sequence = Sequence::new(0x5eed_0012);
    let mut boxes_apart = 0;
    for _ in 0..96 {
        let fixed = random_box(&mut sequence, 2.0);
        let other = random_solid(&mut sequence, 3.0);
        let found = distance(&fixed.proxy(), &other.proxy(), true);

        let expected = match other {
            Solid::Ball { center, radius } => fixed.core_distance(center) - radius,
            Solid::Capsule { p, q, radius } => {
                let nearest = (0u16..=2000)
                    .map(|i| fixed.core_distance(p + (q - p) * (f32::from(i) / 2000.0)))
                    .fold(f32::MAX, f32::min);
                nearest - radius
            }
            Solid::Box { .. } => {
                let separation = separation_along_box_axes(&fixed, &other);
                if separation < -2e-3 {
                    assert_eq!(
                        found.state,
                        DistanceState::Overlapped,
                        "{fixed:?} {other:?}"
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
                let mut nearest =
                    nearest_corner(&fixed, &other).min(nearest_corner(&other, &fixed));
                for edge in fixed.box_edges() {
                    for other_edge in other.box_edges() {
                        nearest = nearest.min(segment_distance(edge, other_edge));
                    }
                }
                nearest
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
    let mut sequence = Sequence::new(0x5eed_0013);
    let (mut hits, mut misses) = (0, 0);
    for _ in 0..96 {
        let fixed = if sequence.unit() < 0.7 {
            random_box(&mut sequence, 1.5)
        } else {
            random_ball(&mut sequence, 1.5)
        };
        let Solid::Ball { center, radius } = random_ball(&mut sequence, 5.0) else {
            unreachable!("random_ball makes a ball");
        };
        // Aimed near the fixed shape, and from half as far as it to half
        // as far again, so hits, near misses and motions that stop short
        // all occur.
        let aim = random_point(&mut sequence, 1.5);
        let translation = (aim - center) * sequence.range(0.5, 1.5);

        let total_radius = fixed.radius() + radius;
        let target = (total_radius - SLOP).max(SLOP);
        let tolerance = 0.25 * SLOP;
        let core_distance_at = |fraction: f32| fixed.core_distance(center + translation * fraction);

        let found = cast(&fixed.proxy(), &ball(center, radius), translation);
        match found.state {
            CastState::Hit => {
                hits += 1;
                let at_hit = core_distance_at(found.fraction);
                assert!(
                    at_hit >= target - 1e-4 && at_hit <= target + tolerance + 1e-4,
                    "{at_hit} apart at the hit, target {target}"
                );
                for step in 0u8..8 {
                    let before = core_distance_at(found.fraction * f32::from(step) / 8.0);
                    assert!(before >= target - 1e-4, "{before} apart before the hit");
                }
                // The normal runs from the fixed shape's nearest point to
                // the ball's centre.
                let ball_at_hit = center + translation * found.fraction;
                let outward = ball_at_hit - fixed.nearest_core_point(ball_at_hit);
                assert_vec_near(found.normal, outward * (1.0 / length(outward)), 1e-3);
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
    type Pair = (fn(Vec3) -> Proxy, fn(Vec3) -> Proxy, Option<Vec3>);
    let pairs: [Pair; 5] = [
        // Ball against ball, (2, 3, 6) apart.
        (|o| ball(o, 1.0), |o| ball(o + v(2.0, 3.0, 6.0), 0.5), None),
        // Ball over a box's face.
        (
            |o| boxed(o, v(2.0, 1.0, 4.0)),
            |o| ball(o + v(0.5, 6.0, -1.0), 0.5),
            Some(v(0.0, -8.0, 0.0)),
        ),
        // Box against box, vertex to vertex.
        (
            |o| boxed(o, Vec3::ONE),
            |o| boxed(o + v(4.0, 5.0, 8.0), Vec3::ONE),
            None,
        ),
        // Box against box, face to face.
        (
            |o| boxed(o, Vec3::ONE),
            |o| boxed(o + v(6.0, 0.5, -0.5), Vec3::ONE),
            Some(v(-8.0, 0.0, 0.0)),
        ),
        // Capsule over a box's face.
        (
            |o| boxed(o, v(2.0, 1.0, 4.0)),
            |o| capsule(o + v(-1.0, 5.0, 0.0), o + v(1.0, 5.0, 0.0), 0.5),
            Some(v(0.0, -8.0, 0.0)),
        ),
    ];
    let slop = 0.0625;

    for (make_a, make_b, translation) in pairs {
        let (home_a, home_b) = (make_a(Vec3::ZERO), make_b(Vec3::ZERO));
        let home_distance = distance(&home_a, &home_b, true);
        let home_cores = distance(&home_a, &home_b, false);
        assert_eq!(home_distance.state, DistanceState::Separated);

        for offset in [
            v(1024.0, -2048.0, 512.0),
            v(-4096.0, 512.0, 2048.0),
            v(8.0, 16.0, -32.0),
        ] {
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
