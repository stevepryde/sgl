//! A second engine as the oracle (#924, item 4): parry (`parry2d`,
//! `parry3d`, Apache-2.0), a collision library that shares no code with
//! the `Box2D` and `Box3D` kernel `sgl-physics` ports. Over a few thousand
//! seeded pairs of balls, capsules, boxes and convex shapes at random
//! poses, in each dimension, the crate's separation must equal parry's and
//! its casts must meet where parry says the shapes touch, within the
//! contact band the crate states. Native only: the comparison needs no
//! browser, and parry is not built for `wasm32` here.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::cast_precision_loss)]

use core::f32::consts::TAU;
use parry2d::query as query2;
use parry2d::shape as shape2;
use parry3d::query as query3;
use parry3d::shape as shape3;

use sgl_core::SplitMix64;
use sgl_core::math::{Quat, Vec2, Vec3};
use sgl_physics::{ALL_LAYERS, Collider, Convex, Shape, Vector, cast, distance};

/// How far the crate's separation may differ from parry's, beyond parry's
/// own precision ([`parry_bound`]). Every query decides at the resolution
/// of its slop, a hundredth of the smaller shape's extent or a quarter of
/// the smaller radius; the smallest shapes here (an extent of 0.2, a
/// radius of 0.1) give a slop of 0.002, and a tenth of that cannot change
/// what any query answers.
const SEPARATION_TOLERANCE: f32 = 2e-4;

/// How far parry's distance between two cores `cores` apart may lie above
/// the true one. Parry's GJK stops once its upper and lower bounds on a
/// distance agree to `sqrt(10 ε)` of it (about 1.1e-3; `eps_rel` in
/// parry's `query/gjk/gjk.rs`) and reports the upper one, between two
/// points of the shapes, so its distance is never below the true one and
/// may be above it by that share. Parry measures here between the cores (a
/// ball's centre, a capsule's segment, a box, a hull), whose radii are
/// subtracted after, because on a round surface, which GJK only
/// approaches, it stops further off still. The first runs found a capsule
/// 3.3317977 from a box by exact geometry and by the crate, 3.3321416 by
/// parry; two capsules 0.9956715 apart, 0.9978549 by parry; and a capsule
/// 5.5105502 from a hull by an exact quadratic program in `f64`, 5.5105476
/// by the crate and 5.51078 by parry between cores.
fn parry_bound(cores: f32) -> f32 {
    (10.0 * f32::EPSILON).sqrt() * cores
}

fn draw(rng: &mut SplitMix64, low: f32, high: f32) -> f32 {
    low + (high - low) * ((rng.next_u64() >> 40) as f32 / 16_777_216.0)
}

/// A uniform whole number below `n`.
fn pick(rng: &mut SplitMix64, n: u64) -> usize {
    usize::try_from(rng.next_u64() % n).expect("small")
}

/// The contract's largest query slop for a pair: a hundredth of the
/// smaller shape's extent (physics.md, "Round and convex shapes").
fn slop_bound<V: Vector>(a: &Shape<V>, b: &Shape<V>) -> f32 {
    let extent = |shape: &Shape<V>| {
        let half = shape.half_extents();
        (0..V::AXES).fold(0.0f32, |most, axis| most.max(half[axis]))
    };
    extent(a).min(extent(b)) / 100.0
}

/// Whether the pair is two boxes, which the crate sweeps in closed form.
fn closed_form<V>(a: &Shape<V>, b: &Shape<V>) -> bool {
    matches!((a, b), (Shape::Box { .. }, Shape::Box { .. }))
}

/// What the comparison found: how many pairs, and the worst disagreement.
#[derive(Default, Debug)]
struct Tally {
    separations: usize,
    worst_separation: f32,
    /// The worst among pairs parry puts less than 0.5 apart, near contact.
    worst_near: f32,
    casts: usize,
    hits: usize,
}

// ------------------------------------------------------------------- 2D

