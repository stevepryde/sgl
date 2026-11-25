//! Round bodies (balls, capsules) and convex colliders (#919): every pair the
//! convex kernel meets, through the public queries, the mover and the worlds.
//! Every expected value comes from geometry worked in the test, a shape's
//! own definition evaluated without the kernel, or a requirement of
//! `specs/physics.md`. Runs natively and in a wasm runtime.
// The case tables of hand-worked pairs run long.
#![allow(clippy::cast_precision_loss, clippy::too_many_lines)]

use core::f32::consts::{FRAC_1_SQRT_2, FRAC_PI_4, SQRT_2};

use sgl_core::math::{IVec2, IVec3, Quat, Vec2, Vec3};
use sgl_core::{CanonicalWrite, SplitMix64, StateHasher};
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, ColliderKind, ColliderSet, Convex, Cut, Hit,
    MoverConfig, MoverOptions, MoverSettings, MoverState, Shape, Vector, World, cast, distance,
    move_and_collide, move_and_slide, overlaps, ray, slide,
};
use wasm_bindgen_test::wasm_bindgen_test;

const SKIN: f32 = 0.01;

fn config<V: Vector>(up: V) -> MoverConfig<V> {
    MoverConfig::new(MoverSettings {
        up,
        skin: SKIN,
        snap_distance: 0.1,
        normal_threshold: 0.7,
        block_epsilon: 1e-4,
    })
}

fn v2(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

fn v3(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z)
}

fn ball<V>(radius: f32) -> Shape<V> {
    Shape::Ball { radius }
}

fn capsule<V>(half_axis: V, radius: f32) -> Shape<V> {
    Shape::Capsule { half_axis, radius }
}

fn cuboid<V>(half: V) -> Shape<V> {
    Shape::Box { half }
}

fn length<V: Vector>(v: V) -> f32 {
    (0..V::AXES)
        .map(|axis| v[axis] * v[axis])
        .sum::<f32>()
        .sqrt()
}

/// The contract's largest query slop for a pair: a hundredth of the
/// smaller shape's extent (its largest half-extent), and at most a quarter
/// of the smaller radius, shapes with no extent (a ray) or no radius not
/// counting (physics.md, "Round and convex shapes").
fn slop_bound<V: Vector>(a: &Shape<V>, b: &Shape<V>) -> f32 {
    let extent = |shape: &Shape<V>| {
        let half = shape.half_extents();
        (0..V::AXES).fold(0.0f32, |most, axis| most.max(half[axis]))
    };
    let radius = |shape: &Shape<V>| match *shape {
        Shape::Ball { radius } | Shape::Capsule { radius, .. } => radius,
        _ => 0.0,
    };
    let fitted = [extent(a), extent(b)]
        .into_iter()
        .filter(|&e| e > 0.0)
        .fold(f32::INFINITY, f32::min)
        / 100.0;
    [radius(a), radius(b)]
        .into_iter()
        .filter(|&r| r > 0.0)
        .fold(fitted, |slop, r| slop.min(r / 4.0))
}

/// A cast or ray worked by hand: the body (a box with no extent for a ray)
/// at `start` moving by `motion` toward one collider. The shapes touch at
/// `touch` of the motion, where the gap between them closes at `speed` per
/// unit of the motion, across the face of unit `normal`. Every case closes
/// head-on on a flat face or a single point, so the gap shrinks linearly.
struct Case<V> {
    name: &'static str,
    body: Shape<V>,
    start: V,
    motion: V,
    collider: Collider<V>,
    touch: f32,
    speed: f32,
    normal: V,
}

/// Defect: a kernel pair whose contact is past touching (the body is cast
/// into the collider) or far short of it, a ray that is not exact, with the
/// wrong normal, or a mover that does not leave the skin. Oracle: the
/// geometry each case works by hand, and the contract: a query meets a
/// kernel pair before touching and within 1.25 slops of it, the slop at
/// most a hundredth of the smaller shape's extent; a ray meets a ball, a
/// capsule or a polygon where it touches, to a few units in the last place
/// of its coordinates (here 4 ε of the motion's 10, in `t`); a 3D hull of
/// eight points stays on the kernel; `move_and_collide` leaves between
/// `skin` and `skin * (1 + 1/64)` (its slop is a sixteenth of the skin, met
/// within a quarter of it).
fn check<V: Vector>(cases: &[Case<V>]) {
    for case in cases {
        let world = [case.collider];
        let point = case.body == cuboid(V::ZERO);
        let hit = if point {
            ray(case.start, case.motion, &world[..], ALL_LAYERS)
        } else {
            cast(&case.body, case.start, case.motion, &world[..], ALL_LAYERS)
        };
        let hit: Hit<V> = hit
            .unwrap()
            .unwrap_or_else(|| panic!("{}: no hit", case.name));
        let hull = matches!(case.collider.shape, Shape::Convex(_)) && V::AXES == 3;
        if point && !hull {
            assert!(
                (hit.t - case.touch).abs() <= 4.0 * f32::EPSILON,
                "{}: t {} against {}",
                case.name,
                hit.t,
                case.touch
            );
        } else {
            let slop = slop_bound(&case.body, &case.collider.shape);
            let earliest = case.touch - 1.25 * slop / case.speed;
            assert!(
                hit.t < case.touch && hit.t >= earliest - 1e-6,
                "{}: t {} outside {earliest}..{}",
                case.name,
                hit.t,
                case.touch
            );
        }
        assert!(
            length(hit.normal - case.normal) < 1e-4,
            "{}: normal {:?}, expected {:?}",
            case.name,
            hit.normal,
            case.normal
        );
        assert_eq!(hit.id, case.collider.id, "{}", case.name);

        let (moved, contact) = move_and_collide(
            &case.body,
            case.start,
            case.motion,
            &world[..],
            ALL_LAYERS,
            &config(case.normal),
        )
        .unwrap();
        assert!(contact.is_some(), "{}: the mover met nothing", case.name);
        let gap = (case.touch - length(moved) / length(case.motion)) * case.speed;
        assert!(
            (SKIN - 1e-5..=SKIN * (1.0 + 1.0 / 64.0) + 1e-5).contains(&gap),
            "{}: rests {gap} from the collider",
            case.name
        );
    }
}

/// A 2 x 1 half-extent box at the origin, the target of several cases.
fn slab2(id: u64) -> Collider<Vec2> {
    Collider::new(id, cuboid(v2(2.0, 1.0)), Vec2::ZERO)
}

/// A unit-half-extent square turned an eighth of a turn: a diamond with
/// corners `sqrt(2)` out along each axis and faces 1 from its centre along
/// the diagonals.
fn diamond2(id: u64) -> Collider<Vec2> {
    let square = [v2(-1.0, -1.0), v2(1.0, -1.0), v2(1.0, 1.0), v2(-1.0, 1.0)];
    let turned = Convex::polygon(&square, FRAC_1_SQRT_2, FRAC_1_SQRT_2);
    Collider::new(id, Shape::Convex(turned), Vec2::ZERO)
}