fn shape2d(rng: &mut SplitMix64) -> Shape<Vec2> {
    match pick(rng, 4) {
        0 => Shape::Ball {
            radius: draw(rng, 0.1, 1.0),
        },
        1 => Shape::Capsule {
            half_axis: Vec2::new(draw(rng, -0.8, 0.8), draw(rng, -0.8, 0.8)),
            radius: draw(rng, 0.1, 0.5),
        },
        2 => Shape::Box {
            half: Vec2::new(draw(rng, 0.2, 1.0), draw(rng, 0.2, 1.0)),
        },
        _ => {
            // Vertices on a circle at increasing angles: strictly convex and
            // counter-clockwise, then turned.
            let count = 3 + pick(rng, 6);
            let radius = draw(rng, 0.3, 1.0);
            let mut angles: Vec<f32> = (0..count).map(|_| draw(rng, 0.0, TAU)).collect();
            angles.sort_by(f32::total_cmp);
            angles.dedup_by(|a, b| *a - *b < 0.2);
            if angles.len() < 3 || angles[0] + TAU - angles[angles.len() - 1] < 0.2 {
                angles = vec![0.0, 2.1, 4.2];
            }
            let points: Vec<Vec2> = angles
                .iter()
                .map(|a| Vec2::new(a.cos(), a.sin()) * radius)
                .collect();
            let turn = draw(rng, 0.0, TAU);
            Shape::Convex(Convex::polygon(&points, turn.cos(), turn.sin()))
        }
    }
}

/// The same shape in parry, about its own centre.
fn parry2(shape: &Shape<Vec2>) -> Box<dyn shape2::Shape> {
    match *shape {
        Shape::Ball { radius } => Box::new(shape2::Ball::new(radius)),
        Shape::Capsule { half_axis, radius } => {
            Box::new(shape2::Capsule::new(-half_axis, half_axis, radius))
        }
        Shape::Box { half } => Box::new(shape2::Cuboid::new(half)),
        Shape::Convex(convex) => Box::new(
            shape2::ConvexPolygon::from_convex_polyline(convex.points().to_vec())
                .expect("a convex polygon"),
        ),
    }
}

fn pose2(at: Vec2) -> parry2d::math::Pose {
    parry2d::math::Pose::from_translation(at)
}

/// The shape's core in parry, and its radius.
fn core2(shape: &Shape<Vec2>) -> (Box<dyn shape2::Shape>, f32) {
    match *shape {
        Shape::Ball { radius } => (Box::new(shape2::Ball::new(0.0)), radius),
        Shape::Capsule { half_axis, radius } => (
            Box::new(shape2::Segment::new(-half_axis, half_axis)),
            radius,
        ),
        _ => (parry2(shape), 0.0),
    }
}

/// Parry's distance between `a` at `a_at` and `b` at `b_at` (between their
/// cores, less their radii, and zero when they touch or overlap), and the
/// distance between the cores.
fn apart2(a: &Shape<Vec2>, a_at: Vec2, b: &Shape<Vec2>, b_at: Vec2) -> (f32, f32) {
    let ((a_core, a_radius), (b_core, b_radius)) = (core2(a), core2(b));
    let cores = query2::distance(&pose2(b_at), &*b_core, &pose2(a_at), &*a_core)
        .expect("convex pairs are supported")
        .distance;
    ((cores - a_radius - b_radius).max(0.0), cores)
}