/// Every new pair in 2D, worked by hand.
#[wasm_bindgen_test(unsupported = test)]
fn every_new_pair_meets_where_geometry_puts_it_in_2d() {
    // The ramp: a box of half-extent 1 cut along its diagonal, solid where
    // y <= x. A ball of radius 0.5 falling at x = 0.25 touches the plane
    // when its centre is 0.5 from it: (y - 0.25) / sqrt(2) = 0.5, at
    // y = 0.25 + sqrt(2) / 2, the foot (0.604, 0.604) on the cut face.
    let ramp =
        Collider::new(7, cuboid(v2(1.0, 1.0)), Vec2::ZERO).with_cut(Cut::new(v2(-1.0, 1.0), 0.0));
    let ramp_touch = (3.0 - (0.25 + FRAC_1_SQRT_2)) / 4.0;
    let diagonal = v2(FRAC_1_SQRT_2, FRAC_1_SQRT_2);
    let triangle = Convex::polygon(&[v2(-0.5, -0.5), v2(0.5, -0.5), v2(0.0, 0.5)], 1.0, 0.0);
    check(&[
        // Centres 5 apart, radii 1.5: they touch after 3.5 of 10.
        Case {
            name: "ball at ball",
            body: ball(0.5),
            start: v2(-5.0, 0.0),
            motion: v2(10.0, 0.0),
            collider: Collider::new(1, ball(1.0), Vec2::ZERO),
            touch: 0.35,
            speed: 10.0,
            normal: Vec2::NEG_X,
        },
        // The slab's top is y = 1: the centre reaches 1.5 after 1.5 of 4.
        Case {
            name: "ball at a box's face",
            body: ball(0.5),
            start: v2(0.5, 3.0),
            motion: v2(0.0, -4.0),
            collider: slab2(2),
            touch: 0.375,
            speed: 4.0,
            normal: Vec2::Y,
        },
        // Straight at the corner (2, 1) from 5 away along (0.6, 0.8): the
        // radius 1 touches it after 4 of 10.
        Case {
            name: "ball at a box's corner",
            body: ball(1.0),
            start: v2(5.0, 5.0),
            motion: v2(-6.0, -8.0),
            collider: slab2(3),
            touch: 0.4,
            speed: 10.0,
            normal: v2(0.6, 0.8),
        },
        // A lying capsule's segment reaches y = 1.5 after 1.5 of 4.
        Case {
            name: "capsule at a box's face",
            body: capsule(v2(1.0, 0.0), 0.5),
            start: v2(0.0, 3.0),
            motion: v2(0.0, -4.0),
            collider: slab2(4),
            touch: 0.375,
            speed: 4.0,
            normal: Vec2::Y,
        },
        // A standing capsule's lower end (0, y - 1) is 1.5 from the ball's
        // centre at y = 2.5, after 2.5 of 8.
        Case {
            name: "capsule at ball",
            body: capsule(v2(0.0, 1.0), 0.5),
            start: v2(0.0, 5.0),
            motion: v2(0.0, -8.0),
            collider: Collider::new(5, ball(1.0), Vec2::ZERO),
            touch: 0.3125,
            speed: 8.0,
            normal: Vec2::Y,
        },
        // A ball's centre reaches a lying capsule's side (y = 0.5 + 0.5)
        // after 2 of 4.
        Case {
            name: "ball at capsule",
            body: ball(0.5),
            start: v2(0.3, 3.0),
            motion: v2(0.0, -4.0),
            collider: Collider::new(6, capsule(v2(1.0, 0.0), 0.5), Vec2::ZERO),
            touch: 0.5,
            speed: 4.0,
            normal: Vec2::Y,
        },
        // A box's right face reaches the ball's leftmost point x = -1
        // after 3.5 of 10.
        Case {
            name: "box at ball",
            body: cuboid(v2(0.5, 0.5)),
            start: v2(-5.0, 0.0),
            motion: v2(10.0, 0.0),
            collider: Collider::new(8, ball(1.0), Vec2::ZERO),
            touch: 0.35,
            speed: 10.0,
            normal: Vec2::NEG_X,
        },
        // Falling at 4 a unit, the gap to the 45 degree plane closes at
        // 4 / sqrt(2).
        Case {
            name: "ball at a ramp",
            body: ball(0.5),
            start: v2(0.25, 3.0),
            motion: v2(0.0, -4.0),
            collider: ramp,
            touch: ramp_touch,
            speed: 4.0 * FRAC_1_SQRT_2,
            normal: v2(-FRAC_1_SQRT_2, FRAC_1_SQRT_2),
        },
        // Along the diamond's diagonal from 3 beyond its face, a radius
        // 0.5 touches after 2.5 of 4.
        Case {
            name: "ball at a turned square",
            body: ball(0.5),
            start: diagonal * 4.0,
            motion: diagonal * -4.0,
            collider: diamond2(9),
            touch: 0.625,
            speed: 4.0,
            normal: diagonal,
        },
        // A triangle's base reaches the slab's top after 1.5 of 4.
        Case {
            name: "triangle at a box",
            body: Shape::Convex(triangle),
            start: v2(0.0, 3.0),
            motion: v2(0.0, -4.0),
            collider: slab2(10),
            touch: 0.375,
            speed: 4.0,
            normal: Vec2::Y,
        },
        // A thin rod, a capsule of radius 0.02 lying 2 long: its slop is a
        // quarter of its radius, 0.005, not a hundredth of its extent,
        // 0.0102. Its side reaches the slab's top after 1.98 of 4.
        Case {
            name: "thin capsule at a box's face",
            body: capsule(v2(1.0, 0.0), 0.02),
            start: v2(0.0, 3.0),
            motion: v2(0.0, -4.0),
            collider: slab2(14),
            touch: 0.495,
            speed: 4.0,
            normal: Vec2::Y,
        },
        // A ray from 5 left of a radius-1 ball's centre meets it after 4 of
        // its 10: exactly, at t = 0.4.
        Case {
            name: "ray at ball",
            body: cuboid(Vec2::ZERO),
            start: v2(-5.0, 0.0),
            motion: v2(10.0, 0.0),
            collider: Collider::new(11, ball(1.0), Vec2::ZERO),
            touch: 0.4,
            speed: 10.0,
            normal: Vec2::NEG_X,
        },
        // A standing capsule's side is at x = -0.5: met after 4.5 of 10,
        // exactly, at t = 0.45.
        Case {
            name: "ray at capsule",
            body: cuboid(Vec2::ZERO),
            start: v2(-5.0, 0.5),
            motion: v2(10.0, 0.0),
            collider: Collider::new(12, capsule(v2(0.0, 1.0), 0.5), Vec2::ZERO),
            touch: 0.45,
            speed: 10.0,
            normal: Vec2::NEG_X,
        },
        // The diamond's upper-left face is the line `(y - x) / sqrt(2) = 1`
        // with outward normal (-1, 1) / sqrt(2); a ray along y = 0.25 meets
        // it at x = 0.25 - sqrt(2), after 5.25 - sqrt(2) of its 10, closing
        // on it at 10 / sqrt(2). (A ray at the corner would meet both faces
        // at once and report one of them.)
        Case {
            name: "ray at a turned square",
            body: cuboid(Vec2::ZERO),
            start: v2(-5.0, 0.25),
            motion: v2(10.0, 0.0),
            collider: diamond2(13),
            touch: (5.25 - SQRT_2) / 10.0,
            speed: 10.0 * FRAC_1_SQRT_2,
            normal: v2(-FRAC_1_SQRT_2, FRAC_1_SQRT_2),
        },
    ]);
}

/// A 2 x 1 x 1 half-extent box at the origin.
fn slab3(id: u64) -> Collider<Vec3> {
    Collider::new(id, cuboid(v3(2.0, 1.0, 1.0)), Vec3::ZERO)
}

/// A unit-half-extent cube turned an eighth of a turn about z: corners
/// `sqrt(2)` out along x and y, faces 1 from its centre along the xy
/// diagonals.
fn diamond3(id: u64) -> Collider<Vec3> {
    let mut corners = Vec::new();
    for x in [-1.0, 1.0] {
        for y in [-1.0, 1.0] {
            for z in [-1.0, 1.0] {
                corners.push(v3(x, y, z));
            }
        }
    }
    let turned = Convex::hull(&corners, Quat::from_rotation_z(FRAC_PI_4));
    Collider::new(id, Shape::Convex(turned), Vec3::ZERO)
}

/// Every new pair in 3D, worked by hand.
#[wasm_bindgen_test(unsupported = test)]
fn every_new_pair_meets_where_geometry_puts_it_in_3d() {
    let ramp =
        Collider::new(7, cuboid(Vec3::ONE), Vec3::ZERO).with_cut(Cut::new(v3(-1.0, 1.0, 0.0), 0.0));
    let ramp_touch = (3.0 - (0.25 + FRAC_1_SQRT_2)) / 4.0;
    let diagonal = v3(FRAC_1_SQRT_2, FRAC_1_SQRT_2, 0.0);
    // The slab's corner (2, 1, 1), approached along (2, 2, 1) / 3 from 6
    // away by a radius 1: it touches after 5 of 10.
    let toward = v3(2.0, 2.0, 1.0) / 3.0;
    check(&[
        Case {
            name: "ball at ball",
            body: ball(0.5),
            start: v3(-5.0, 0.0, 0.0),
            motion: v3(10.0, 0.0, 0.0),
            collider: Collider::new(1, ball(1.0), Vec3::ZERO),
            touch: 0.35,
            speed: 10.0,
            normal: Vec3::NEG_X,
        },
        Case {
            name: "ball at a box's face",
            body: ball(0.5),
            start: v3(0.5, 3.0, 0.25),
            motion: v3(0.0, -4.0, 0.0),
            collider: slab3(2),
            touch: 0.375,
            speed: 4.0,
            normal: Vec3::Y,
        },
        // Straight at the edge x = 2, y = 1 (z = 0 is within it) along
        // (0.6, 0.8, 0) from 5 away.
        Case {
            name: "ball at a box's edge",
            body: ball(1.0),
            start: v3(5.0, 5.0, 0.0),
            motion: v3(-6.0, -8.0, 0.0),
            collider: slab3(3),
            touch: 0.4,
            speed: 10.0,
            normal: v3(0.6, 0.8, 0.0),
        },
        Case {
            name: "ball at a box's corner",
            body: ball(1.0),
            start: v3(2.0, 1.0, 1.0) + toward * 6.0,
            motion: toward * -10.0,
            collider: slab3(4),
            touch: 0.5,
            speed: 10.0,
            normal: toward,
        },
        Case {
            name: "capsule at a box's face",
            body: capsule(v3(1.0, 0.0, 0.0), 0.5),
            start: v3(0.0, 3.0, 0.0),
            motion: v3(0.0, -4.0, 0.0),
            collider: slab3(5),
            touch: 0.375,
            speed: 4.0,
            normal: Vec3::Y,
        },
        Case {
            name: "capsule at ball",
            body: capsule(v3(0.0, 1.0, 0.0), 0.5),
            start: v3(0.0, 5.0, 0.0),
            motion: v3(0.0, -8.0, 0.0),
            collider: Collider::new(6, ball(1.0), Vec3::ZERO),
            touch: 0.3125,
            speed: 8.0,
            normal: Vec3::Y,
        },
        Case {
            name: "box at ball",
            body: cuboid(Vec3::splat(0.5)),
            start: v3(-5.0, 0.0, 0.0),
            motion: v3(10.0, 0.0, 0.0),
            collider: Collider::new(8, ball(1.0), Vec3::ZERO),
            touch: 0.35,
            speed: 10.0,
            normal: Vec3::NEG_X,
        },
        Case {
            name: "ball at a ramp",
            body: ball(0.5),
            start: v3(0.25, 3.0, 0.0),
            motion: v3(0.0, -4.0, 0.0),
            collider: ramp,
            touch: ramp_touch,
            speed: 4.0 * FRAC_1_SQRT_2,
            normal: v3(-FRAC_1_SQRT_2, FRAC_1_SQRT_2, 0.0),
        },
        Case {
            name: "ball at a turned cube",
            body: ball(0.5),
            start: diagonal * 4.0,
            motion: diagonal * -4.0,
            collider: diamond3(9),
            touch: 0.625,
            speed: 4.0,
            normal: diagonal,
        },
        Case {
            name: "ray at ball",
            body: cuboid(Vec3::ZERO),
            start: v3(-5.0, 0.0, 0.0),
            motion: v3(10.0, 0.0, 0.0),
            collider: Collider::new(11, ball(1.0), Vec3::ZERO),
            touch: 0.4,
            speed: 10.0,
            normal: Vec3::NEG_X,
        },
        Case {
            name: "ray at capsule",
            body: cuboid(Vec3::ZERO),
            start: v3(-5.0, 0.5, 0.0),
            motion: v3(10.0, 0.0, 0.0),
            collider: Collider::new(12, capsule(v3(0.0, 1.0, 0.0), 0.5), Vec3::ZERO),
            touch: 0.45,
            speed: 10.0,
            normal: Vec3::NEG_X,
        },
        Case {
            name: "ray at a turned cube",
            body: cuboid(Vec3::ZERO),
            start: v3(-5.0, 0.0, 0.0),
            motion: v3(10.0, 0.0, 0.0),
            collider: diamond3(13),
            touch: (5.0 - SQRT_2) / 10.0,
            speed: 10.0,
            normal: Vec3::NEG_X,
        },
    ]);
}

/// Defect: an overlap test that reads the shapes' boxes, or their cores,
/// or allows a margin, instead of the shapes. Oracle: the shapes'
/// definitions. A ball of radius 1 at the origin overlaps a ball of radius
/// 0.5 at 1.49 (centres closer than 1.5) and not one at 1.51; it does not
/// overlap a box whose corner (0.71, 0.71) is 1.004 from its centre, though
/// their boxes overlap, and does overlap one whose corner (0.6, 0.6) is
/// 0.85 from it; it overlaps a standing capsule whose side is at x = 0.99
/// and not one at x = 1.01, and the turned square whose corner is 0.99
/// from its centre, but not one whose corner is 1.01 from it.
#[wasm_bindgen_test(unsupported = test)]
fn overlaps_reads_the_shapes_themselves() {
    let pairs = [
        (Collider::new(1, ball(0.5), v2(1.49, 0.0)), true),
        (Collider::new(2, ball(0.5), v2(1.51, 0.0)), false),
        (Collider::new(3, cuboid(Vec2::ONE), v2(1.71, 1.71)), false),
        (Collider::new(4, cuboid(Vec2::ONE), v2(1.6, 1.6)), true),
        (
            Collider::new(5, capsule(v2(0.0, 1.0), 0.5), v2(1.49, 0.0)),
            true,
        ),
        (
            Collider::new(6, capsule(v2(0.0, 1.0), 0.5), v2(1.51, 0.0)),
            false,
        ),
        (moved(diamond2(7), v2(SQRT_2 + 0.99, 0.0)), true),
        (moved(diamond2(8), v2(SQRT_2 + 1.01, 0.0)), false),
    ];
    for (collider, expected) in pairs {
        let found = overlaps(&ball(1.0), Vec2::ZERO, &[collider][..], ALL_LAYERS).unwrap();
        assert_eq!(!found.is_empty(), expected, "{collider:?}");
    }
    // The same in 3D, along a diagonal of the xz plane for the boxes.
    let pairs3 = [
        (Collider::new(1, ball(0.5), v3(0.0, 0.0, 1.49)), true),
        (Collider::new(2, ball(0.5), v3(0.0, 0.0, 1.51)), false),
        (
            Collider::new(3, cuboid(Vec3::ONE), v3(1.71, 0.0, 1.71)),
            false,
        ),
        (Collider::new(4, cuboid(Vec3::ONE), v3(1.6, 0.0, 1.6)), true),
        (moved(diamond3(7), v3(SQRT_2 + 0.99, 0.0, 0.0)), true),
        (moved(diamond3(8), v3(SQRT_2 + 1.01, 0.0, 0.0)), false),
    ];
    for (collider, expected) in pairs3 {
        let found = overlaps(&ball(1.0), Vec3::ZERO, &[collider][..], ALL_LAYERS).unwrap();
        assert_eq!(!found.is_empty(), expected, "{collider:?}");
    }
}

/// The same collider moved by `by`.
fn moved<V: Vector>(collider: Collider<V>, by: V) -> Collider<V> {
    Collider {
        position: collider.position + by,
        ..collider
    }
}

/// Defect: a distance that reads the boxes or the cores, or a normal the
/// wrong way round. Oracle: geometry. Balls of radii 1 and 1.5 with
/// centres 5 apart along (0.6, 0.8) are 2.5 apart; a ball of radius 1
/// centred (3, 4) from a box's corner is 4 from it; a capsule lying 2 above
/// the box's face, radius 0.5, is 1.5 from it; two boxes whose corners are
/// (3, 4) apart are 5 apart, measured in closed form; shapes that overlap
/// are 0 apart with no normal. Every normal runs from the collider toward
/// the shape, on either side of it.
#[wasm_bindgen_test(unsupported = test)]
fn distance_is_the_gap_between_the_shapes() {
    let cases = [
        (
            ball(1.0),
            v2(3.0, 4.0),
            Collider::new(1, ball(1.5), Vec2::ZERO),
            2.5,
            v2(0.6, 0.8),
        ),
        (ball(1.0), v2(5.0, 5.0), slab2(2), 4.0, v2(0.6, 0.8)),
        (
            capsule(v2(1.0, 0.0), 0.5),
            v2(0.0, 3.0),
            slab2(3),
            1.5,
            Vec2::Y,
        ),
        (cuboid(Vec2::ONE), v2(6.0, 6.0), slab2(4), 5.0, v2(0.6, 0.8)),
        (
            cuboid(Vec2::ONE),
            v2(-6.0, -6.0),
            slab2(4),
            5.0,
            v2(-0.6, -0.8),
        ),
        (ball(1.0), v2(0.0, 1.5), slab2(5), 0.0, Vec2::ZERO),
        (cuboid(Vec2::ONE), v2(0.0, 1.5), slab2(6), 0.0, Vec2::ZERO),
    ];
    for (body, at, collider, gap, normal) in cases {
        let found = distance(&body, at, &collider).expect("both are sound");
        assert!((found.distance - gap).abs() < 1e-5, "{body:?}: {found:?}");
        assert!(length(found.normal - normal) < 1e-5, "{body:?}: {found:?}");
    }
    let found = distance(&ball(1.0), v3(2.0, 1.0, 1.0) + v3(2.0, 2.0, 1.0), &slab3(1)).unwrap();
    assert!((found.distance - 2.0).abs() < 1e-5, "{found:?}");
    let broken = Collider::new(9, ball(-1.0), Vec2::ZERO);
    assert_eq!(distance(&ball(1.0), Vec2::ZERO, &broken), None);
}

// ------------------------------------------------------------ plain geometry
//
// Distances from a point to each shape, from the shapes' definitions, with
// no crate code: the oracles of the scenarios and the property below.

/// Distance from `p` to the box of half-extents `half` at `c`, zero inside.
fn to_box<V: Vector>(p: V, c: V, half: V) -> f32 {
    let mut outside = V::ZERO;
    for axis in 0..V::AXES {
        outside[axis] = ((p[axis] - c[axis]).abs() - half[axis]).max(0.0);
    }
    length(outside)
}

/// Distance from `p` to the segment `a b`.
fn to_segment<V: Vector>(p: V, a: V, b: V) -> f32 {
    let ab = b - a;
    let along: f32 = (0..V::AXES)
        .map(|axis| (p[axis] - a[axis]) * ab[axis])
        .sum();
    let span: f32 = (0..V::AXES).map(|axis| ab[axis] * ab[axis]).sum();
    let t = if span > 0.0 {
        (along / span).clamp(0.0, 1.0)
    } else {
        0.0
    };
    length(p - (a + ab * t))
}

/// Distance from `p` to the convex polygon `vertices` (counter-clockwise),
/// zero inside.
fn to_polygon(p: Vec2, vertices: &[Vec2]) -> f32 {
    let n = vertices.len();
    let inside = (0..n).all(|i| {
        let (a, b) = (vertices[i], vertices[(i + 1) % n]);
        (b - a).perp_dot(p - a) >= 0.0
    });
    if inside {
        return 0.0;
    }
    (0..n)
        .map(|i| to_segment(p, vertices[i], vertices[(i + 1) % n]))
        .fold(f32::INFINITY, f32::min)
}

/// The corners of a 2D box at `c` cut by the plane `n · (p - c) <= offset`,
/// counter-clockwise: the corners on the solid side and the points the
/// plane crosses the edges at, walked around the box.
fn clipped(c: Vec2, half: Vec2, n: Vec2, offset: f32) -> Vec<Vec2> {
    let corners = [
        v2(-half.x, -half.y),
        v2(half.x, -half.y),
        v2(half.x, half.y),
        v2(-half.x, half.y),
    ];
    let side = |p: Vec2| n.dot(p) - offset;
    let mut out = Vec::new();
    for i in 0..4 {
        let (a, b) = (corners[i], corners[(i + 1) % 4]);
        if side(a) <= 0.0 {
            out.push(c + a);
        }
        if (side(a) < 0.0) != (side(b) < 0.0) && side(a) != 0.0 && side(b) != 0.0 {
            out.push(c + a + (b - a) * (side(a) / (side(a) - side(b))));
        }
    }
    out
}

/// Distance from `p` to a 2D collider, zero inside.
fn to_collider2(p: Vec2, collider: &Collider<Vec2>) -> f32 {
    let c = collider.position;
    match (collider.shape, collider.cut) {
        (Shape::Box { half }, None) => to_box(p, c, half),
        (Shape::Box { half }, Some(cut)) => {
            to_polygon(p, &clipped(c, half, cut.normal(), cut.offset()))
        }
        (Shape::Ball { radius }, _) => (length(p - c) - radius).max(0.0),
        (Shape::Capsule { half_axis, radius }, _) => {
            (to_segment(p, c - half_axis, c + half_axis) - radius).max(0.0)
        }
        (Shape::Convex(convex), _) => {
            let vertices: Vec<Vec2> = convex.points().iter().map(|&q| c + q).collect();
            to_polygon(p, &vertices)
        }
    }
}