/// Defect: a kernel port that measures or casts wrong (a sign, a solver
/// region, a stale weight), or wiring that hands it the wrong shapes.
/// Oracle: parry. For 2000 random pairs in 2D, the crate's separation is
/// never more than parry's (an upper bound) by [`SEPARATION_TOLERANCE`]
/// and never less by more than that and parry's own precision
/// ([`parry_bound`]). For 2000 random casts that start clear, a cast the
/// crate reports never puts the body past touching (parry finds the shapes
/// apart there, and its time of impact is no earlier) and leaves them no
/// more than 1.25 slops apart, the slop at most a hundredth of the smaller
/// extent; a box against a box touches at parry's time of impact; and when
/// parry finds the shapes touch, the crate reports a hit, and when it does
/// not, the crate reports none or one within that band (a graze).
#[test]
fn the_2d_kernel_agrees_with_parry() {
    let mut rng = SplitMix64::new(0x0919_9200);
    let mut tally = Tally::default();
    for _ in 0..2000 {
        let (body, target) = (shape2d(&mut rng), shape2d(&mut rng));
        let at = Vec2::new(draw(&mut rng, -4.0, 4.0), draw(&mut rng, -4.0, 4.0));
        let collider = Collider::new(1, target, Vec2::new(draw(&mut rng, -4.0, 4.0), 0.5));
        let (body_p, target_p) = (parry2(&body), parry2(&target));

        let ours = distance(&body, at, &collider)
            .expect("sound shapes")
            .distance;
        let (theirs, cores) = apart2(&body, at, &target, collider.position);
        tally.worst_separation = tally.worst_separation.max((ours - theirs).abs());
        if theirs < 0.5 {
            tally.worst_near = tally.worst_near.max((ours - theirs).abs());
        }
        tally.separations += 1;
        assert!(
            ours <= theirs + SEPARATION_TOLERANCE
                && ours >= theirs - SEPARATION_TOLERANCE - parry_bound(cores),
            "{body:?} at {at} from {collider:?}: {ours} against parry's {theirs}"
        );

        // A cast from here toward somewhere near the target.
        let slop = slop_bound(&body, &target);
        if theirs <= 2.0 * slop {
            continue;
        }
        let aim =
            collider.position + Vec2::new(draw(&mut rng, -1.5, 1.5), draw(&mut rng, -1.5, 1.5));
        let motion = (aim - at) * draw(&mut rng, 0.5, 2.0);
        let hit = cast(&body, at, motion, &[collider][..], ALL_LAYERS).unwrap();
        let options = query2::ShapeCastOptions {
            max_time_of_impact: 1.0,
            ..query2::ShapeCastOptions::default()
        };
        let toi = query2::cast_shapes(
            &pose2(collider.position),
            Vec2::ZERO,
            &*target_p,
            &pose2(at),
            motion,
            &*body_p,
            options,
        )
        .expect("convex pairs are supported")
        .map(|hit| hit.time_of_impact);
        tally.casts += 1;
        let gap_at = |t: f32| apart2(&body, at + motion * t, &target, collider.position);
        let touching_at = |t: f32| {
            query2::intersection_test(
                &pose2(collider.position),
                &*target_p,
                &pose2(at + motion * t),
                &*body_p,
            )
            .expect("convex pairs are supported")
            .intersecting
        };
        match (hit, toi) {
            (Some(hit), _) if closed_form(&body, &target) => {
                // A box against a box is the exact closed-form sweep: parry
                // finds them touching at `t` and apart a little before it,
                // where their gap is four tolerances along the normal.
                tally.hits += 1;
                assert!(toi.is_some(), "parry meets what the sweep meets");
                let approach = -motion.dot(hit.normal);
                let before = hit.t - 4.0 * SEPARATION_TOLERANCE / approach;
                assert!(
                    gap_at(hit.t).0 <= SEPARATION_TOLERANCE && gap_at(before).0 > 0.0,
                    "{body:?} from {at} by {motion} at {collider:?}: t {}",
                    hit.t
                );
            }
            (Some(hit), _) => {
                tally.hits += 1;
                let (gap, cores) = gap_at(hit.t);
                let band = 1.25 * slop + SEPARATION_TOLERANCE + parry_bound(cores);
                assert!(
                    !touching_at(hit.t) && gap <= band,
                    "{body:?} from {at} by {motion} at {collider:?}: {gap} apart at t {}",
                    hit.t
                );
                if let Some(toi) = toi {
                    assert!(hit.t <= toi, "t {} after parry's {toi}", hit.t);
                }
            }
            (None, Some(toi)) => {
                panic!("{body:?} from {at} by {motion} at {collider:?}: parry touches at {toi}")
            }
            (None, None) => {}
        }
    }
    assert!(tally.hits > 300, "{tally:?}");
}