/// Distance from `p` to a 3D collider, zero inside. A cut box turns about
/// one axis, so it is its cross-section in the other two, swept along that
/// one; a convex collider here is always a cube of half-extent `CUBE` turned
/// by `turn`, measured in its own frame.
fn to_collider3(point: Vec3, collider: &Collider<Vec3>, turn: Quat) -> f32 {
    let centre = collider.position;
    match (collider.shape, collider.cut) {
        (Shape::Box { half }, None) => to_box(point, centre, half),
        (Shape::Box { half }, Some(cut)) => {
            let normal = cut.normal();
            let along = (0..3)
                .find(|&axis| normal[axis] == 0.0)
                .expect("a 3D cut turns about an axis");
            let (first, second) = match along {
                0 => (1, 2),
                1 => (2, 0),
                _ => (0, 1),
            };
            let flat = |v: Vec3| v2(v[first], v[second]);
            let section = clipped(flat(centre), flat(half), flat(normal), cut.offset());
            let across = to_polygon(flat(point), &section);
            let beyond = ((point[along] - centre[along]).abs() - half[along]).max(0.0);
            across.hypot(beyond)
        }
        (Shape::Ball { radius }, _) => (length(point - centre) - radius).max(0.0),
        (Shape::Capsule { half_axis, radius }, _) => {
            (to_segment(point, centre - half_axis, centre + half_axis) - radius).max(0.0)
        }
        (Shape::Convex(_), _) => to_box(
            turn.inverse() * (point - centre),
            Vec3::ZERO,
            Vec3::splat(CUBE),
        ),
    }
}

/// The half-extent of the turned cubes the 3D worlds hold: turned any way,
/// one reaches at most `0.25 * sqrt(3)`, inside its cell.
const CUBE: f32 = 0.25;

/// A cube of half-extent [`CUBE`] turned by `turn`, as a hull.
fn turned_cube(turn: Quat) -> Convex<Vec3> {
    let corners: Vec<Vec3> = (0u8..8)
        .map(|i| {
            let sign = |bit: u8| if i >> bit & 1 == 0 { -CUBE } else { CUBE };
            v3(sign(0), sign(1), sign(2))
        })
        .collect();
    Convex::hull(&corners, turn)
}

/// How far a round body at `at` reaches into a collider, by its definition:
/// its radius less the least distance from its core (a point, or a
/// segment) to the collider. Positive when it overlaps. Every collider is
/// convex, so the distance to it is convex along the segment, and a golden
/// section search finds its least to the rounding of `to`; sampling, by
/// contrast, misses a corner between samples by up to half their spacing.
fn depth<V: Vector>(body: &Shape<V>, at: V, to: impl Fn(V) -> f32) -> f32 {
    match *body {
        Shape::Ball { radius } => radius - to(at),
        Shape::Capsule { half_axis, radius } => {
            let point = |s: f32| to(at + half_axis * s);
            let ratio = (5.0f32.sqrt() - 1.0) / 2.0;
            let (mut lo, mut hi) = (-1.0f32, 1.0f32);
            for _ in 0..40 {
                let (m1, m2) = (hi - ratio * (hi - lo), lo + ratio * (hi - lo));
                if point(m1) <= point(m2) {
                    hi = m2;
                } else {
                    lo = m1;
                }
            }
            let nearest = [-1.0, 1.0, 0.5 * (lo + hi)]
                .into_iter()
                .map(point)
                .fold(f32::INFINITY, f32::min);
            radius - nearest
        }
        _ => unreachable!("round bodies only"),
    }
}

/// Whether a collider's enclosing box lies within `margin` of a body's at
/// `at` on every axis: one further away cannot reach the body, its depth
/// below `-margin`.
fn near<V: Vector>(body: &Shape<V>, at: V, collider: &Collider<V>, margin: f32) -> bool {
    let (reach, half) = (body.half_extents(), collider.shape.half_extents());
    (0..V::AXES)
        .all(|axis| (at[axis] - collider.position[axis]).abs() < reach[axis] + half[axis] + margin)
}

// ------------------------------------------------------- top-down 2D games

/// Defect: a round body that catches on an outside corner as a box does, or
/// loses speed along the wall after it. Oracle: the requirement. Top-down,
/// a wall fills x 0..4, y 0..2; a ball of radius 0.5 whose centre is 0.3
/// below the wall's bottom face moves right at 0.1 a step. It meets the
/// corner (0, 0), goes around it without stopping, never overlaps the wall
/// (its centre stays a radius from the box), and, once under the wall, keeps
/// moving right at its full 0.1 a step along the face. A box of the same
/// size stops at the wall's left face: the oracle tells them apart.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_goes_around_an_outside_corner_and_keeps_its_speed() {
    let wall = Collider::new(1, cuboid(v2(2.0, 1.0)), v2(2.0, 1.0));
    let world = [wall];
    let config = config(Vec2::Y);
    let step = v2(0.1, 0.0);
    let body = ball(0.5);
    let mut at = v2(-3.0, -0.3);
    let mut under = 0;
    for _ in 0..60 {
        let (moved, _) = slide(&body, at, step, &world[..], ALL_LAYERS, &config).unwrap();
        assert!(length(moved) > 0.0, "stopped at {at}");
        let before = at;
        at += moved;
        assert!(
            to_box(at, wall.position, v2(2.0, 1.0)) >= 0.5,
            "inside the wall at {at}"
        );
        if before.x > 0.0 {
            // Past the corner, under the wall: the whole step, along the face.
            assert!((moved.x - 0.1).abs() < 1e-6 && moved.y == 0.0, "{moved}");
            under += 1;
        }
    }
    assert!(under > 20, "the ball reached the face for {under} steps");
    let mut boxed = v2(-3.0, -0.3);
    for _ in 0..60 {
        let half = cuboid(v2(0.5, 0.5));
        boxed += slide(&half, boxed, step, &world[..], ALL_LAYERS, &config)
            .unwrap()
            .0;
    }
    assert!(boxed.x < -0.5, "a box catches on the face: {boxed}");
}

/// Defect: a round body that catches on the seams between flush tiles.
/// Oracle: the requirement, and a single long box as the reference. A ball
/// pressed into a floor while it moves along it travels the same distance
/// across a row of one-unit tiles (a set, and a cell world) as across one
/// long box, its full requested distance across, every step.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_crosses_flush_tiles_as_it_crosses_one_long_box() {
    let config = config(Vec2::Y);
    let long = [Collider::new(1, cuboid(v2(15.0, 0.5)), v2(15.0, -0.5))];
    let mut tiles = ColliderSet::new(1.0);
    for x in 0..30u8 {
        tiles.insert(Collider::new(
            u64::from(x) + 10,
            cuboid(Vec2::splat(0.5)),
            v2(f32::from(x) + 0.5, -0.5),
        ));
    }
    let cells = CellWorld::new(1.0, |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
        if cell.y == -1 && (0..30).contains(&cell.x) {
            out.push(Collider::new(7, cuboid(Vec2::splat(0.5)), Vec2::splat(0.5)));
        }
    });
    let walk = |world: &dyn World<Vec2>| {
        let mut at = v2(1.0, 0.5 + SKIN);
        for _ in 0..200 {
            let (moved, _) =
                slide(&ball(0.5), at, v2(0.13, -0.02), world, ALL_LAYERS, &config).unwrap();
            assert!((moved.x - 0.13).abs() < 1e-5, "caught at {at}: {moved}");
            at += moved;
        }
        at
    };
    let along = walk(&&long[..]);
    for world in [&tiles as &dyn World<Vec2>, &cells] {
        let end = walk(world);
        assert!((end.x - along.x).abs() < 1e-4, "{end} against {along}");
    }
}

/// The same in 3D, through a cell world of voxels: a ball pressed into the
/// ground moves diagonally across many cell seams at its full speed across,
/// as over one long slab.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_crosses_voxel_seams_as_it_crosses_one_slab() {
    let config = config(Vec3::Y);
    let slab = [Collider::new(
        1,
        cuboid(v3(30.0, 0.5, 30.0)),
        v3(0.0, -0.5, 0.0),
    )];
    let cells = CellWorld::new(1.0, |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
        if cell.y == -1 && cell.x.abs() < 30 && cell.z.abs() < 30 {
            out.push(Collider::new(7, cuboid(Vec3::splat(0.5)), Vec3::splat(0.5)));
        }
    });
    let walk = |world: &dyn World<Vec3>| {
        let mut at = v3(0.2, 0.4 + SKIN, 0.3);
        for _ in 0..150 {
            let step = v3(0.11, -0.02, 0.07);
            let (moved, _) = slide(&ball(0.4), at, step, world, ALL_LAYERS, &config).unwrap();
            assert!(
                (moved.x - 0.11).abs() < 1e-5 && (moved.z - 0.07).abs() < 1e-5,
                "{moved}"
            );
            at += moved;
        }
        at
    };
    let along = walk(&&slab[..]);
    let across = walk(&cells);
    assert!(length(across - along) < 1e-4, "{across} against {along}");
}

// ------------------------------------------------------ one-way platforms

/// Whether a body `gap` above what it stands on rests there: a landing
/// leaves the skin (and, through the kernel, up to a sixty-fourth of it
/// more), and a later step whose fall is shorter than the gap closes some
/// of it without touching, as for a box.
fn rests(gap: f32) -> bool {
    (0.0..=SKIN * (1.0 + 1.0 / 64.0) + 1e-5).contains(&gap)
}

const DT: f32 = 1.0 / 60.0;
const GRAVITY: f32 = 20.0;

/// A one-way platform 4 wide and a quarter thick, its top at y = 0, and the
/// ground far below.
fn platform_world() -> [Collider<Vec2>; 2] {
    [
        Collider::new(1, cuboid(v2(2.0, 0.125)), v2(0.0, -0.125)).with_kind(ColliderKind::OneWay),
        Collider::new(2, cuboid(v2(20.0, 0.5)), v2(0.0, -10.5)),
    ]
}

/// Falls a ball of radius 0.5 from `start` under gravity for two seconds,
/// asserting every step that it is never inside the platform by more than
/// the skin (the one-way rule's margin), and returns where it ends and
/// whether it stands on the platform.
fn fall(start: Vec2, rising: f32) -> (Vec2, bool) {
    let world = platform_world();
    let config = config(Vec2::Y);
    let mut state = MoverState {
        position: start,
        velocity: v2(0.0, rising),
        floor: None,
    };
    let mut on_platform = false;
    for _ in 0..120 {
        state.velocity.y -= GRAVITY * DT;
        move_and_slide(
            &mut state,
            &ball(0.5),
            DT,
            &world[..],
            &config,
            &MoverOptions::default(),
        )
        .unwrap();
        on_platform = state.floor.is_some_and(|floor| floor.id == 1);
        let falling_into = state.velocity.y <= 0.0 && state.position.y > 0.0;
        if falling_into {
            let into = 0.5 - to_box(state.position, world[0].position, v2(2.0, 0.125));
            assert!(
                into <= SKIN + 1e-5,
                "{into} into the platform at {}",
                state.position
            );
        }
    }
    (state.position, on_platform)
}

/// Defect: a one-way platform that a round body passes through where it
/// meets the platform's edge, because the kernel's normal there leans off
/// the floor, or that stops a body rising through it. Oracle: the rule's
/// meaning (a one-way platform blocks motion into its top face, edges
/// included, and nothing else) and geometry. A ball of radius 0.5 dropped
/// over the platform lands and rests on it ([`rests`]). One whose centre is
/// 0.2 beyond the platform's end (at x = 2) meets its corner along the
/// normal (0.4, 0.92), within the floor threshold, and stays on it, its
/// centre 0.46 above the top. One 0.4 beyond meets the corner along
/// (0.8, 0.6), no floor: it is turned off the edge as a solid box would
/// turn it, never passing into the platform, and falls past it to the
/// ground. One thrown up through the platform from below passes it, then
/// lands on it.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_lands_on_a_one_way_platform_edges_included_and_rises_through_it() {
    let (middle, on) = fall(v2(0.5, 3.0), 0.0);
    assert!(on && rests(middle.y - 0.5), "{middle}");
    let (near_edge, on) = fall(v2(2.2, 3.0), 0.0);
    let corner_height = (0.25f32 - 0.04).sqrt();
    assert!(on && rests(near_edge.y - corner_height), "{near_edge}");
    let (off_edge, on) = fall(v2(2.4, 3.0), 0.0);
    assert!(!on && off_edge.y < -9.0 && off_edge.x > 2.4, "{off_edge}");
    let (thrown, on) = fall(v2(0.5, -1.0), 10.0);
    assert!(on && rests(thrown.y - 0.5), "{thrown}");
}

// ----------------------------------------------------- a capsule walker

/// A character capsule 0.6 wide and 1.8 tall: its centre is 0.9 above its
/// lowest point.
fn walker() -> Shape<Vec3> {
    capsule(v3(0.0, 0.6, 0.0), 0.3)
}

/// A voxel level answered cell by cell: `rule` says what a cell holds, in
/// cell-local units.
fn voxels(
    rule: impl Fn(IVec3) -> Option<Collider<Vec3>>,
) -> CellWorld<Vec3, impl Fn(IVec3, &mut CellColliders<'_, Vec3>)> {
    CellWorld::new(
        1.0,
        move |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
            if let Some(collider) = rule(cell) {
                out.push(collider);
            }
        },
    )
}

fn block(id: u64) -> Collider<Vec3> {
    Collider::new(id, cuboid(Vec3::splat(0.5)), Vec3::splat(0.5))
}

/// The ground: the cells at y = -1, so its top is y = 0.
fn ground(cell: IVec3) -> bool {
    cell.y == -1 && cell.x.abs() <= 20 && cell.z.abs() <= 20
}

/// A capsule walker standing on the ground at (x, z).
fn standing(x: f32, z: f32) -> MoverState<Vec3> {
    MoverState {
        position: v3(x, 0.9 + SKIN, z),
        velocity: Vec3::ZERO,
        floor: Some(sgl_physics::Floor {
            normal: Vec3::Y,
            id: 0,
        }),
    }
}

/// One step of the capsule walker holding `walk` across, as a game runs it:
/// gravity, then the mover.
fn stride<W: World<Vec3> + ?Sized>(
    state: &mut MoverState<Vec3>,
    world: &W,
    walk: Vec3,
    options: &MoverOptions,
) -> sgl_physics::Contacts<Vec3> {
    state.velocity.x = walk.x;
    state.velocity.z = walk.z;
    state.velocity.y -= GRAVITY * DT;
    move_and_slide(state, &walker(), DT, world, &config(Vec3::Y), options).unwrap()
}

/// Defect: a capsule that does not land, or lands inside the ground.
/// Oracle: the requirement (a landing zeroes the fall and sets the floor)
/// and geometry (it rests on the ground's top, y = 0).
#[wasm_bindgen_test(unsupported = test)]
fn a_capsule_walker_lands() {
    let world = voxels(|cell| ground(cell).then(|| block(1)));
    let mut state = MoverState {
        position: v3(0.5, 3.0, 0.5),
        ..MoverState::default()
    };
    for _ in 0..90 {
        stride(&mut state, &world, Vec3::ZERO, &MoverOptions::default());
    }
    assert!(
        state.floor.is_some() && state.velocity.y == 0.0,
        "{state:?}"
    );
    assert!(rests(state.position.y - 0.9), "{state:?}");
}

/// Defect: a capsule slowed by a ramp, or that leaves the ground walking
/// down one. Oracle: the floor rule (a walkable slope keeps a body's motion
/// across `up` whole) and the snap (a walking body stays on a ramp it walks
/// down). The ground rises along a 45 degree ramp at x = 3..4 to a plateau
/// one cell high and falls along another at x = 8..9; walking +x at 4 a
/// second, the capsule covers exactly 4 / 60 across every step and stands
/// on a floor at every step, up, along and down.
#[wasm_bindgen_test(unsupported = test)]
fn a_capsule_walker_crosses_ramps_at_its_flat_ground_speed() {
    let world = voxels(|cell| {
        let (x, y) = (cell.x, cell.y);
        if cell.z.abs() > 3 {
            return None;
        }
        let up = block(2).with_cut(Cut::new(v3(-1.0, 1.0, 0.0), 0.0));
        let down = block(3).with_cut(Cut::new(v3(1.0, 1.0, 0.0), 0.0));
        match (x, y) {
            (_, -1) => Some(block(1)),
            (3, 0) => Some(up),
            (4..=7, 0) => Some(block(4)),
            (8, 0) => Some(down),
            _ => None,
        }
    });
    let mut state = standing(0.5, 0.5);
    for _ in 0..150 {
        let before = state.position;
        stride(
            &mut state,
            &world,
            v3(4.0, 0.0, 0.0),
            &MoverOptions::default(),
        );
        let across = state.position.x - before.x;
        assert!((across - 4.0 / 60.0).abs() < 1e-5, "{across} at {before}");
        assert!(
            state.floor.is_some(),
            "off the ground at {}",
            state.position
        );
    }
    assert!(state.position.x > 10.0, "{state:?}");
    assert!(rests(state.position.y - 0.9), "{state:?}");
}

/// Defect: a capsule that cannot climb a step its step height allows, as a
/// round body's settle lands on the step's edge, whose normal leans off the
/// floor. Oracle: the requirement (a walking body stopped by a wall climbs
/// a step no taller than `step_height` and ends on its floor) and geometry.
/// A step 0.3 high starts at x = 3; walking +x at 4 a second with a step
/// height of 0.35, the capsule climbs it and stands on its top, y = 0.3,
/// and without a step height it stays below it.
#[wasm_bindgen_test(unsupported = test)]
fn a_capsule_walker_climbs_a_step_of_its_step_height() {
    let step = Collider::new(2, cuboid(v3(5.0, 0.15, 5.0)), v3(8.0, 0.15, 0.0));
    let floor = Collider::new(1, cuboid(v3(20.0, 0.5, 20.0)), v3(0.0, -0.5, 0.0));
    let world = [floor, step];
    let climbing = MoverOptions {
        step_height: 0.35,
        ..MoverOptions::default()
    };
    for (options, climbs) in [(climbing, true), (MoverOptions::default(), false)] {
        let mut state = standing(1.0, 0.0);
        let mut stepped = false;
        for _ in 0..90 {
            stepped |= stride(&mut state, &world[..], v3(4.0, 0.0, 0.0), &options).stepped;
        }
        if climbs {
            assert!(stepped && state.position.x > 4.0, "{state:?}");
            assert!(
                rests(state.position.y - 1.2) && state.floor.is_some(),
                "{state:?}"
            );
        } else {
            assert!(!stepped && state.position.x < 3.0, "{state:?}");
        }
    }
}