// ------------------------------------------------------------------- 3D

fn shape3d(rng: &mut SplitMix64) -> Shape<Vec3> {
    let vector = |rng: &mut SplitMix64, size: f32| {
        Vec3::new(
            draw(rng, -size, size),
            draw(rng, -size, size),
            draw(rng, -size, size),
        )
    };
    match pick(rng, 4) {
        0 => Shape::Ball {
            radius: draw(rng, 0.1, 1.0),
        },
        1 => Shape::Capsule {
            half_axis: vector(rng, 0.8),
            radius: draw(rng, 0.1, 0.5),
        },
        2 => Shape::Box {
            half: Vec3::new(
                draw(rng, 0.2, 1.0),
                draw(rng, 0.2, 1.0),
                draw(rng, 0.2, 1.0),
            ),
        },
        _ => {
            // A cloud of points and its hull, turned: the eight corners of a
            // box, so it has volume, and up to eight more.
            let half = Vec3::new(
                draw(rng, 0.2, 0.8),
                draw(rng, 0.2, 0.8),
                draw(rng, 0.2, 0.8),
            );
            let mut points: Vec<Vec3> = (0u8..8)
                .map(|i| {
                    let sign = |bit: u8| if i >> bit & 1 == 0 { -1.0 } else { 1.0 };
                    half * Vec3::new(sign(0), sign(1), sign(2))
                })
                .collect();
            for _ in 0..pick(rng, 8) {
                points.push(vector(rng, 1.0));
            }
            let axis = vector(rng, 1.0).normalize_or(Vec3::Y);
            let turn = Quat::from_axis_angle(axis, draw(rng, 0.0, TAU));
            Shape::Convex(Convex::hull(&points, turn))
        }
    }
}

fn parry3(shape: &Shape<Vec3>) -> Box<dyn shape3::Shape> {
    match *shape {
        Shape::Ball { radius } => Box::new(shape3::Ball::new(radius)),
        Shape::Capsule { half_axis, radius } => {
            Box::new(shape3::Capsule::new(-half_axis, half_axis, radius))
        }
        Shape::Box { half } => Box::new(shape3::Cuboid::new(half)),
        Shape::Convex(convex) => Box::new(
            shape3::ConvexPolyhedron::from_convex_hull(convex.points())
                .expect("a hull with volume"),
        ),
    }
}

fn pose3(at: Vec3) -> parry3d::math::Pose {
    parry3d::math::Pose::from_translation(at)
}

/// [`core2`] in 3D.
fn core3(shape: &Shape<Vec3>) -> (Box<dyn shape3::Shape>, f32) {
    match *shape {
        Shape::Ball { radius } => (Box::new(shape3::Ball::new(0.0)), radius),
        Shape::Capsule { half_axis, radius } => (
            Box::new(shape3::Segment::new(-half_axis, half_axis)),
            radius,
        ),
        _ => (parry3(shape), 0.0),
    }
}

/// [`apart2`] in 3D.
fn apart3(a: &Shape<Vec3>, a_at: Vec3, b: &Shape<Vec3>, b_at: Vec3) -> (f32, f32) {
    let ((a_core, a_radius), (b_core, b_radius)) = (core3(a), core3(b));
    let cores = query3::distance(&pose3(b_at), &*b_core, &pose3(a_at), &*a_core)
        .expect("convex pairs are supported")
        .distance;
    ((cores - a_radius - b_radius).max(0.0), cores)
}