/// Defect: a guarded capsule that walks off a ledge. Oracle: the
/// requirement (a walking body guarded by `ledge_drop` stops over the edge
/// of a deeper drop) and geometry. The ground ends at x = 5 over a drop of
/// two cells; guarded with a drop of 0.5, the capsule walking +x for two
/// seconds rests on the ground at every step (its rounded bottom on the
/// ground's top or its edge), its centre never more than its radius past
/// the edge.
#[wasm_bindgen_test(unsupported = test)]
fn a_guarded_capsule_walker_stops_at_a_ledge() {
    let world = voxels(|cell| {
        (cell.x < 5 && ground(cell) || cell.y == -3 && cell.x.abs() <= 20).then(|| block(1))
    });
    let options = MoverOptions {
        ledge_drop: Some(0.5),
        ..MoverOptions::default()
    };
    let mut state = standing(2.5, 0.5);
    let mut guarded = false;
    for _ in 0..120 {
        guarded |= stride(&mut state, &world, v3(4.0, 0.0, 0.0), &options).ledge_guarded;
        // The lower end of the capsule's segment, against the ground's
        // slab, x below 5 and y below 0.
        let foot = state.position - v3(0.0, 0.6, 0.0);
        // On an edge the skin back-off leaves at least the skin; it does
        // not fall.
        let gap = to_box(foot, v3(-7.5, -0.5, 0.0), v3(12.5, 0.5, 21.0)) - 0.3;
        assert!(
            state.floor.is_some() && (0.0..2.0 * SKIN).contains(&gap),
            "{gap}: {state:?}"
        );
        assert!(state.position.x <= 5.3, "{state:?}");
    }
    assert!(guarded && state.position.x > 4.9, "{state:?}");
}

/// Defect: a capsule left inside a crate dropped on it, or pushed through
/// it. Oracle: the requirement (a body that starts inside a solid is pushed
/// out the shortest way) and the shapes' definitions: afterwards no point
/// of the capsule's segment is within its radius of the crate.
#[wasm_bindgen_test(unsupported = test)]
fn a_capsule_walker_is_pushed_out_of_a_crate_placed_on_it() {
    let floor = Collider::new(1, cuboid(v3(20.0, 0.5, 20.0)), v3(0.0, -0.5, 0.0));
    let crate_half = v3(0.2, 0.2, 0.2);
    let crate_at = v3(0.75, 1.4, 0.5);
    let world = [floor, Collider::new(2, cuboid(crate_half), crate_at)];
    let mut state = standing(0.5, 0.5);
    let into = depth(&walker(), state.position, |p| {
        to_box(p, crate_at, crate_half)
    });
    assert!(into > 0.1, "the crate is not inside the walker: {into}");
    let contacts = stride(&mut state, &world[..], Vec3::ZERO, &MoverOptions::default());
    assert!(contacts.pushed_out && !contacts.stuck, "{contacts:?}");
    let into = depth(&walker(), state.position, |p| {
        to_box(p, crate_at, crate_half)
    });
    assert!(
        into <= 0.0,
        "still {into} inside the crate at {}",
        state.position
    );
}

// ------------------------------------------------- beams and projectiles

/// Defect: a beam or a swept ball that stops at a sensor, at a collider on
/// a layer it does not mask, or at a farther collider than the nearest.
/// Oracle: the requirement (a cast meets the first non-sensor collider on
/// its layers) and geometry. Along +x from the origin: a sensor ball at
/// x = 2, a capsule on layer 2 at x = 3, a turned square on layer 1 whose
/// corner is at x = 5, and a box on layer 1 at x = 6. A beam and a ball of
/// radius 0.25 masking layer 1 meet the square; masking layer 2, the
/// capsule; masking both, the capsule.
#[wasm_bindgen_test(unsupported = test)]
fn a_beam_and_a_swept_ball_stop_at_the_first_thing_on_their_layers() {
    let world = [
        Collider::new(1, ball(0.5), v2(2.0, 0.0)).with_kind(ColliderKind::Sensor),
        Collider::new(2, capsule(v2(0.0, 1.0), 0.25), v2(3.0, 0.0)).with_layers(0b10),
        moved(diamond2(3), v2(5.0 + SQRT_2, 0.0)).with_layers(0b01),
        Collider::new(4, cuboid(v2(0.5, 2.0)), v2(6.5, 0.0)).with_layers(0b01),
    ];
    let motion = v2(10.0, 0.0);
    for (mask, id) in [(0b01, 3), (0b10, 2), (0b11, 2)] {
        let beam = ray(Vec2::ZERO, motion, &world[..], mask)
            .unwrap()
            .map(|hit| hit.id);
        let swept = cast(&ball(0.25), Vec2::ZERO, motion, &world[..], mask).unwrap();
        assert_eq!(beam, Some(id), "beam, mask {mask:b}");
        assert_eq!(swept.map(|hit| hit.id), Some(id), "ball, mask {mask:b}");
    }
}

/// Defect: a projectile that, stepped to each hit and cast on, passes
/// through what it hit (a cast from a contact a slop away that misses the
/// surface it rests at). Oracle: geometry. A ball of radius 0.2 fired at a
/// wall at an angle is stepped to each hit and cast on with what is left of
/// its motion, again and again; it never ends past the wall's face, in 2D
/// and 3D, against a box, a capsule and a turned square or cube.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_projectile_stepped_to_each_hit_never_passes_a_wall() {
    let walls2 = [
        Collider::new(1, cuboid(v2(1.0, 10.0)), v2(6.0, 0.0)),
        Collider::new(2, capsule(v2(0.0, 10.0), 1.0), v2(6.0, 0.0)),
        moved(diamond2(3), v2(5.0 + SQRT_2, 0.3)),
    ];
    for wall in walls2 {
        let world = [wall];
        let (mut at, mut left) = (Vec2::ZERO, v2(12.0, 1.0));
        for _ in 0..20 {
            let Some(hit) = cast(&ball(0.2), at, left, &world[..], ALL_LAYERS).unwrap() else {
                break;
            };
            at += left * hit.t;
            left *= 1.0 - hit.t;
            assert!(to_collider2(at, &wall) >= 0.2, "{wall:?}: passed at {at}");
        }
        assert!(at.x < 5.0, "{wall:?}: {at}");
    }
    let turn = Quat::from_rotation_z(FRAC_PI_4);
    let walls3 = [
        Collider::new(1, cuboid(v3(1.0, 10.0, 10.0)), v3(6.0, 0.0, 0.0)),
        Collider::new(2, capsule(v3(0.0, 10.0, 0.0), 1.0), v3(6.0, 0.0, 0.0)),
        Collider::new(3, Shape::Convex(turned_cube(turn)), v3(5.0, 0.1, 0.0)),
    ];
    for wall in walls3 {
        let world = [wall];
        let (mut at, mut left) = (Vec3::ZERO, v3(12.0, 0.5, 0.25));
        for _ in 0..20 {
            let Some(hit) = cast(&ball(0.2), at, left, &world[..], ALL_LAYERS).unwrap() else {
                break;
            };
            at += left * hit.t;
            left *= 1.0 - hit.t;
            assert!(
                to_collider3(at, &wall, turn) >= 0.2,
                "{wall:?}: passed at {at}"
            );
        }
        assert!(at.x < 5.0, "{wall:?}: {at}");
    }
}

// ------------------------------------------------- random round bodies

/// A uniform draw in `[low, high)` from 24 bits of the stream.
fn draw(rng: &mut SplitMix64, low: f32, high: f32) -> f32 {
    low + (high - low) * ((rng.next_u64() >> 40) as f32 / 16_777_216.0)
}

/// A uniform whole number below `n`.
fn pick(rng: &mut SplitMix64, n: u64) -> u64 {
    rng.next_u64() % n
}

/// A random round body: a ball, or a capsule turned any way.
fn round_body<V: Vector>(rng: &mut SplitMix64) -> Shape<V> {
    if draw(rng, 0.0, 1.0) < 0.5 {
        return ball(draw(rng, 0.15, 0.45));
    }
    let mut half_axis = V::ZERO;
    for axis in 0..V::AXES {
        half_axis[axis] = draw(rng, -0.3, 0.3);
    }
    capsule(half_axis, draw(rng, 0.1, 0.3))
}

/// What one occupied cell holds, in cell-local units (the cell runs from 0
/// to 1): a full block, a bottom slab, a ramp, a ball, a capsule or a
/// convex shape, each inside its cell.
fn cell_shape<V: Vector>(rng: &mut SplitMix64, id: u64, convex: Convex<V>) -> Collider<V> {
    let centre = V::ZERO + {
        let mut c = V::ZERO;
        for axis in 0..V::AXES {
            c[axis] = 0.5;
        }
        c
    };
    let mut slab_half = centre;
    slab_half[1] = 0.25;
    let mut slab_at = centre;
    slab_at[1] = 0.25;
    let mut normal = V::ZERO;
    normal[0] = if draw(rng, 0.0, 1.0) < 0.5 { -1.0 } else { 1.0 };
    normal[1] = draw(rng, 0.5, 2.0);
    match pick(rng, 6) {
        0 => Collider::new(id, cuboid(centre), centre),
        1 => Collider::new(id, cuboid(slab_half), slab_at),
        2 => Collider::new(id, cuboid(centre), centre).with_cut(Cut::new(normal, 0.0)),
        3 => Collider::new(id, ball(draw(rng, 0.2, 0.5)), centre),
        4 => {
            let mut half_axis = V::ZERO;
            for axis in 0..V::AXES {
                half_axis[axis] = draw(rng, -0.2, 0.2);
            }
            // At most 0.2 * sqrt(3) + 0.15 from the centre: inside.
            Collider::new(id, capsule(half_axis, 0.15), centre)
        }
        _ => Collider::new(id, Shape::Convex(convex), centre),
    }
}

/// Defect: a slide that leaves a round body inside a collider (a kernel cast
/// past a contact, a skin applied the wrong way, a world that leaves a
/// collider out), or a result that depends on the world. Oracle: the
/// shapes' own definitions, sampled with no kernel maths, and the same
/// level as a slice, a set and a cell world, which must agree bit for bit.
/// Random balls and capsules start clear of a random level of boxes, slabs,
/// ramps, balls, capsules and convex shapes and move by random deltas; none
/// ends inside anything by more than a hundredth of the skin.
#[wasm_bindgen_test(unsupported = test)]
fn a_round_body_never_ends_inside_a_collider_through_any_world_in_2d() {
    let mut rng = SplitMix64::new(0x0919_0002);
    let config = config(Vec2::Y);
    let (mut ran, mut touched) = (0, 0);
    for _ in 0..6 {
        let mut level = Vec::new();
        for y in 0..8i32 {
            for x in 0..10 {
                if draw(&mut rng, 0.0, 1.0) < 0.3 {
                    let turn = draw(&mut rng, 0.0, 6.0);
                    let triangle = [v2(-0.4, -0.3), v2(0.4, -0.3), v2(0.0, 0.4)];
                    let convex = Convex::polygon(&triangle, turn.cos(), turn.sin());
                    let id = u64::from((y * 10 + x).unsigned_abs());
                    level.push((IVec2::new(x, y), cell_shape(&mut rng, id, convex)));
                }
            }
        }
        let slice: Vec<Collider<Vec2>> = level
            .iter()
            .map(|(cell, local)| Collider {
                position: cell.as_vec2() + local.position,
                ..*local
            })
            .collect();
        let mut set = ColliderSet::new(2.0);
        for collider in &slice {
            set.insert(*collider);
        }
        let cells = CellWorld::new(1.0, |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
            for (at, local) in &level {
                if *at == cell {
                    out.push(*local);
                }
            }
        });
        // Colliders more than 0.1 from the body (ten skins) cannot change
        // either comparison below, so only the near ones are measured.
        let into = |body: &Shape<Vec2>, at: Vec2| {
            slice
                .iter()
                .filter(|c| near(body, at, c, 0.1))
                .map(|c| depth(body, at, |p| to_collider2(p, c)))
                .fold(-0.1, f32::max)
        };
        for _ in 0..40 {
            let body = round_body(&mut rng);
            let start = v2(draw(&mut rng, 0.0, 10.0), draw(&mut rng, 0.0, 8.0));
            let delta = v2(draw(&mut rng, -3.0, 3.0), draw(&mut rng, -3.0, 3.0));
            if into(&body, start) > -SKIN {
                continue;
            }
            ran += 1;
            let worlds: [&dyn World<Vec2>; 3] = [&&slice[..], &set, &cells];
            let digests = worlds.map(|world| {
                let (moved, contacts) =
                    slide(&body, start, delta, world, ALL_LAYERS, &config).unwrap();
                let end = start + moved;
                let deepest = into(&body, end);
                assert!(
                    deepest <= SKIN / 100.0,
                    "{body:?} from {start} by {delta}: {deepest} in"
                );
                let mut hasher = StateHasher::new();
                hasher.u32(moved.x.to_bits());
                hasher.u32(moved.y.to_bits());
                touched += usize::from(contacts.floor || contacts.wall || contacts.ceiling);
                contacts.canonical_write(&mut hasher);
                hasher.finish().to_hex()
            });
            assert!(
                digests.iter().all(|d| *d == digests[0]),
                "{body:?} from {start} by {delta}"
            );
        }
    }
    // Each world counts its touches, so three per case that touched.
    assert!(
        ran >= 140 && touched >= 3 * 60,
        "{ran} cases, {touched} touches"
    );
}

/// The same in 3D: balls and capsules through levels of blocks, slabs,
/// ramps, balls, capsules and turned cubes.
#[wasm_bindgen_test(unsupported = test)]
fn a_round_body_never_ends_inside_a_collider_through_any_world_in_3d() {
    let mut rng = SplitMix64::new(0x0919_0003);
    let config = config(Vec3::Y);
    let turn = Quat::from_rotation_y(0.5) * Quat::from_rotation_x(0.3);
    let (mut ran, mut touched) = (0, 0);
    for _ in 0..7 {
        let mut level = Vec::new();
        for y in 0..4 {
            for z in 0..6i32 {
                for x in 0..6 {
                    if draw(&mut rng, 0.0, 1.0) < 0.25 {
                        let id = u64::from(((y * 6 + z) * 6 + x).unsigned_abs());
                        let local = cell_shape(&mut rng, id, turned_cube(turn));
                        level.push((IVec3::new(x, y, z), local));
                    }
                }
            }
        }
        let slice: Vec<Collider<Vec3>> = level
            .iter()
            .map(|(cell, local)| Collider {
                position: cell.as_vec3() + local.position,
                ..*local
            })
            .collect();
        let mut set = ColliderSet::new(2.0);
        for collider in &slice {
            set.insert(*collider);
        }
        let cells = CellWorld::new(1.0, |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
            for (at, local) in &level {
                if *at == cell {
                    out.push(*local);
                }
            }
        });
        // As in 2D: only colliders within 0.1 of the body are measured.
        let into = |body: &Shape<Vec3>, at: Vec3| {
            slice
                .iter()
                .filter(|c| near(body, at, c, 0.1))
                .map(|c| depth(body, at, |p| to_collider3(p, c, turn)))
                .fold(-0.1, f32::max)
        };
        for _ in 0..40 {
            let body = round_body(&mut rng);
            let start = v3(
                draw(&mut rng, 0.0, 6.0),
                draw(&mut rng, 0.0, 4.0),
                draw(&mut rng, 0.0, 6.0),
            );
            let delta = v3(
                draw(&mut rng, -2.0, 2.0),
                draw(&mut rng, -2.0, 2.0),
                draw(&mut rng, -2.0, 2.0),
            );
            if into(&body, start) > -SKIN {
                continue;
            }
            ran += 1;
            let worlds: [&dyn World<Vec3>; 3] = [&&slice[..], &set, &cells];
            let digests = worlds.map(|world| {
                let (moved, contacts) =
                    slide(&body, start, delta, world, ALL_LAYERS, &config).unwrap();
                let end = start + moved;
                let deepest = into(&body, end);
                assert!(
                    deepest <= SKIN / 100.0,
                    "{body:?} from {start} by {delta}: {deepest} in"
                );
                let mut hasher = StateHasher::new();
                for axis in 0..3 {
                    hasher.u32(moved[axis].to_bits());
                }
                touched += usize::from(contacts.floor || contacts.wall || contacts.ceiling);
                contacts.canonical_write(&mut hasher);
                hasher.finish().to_hex()
            });
            assert!(
                digests.iter().all(|d| *d == digests[0]),
                "{body:?} from {start} by {delta}"
            );
        }
    }
    // Each world counts its touches, so three per case that touched.
    assert!(
        ran >= 140 && touched >= 3 * 60,
        "{ran} cases, {touched} touches"
    );
}

// -------------------------------------------- bodies that start overlapping

/// Defect: a slide that moves a round body deeper into a collider it starts
/// inside, or holds one that could leave. Oracle: the requirement (`slide`
/// never pushes out; a body that starts overlapping may move where a way
/// out is known and is otherwise not moved deeper) and geometry. A ball of
/// radius 0.5 whose centre is 0.3 above a box's top face overlaps it by
/// 0.2: moving down it does not move; moving up it leaves; moving along
/// the face it slides, no deeper. A ball whose centre is inside the box has
/// no way out the kernel knows: it does not move at all, whichever way it
/// is pushed.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_that_starts_inside_a_box_is_never_moved_deeper() {
    let floor = [Collider::new(1, cuboid(v2(5.0, 1.0)), v2(0.0, -1.0))];
    let config = config(Vec2::Y);
    let move_by = |at: Vec2, delta: Vec2| {
        slide(&ball(0.5), at, delta, &floor[..], ALL_LAYERS, &config)
            .unwrap()
            .0
    };
    let shallow = v2(0.0, 0.3);
    assert_eq!(move_by(shallow, v2(0.0, -1.0)), Vec2::ZERO);
    assert_eq!(move_by(shallow, v2(0.0, 1.0)), v2(0.0, 1.0));
    let along = move_by(shallow, v2(1.0, -0.5));
    assert!((along.x - 1.0).abs() < 1e-6 && along.y >= 0.0, "{along}");
    let deep = v2(0.0, -0.4);
    for delta in [v2(0.0, -1.0), v2(0.0, 1.0), v2(1.0, 0.0), v2(-0.3, 0.7)] {
        assert_eq!(move_by(deep, delta), Vec2::ZERO, "pushed by {delta}");
    }
}

/// Defect: a cast that passes a collider a round body touches to within its
/// slop, or stops at one it starts deep inside. Oracle: the requirement (a
/// body touching a face, or inside it by no more than the contact
/// tolerance, meets it at once moving in; one deeper passes it and meets
/// what lies beyond) and geometry. A ball of radius 0.5 a thousandth into a
/// wall (its slop is 0.005) moving into it meets it at `t = 0`; moving
/// along it, not at all. One half its radius in passes it and meets the
/// next wall, 3 further on.
#[wasm_bindgen_test(unsupported = test)]
fn a_cast_meets_what_a_ball_touches_and_passes_what_it_starts_in() {
    let walls = [
        Collider::new(1, cuboid(v2(0.5, 5.0)), v2(1.0, 0.0)),
        Collider::new(2, cuboid(v2(0.5, 5.0)), v2(4.5, 0.0)),
    ];
    let touching = v2(0.001, 0.0);
    let into = cast(&ball(0.5), touching, v2(2.0, 0.0), &walls[..], ALL_LAYERS).unwrap();
    assert_eq!(into.map(|hit| (hit.t, hit.id)), Some((0.0, 1)));
    let along = cast(&ball(0.5), touching, v2(0.0, 2.0), &walls[..], ALL_LAYERS).unwrap();
    assert_eq!(along, None);
    let deep = cast(
        &ball(0.5),
        v2(0.25, 0.0),
        v2(4.0, 0.0),
        &walls[..],
        ALL_LAYERS,
    )
    .unwrap();
    assert_eq!(deep.map(|hit| hit.id), Some(2));
}