/// [`the_2d_kernel_agrees_with_parry`] in 3D, with hulls of up to sixteen
/// points.
#[test]
fn the_3d_kernel_agrees_with_parry() {
    let mut rng = SplitMix64::new(0x0919_9300);
    let mut tally = Tally::default();
    for _ in 0..2000 {
        let (body, target) = (shape3d(&mut rng), shape3d(&mut rng));
        let place = |rng: &mut SplitMix64| {
            Vec3::new(
                draw(rng, -4.0, 4.0),
                draw(rng, -4.0, 4.0),
                draw(rng, -4.0, 4.0),
            )
        };
        let at = place(&mut rng);
        let collider = Collider::new(1, target, place(&mut rng) * 0.5);
        let (body_p, target_p) = (parry3(&body), parry3(&target));

        let ours = distance(&body, at, &collider)
            .expect("sound shapes")
            .distance;
        let (theirs, cores) = apart3(&body, at, &target, collider.position);
        tally.worst_separation = tally.worst_separation.max((ours - theirs).abs());
        if theirs < 0.5 {
            tally.worst_near = tally.worst_near.max((ours - theirs).abs());
        }
        tally.separations += 1;
        assert!(
            ours <= theirs + SEPARATION_TOLERANCE
                && ours >= theirs - SEPARATION_TOLERANCE - parry_bound(cores),
            "{body:?} at {at} from {collider:?}: {ours} against parry's {theirs}"
        );

        let slop = slop_bound(&body, &target);
        if theirs <= 2.0 * slop {
            continue;
        }
        let aim = collider.position
            + Vec3::new(
                draw(&mut rng, -1.5, 1.5),
                draw(&mut rng, -1.5, 1.5),
                draw(&mut rng, -1.5, 1.5),
            );
        let motion = (aim - at) * draw(&mut rng, 0.5, 2.0);
        let hit = cast(&body, at, motion, &[collider][..], ALL_LAYERS).unwrap();
        let options = query3::ShapeCastOptions {
            max_time_of_impact: 1.0,
            ..query3::ShapeCastOptions::default()
        };
        let toi = query3::cast_shapes(
            &pose3(collider.position),
            Vec3::ZERO,
            &*target_p,
            &pose3(at),
            motion,
            &*body_p,
            options,
        )
        .expect("convex pairs are supported")
        .map(|hit| hit.time_of_impact);
        tally.casts += 1;
        let gap_at = |t: f32| apart3(&body, at + motion * t, &target, collider.position);
        let touching_at = |t: f32| {
            query3::intersection_test(
                &pose3(collider.position),
                &*target_p,
                &pose3(at + motion * t),
                &*body_p,
            )
            .expect("convex pairs are supported")
            .intersecting
        };
        match (hit, toi) {
            (Some(hit), _) if closed_form(&body, &target) => {
                // A box against a box is the exact closed-form sweep: parry
                // finds them touching at `t` and apart a little before it,
                // where their gap is four tolerances along the normal.
                tally.hits += 1;
                assert!(toi.is_some(), "parry meets what the sweep meets");
                let approach = -motion.dot(hit.normal);
                let before = hit.t - 4.0 * SEPARATION_TOLERANCE / approach;
                assert!(
                    gap_at(hit.t).0 <= SEPARATION_TOLERANCE && gap_at(before).0 > 0.0,
                    "{body:?} from {at} by {motion} at {collider:?}: t {}",
                    hit.t
                );
            }
            (Some(hit), _) => {
                tally.hits += 1;
                let (gap, cores) = gap_at(hit.t);
                let band = 1.25 * slop + SEPARATION_TOLERANCE + parry_bound(cores);
                assert!(
                    !touching_at(hit.t) && gap <= band,
                    "{body:?} from {at} by {motion} at {collider:?}: {gap} apart at t {}",
                    hit.t
                );
                if let Some(toi) = toi {
                    assert!(hit.t <= toi, "t {} after parry's {toi}", hit.t);
                }
            }
            (None, Some(toi)) => {
                panic!("{body:?} from {at} by {motion} at {collider:?}: parry touches at {toi}")
            }
            (None, None) => {}
        }
    }
    assert!(tally.hits > 300, "{tally:?}");
}