/// Defect: a push-out that cannot free a round body from a collider whose
/// box is far larger than the overlap (a long wall at an angle), because it
/// tries only the faces of that box. Oracle: the requirement (a body that
/// starts inside a solid is pushed out the shortest way, at most its own
/// size per axis) and the shapes' definitions. A ball of radius 0.5 overlaps
/// a 10-long wall turned an eighth of a turn by 0.1: the wall's box reaches
/// 3.9 beyond the ball on each side, out of reach, but the normal between
/// them frees it, and afterwards it overlaps nothing.
#[wasm_bindgen_test(unsupported = test)]
fn a_ball_is_pushed_out_of_a_wall_at_an_angle() {
    let rod = [
        v2(-5.0, -0.25),
        v2(5.0, -0.25),
        v2(5.0, 0.25),
        v2(-5.0, 0.25),
    ];
    let wall = Collider::new(
        1,
        Shape::Convex(Convex::polygon(&rod, FRAC_1_SQRT_2, FRAC_1_SQRT_2)),
        Vec2::ZERO,
    );
    // 0.65 out along the wall's normal (-1, 1) / sqrt(2): the face is 0.25
    // from the axis, so the ball reaches 0.1 into it.
    let normal = v2(-FRAC_1_SQRT_2, FRAC_1_SQRT_2);
    let mut state = MoverState {
        position: normal * 0.65,
        ..MoverState::default()
    };
    let world = [wall];
    let into = depth(&ball(0.5), state.position, |p| to_collider2(p, &wall));
    assert!((into - 0.1).abs() < 1e-4, "{into}");
    let contacts = move_and_slide(
        &mut state,
        &ball(0.5),
        DT,
        &world[..],
        &config(Vec2::Y),
        &MoverOptions::default(),
    )
    .unwrap();
    assert!(contacts.pushed_out && !contacts.stuck, "{contacts:?}");
    let into = depth(&ball(0.5), state.position, |p| to_collider2(p, &wall));
    assert!(into <= 0.0, "{into} at {}", state.position);
}

// ------------------------------------------------ shapes as data

/// Defect: a collider whose numbers describe nothing that a query still
/// meets, or a one-way platform or a cut on a shape that is not a box.
/// Oracle: the requirement (a collider with a negative or non-finite size
/// occupies nothing; a one-way platform is a whole box; only a box takes a
/// cut). A ball of negative radius, a capsule of non-finite axis, a
/// capsule whose negative radius still leaves its box a size, and a ball
/// built as a one-way platform, in the path of a ray, are all passed, and
/// the box behind them is met.
#[wasm_bindgen_test(unsupported = test)]
fn a_round_collider_that_occupies_nothing_is_passed() {
    let one_way_ball = Collider {
        kind: ColliderKind::OneWay,
        ..Collider::new(4, ball(0.5), v2(4.0, 0.0))
    };
    let world = [
        Collider::new(1, ball(-0.5), v2(2.0, 0.0)),
        Collider::new(2, capsule(v2(f32::NAN, 0.0), 0.5), v2(3.0, 0.0)),
        Collider::new(3, capsule(v2(1.0, 1.0), -0.5), v2(3.5, 0.0)),
        one_way_ball,
        Collider::new(5, cuboid(Vec2::splat(0.5)), v2(6.0, 0.0)),
    ];
    let hit = ray(Vec2::ZERO, v2(10.0, 0.0), &world[..], ALL_LAYERS).unwrap();
    assert_eq!(hit.map(|hit| hit.id), Some(5));
    let found = overlaps(&ball(1.0), v2(3.0, 0.0), &world[..], ALL_LAYERS).unwrap();
    assert!(found.is_empty(), "{found:?}");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_one_way_platform_or_a_cut_needs_a_box() {
    let refused = |build: fn() -> Collider<Vec2>| std::panic::catch_unwind(build).is_err();
    assert!(refused(
        || Collider::new(1, ball(1.0), Vec2::ZERO).with_kind(ColliderKind::OneWay)
    ));
    assert!(refused(
        || Collider::new(1, ball(1.0), Vec2::ZERO).with_cut(Cut::new(Vec2::Y, 0.0))
    ));
}

/// Defect: a convex shape that accepts points it cannot be: too many, not
/// finite, or in 2D not a strictly convex counter-clockwise polygon (a
/// clockwise one, a dent, three in a line). Oracle: the requirement.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_convex_shape_refuses_what_it_cannot_be() {
    let square = [v2(0.0, 0.0), v2(1.0, 0.0), v2(1.0, 1.0), v2(0.0, 1.0)];
    let polygon = |points: Vec<Vec2>| {
        std::panic::catch_unwind(move || Convex::polygon(&points, 1.0, 0.0)).is_ok()
    };
    assert!(polygon(square.to_vec()));
    let mut clockwise = square.to_vec();
    clockwise.reverse();
    assert!(!polygon(clockwise));
    assert!(!polygon(vec![
        v2(0.0, 0.0),
        v2(1.0, 0.0),
        v2(0.5, 0.2),
        v2(1.0, 1.0),
        v2(0.0, 1.0)
    ]));
    assert!(!polygon(vec![
        v2(0.0, 0.0),
        v2(1.0, 0.0),
        v2(2.0, 0.0),
        v2(0.0, 1.0)
    ]));
    assert!(!polygon(vec![v2(0.0, 0.0), v2(1.0, 0.0)]));
    assert!(!polygon(vec![
        v2(0.0, 0.0),
        v2(1.0, f32::NAN),
        v2(0.0, 1.0)
    ]));
    let nonagon: Vec<Vec2> = (0u8..9)
        .map(|i| {
            let a = f32::from(i) * 0.698;
            v2(a.cos(), a.sin())
        })
        .collect();
    assert!(!polygon(nonagon));
    let hull = |count: u8| {
        std::panic::catch_unwind(move || {
            let points: Vec<Vec3> = (0..count).map(|i| Vec3::splat(f32::from(i))).collect();
            Convex::hull(&points, Quat::IDENTITY)
        })
        .is_ok()
    };
    assert!(hull(16) && !hull(17) && !hull(0));
    assert!(std::panic::catch_unwind(|| Convex::polygon(&square, 0.0, 0.0)).is_err());
}

/// Defect: a round or convex shape that does not survive the wire to the
/// same bits, or a polygon from the wire that is not checked. Oracle: a
/// postcard round trip (the decoded value encodes to the same bytes and
/// writes the same digest) and the requirement (a deserialised convex
/// shape is validated as a built one is).
#[wasm_bindgen_test(unsupported = test)]
fn round_and_convex_shapes_cross_the_wire_unchanged_and_are_checked() {
    let shapes = [
        ball(0.75),
        capsule(v2(0.25, -1.0), 0.5),
        Shape::Convex(Convex::polygon(
            &[v2(-1.0, -1.0), v2(1.0, -1.0), v2(0.0, 1.0)],
            0.6,
            0.8,
        )),
    ];
    for shape in shapes {
        let bytes = postcard::to_allocvec(&shape).unwrap();
        let back: Shape<Vec2> = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(postcard::to_allocvec(&back).unwrap(), bytes);
        let digest = |s: &Shape<Vec2>| {
            let mut hasher = StateHasher::new();
            s.canonical_write(&mut hasher);
            hasher.finish().to_hex()
        };
        assert_eq!(digest(&back), digest(&shape));
    }
    // A convex shape travels as its turned points; the same three
    // clockwise are refused.
    let clockwise = [v2(-1.0, -1.0), v2(0.0, 1.0), v2(1.0, -1.0)];
    let bytes = postcard::to_allocvec(&clockwise.to_vec()).unwrap();
    assert!(postcard::from_bytes::<Convex<Vec2>>(&bytes).is_err());
    let too_many = vec![Vec3::ONE; 17];
    let bytes = postcard::to_allocvec(&too_many).unwrap();
    assert!(postcard::from_bytes::<Convex<Vec3>>(&bytes).is_err());
}

/// Defect: a kernel contact made with a collider just beyond the region a
/// world offers, so a slice (which offers everything) and a set (which
/// offers what its cells hold) answer differently. Oracle: the requirement
/// (a result never depends on the world) and the two worlds' answers. A
/// ball of radius 0.5 (slop 0.005) moving right and a little down starts
/// 0.0015 above a slab whose top, at y = -0.5003, lies in the cell below
/// the one its swept box reaches in a set of half-unit cells: within its
/// slop, but out of the set's region. Both worlds let it pass.
#[wasm_bindgen_test(unsupported = test)]
fn a_kernel_contact_never_depends_on_the_world() {
    let slab = Collider::new(1, cuboid(v2(2.0, 0.2)), v2(0.0, -0.7003));
    let slice = [slab];
    let mut set = ColliderSet::new(0.5);
    set.insert(slab);
    let config = config(Vec2::Y);
    let (start, delta) = (v2(0.0, 0.0012), v2(0.3, -0.0002));
    let worlds: [&dyn World<Vec2>; 2] = [&&slice[..], &set];
    for world in worlds {
        let (moved, _) = slide(&ball(0.5), start, delta, world, ALL_LAYERS, &config).unwrap();
        assert_eq!(moved, delta);
        let hit = cast(&ball(0.5), start, delta, world, ALL_LAYERS).unwrap();
        assert_eq!(hit, None);
    }
}
