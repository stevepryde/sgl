//! A second engine as the oracle for planar bumps (#921; #924, item 4):
//! parry (`parry2d`, Apache-2.0), which shares no code with the `Box2D`
//! time of impact and manifold the crate ports. Over a few hundred seeded
//! pairs of turned boxes and discs, the crate's time of impact for bodies
//! that move and turn must meet where parry's nonlinear time of impact says
//! the shapes touch, within the contact band the crate states; and the
//! crate's manifold of overlapping pairs must have parry's contact depth
//! and normal. Native only: the comparison needs no browser, and parry is
//! not built for `wasm32` here.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use parry2d::math::Pose;
use parry2d::query::{self, NonlinearRigidMotion, ShapeCastStatus};
use parry2d::shape as shape2;

use sgl_core::SplitMix64;
use sgl_core::math::Vec2;
use sgl_physics::Shape;
use sgl_physics::planar::{self, Body, Motion, Toi};

fn draw(rng: &mut SplitMix64, low: f32, high: f32) -> f32 {
    low + (high - low) * ((rng.next_u64() >> 40) as f32 / 16_777_216.0)
}

/// A turned box or a disc: the planar bodies the driving games use.
fn shape(rng: &mut SplitMix64) -> Shape<Vec2> {
    if rng.next_u64().is_multiple_of(3) {
        Shape::Ball {
            radius: draw(rng, 0.2, 1.0),
        }
    } else {
        Shape::Box {
            half: Vec2::new(draw(rng, 0.3, 1.5), draw(rng, 0.2, 1.0)),
        }
    }
}

/// The shape's core in parry and its radius: a disc's centre is a point,
/// whose distance parry measures exactly; a box has no radius.
fn core(shape: &Shape<Vec2>) -> (Box<dyn shape2::Shape>, f32) {
    match *shape {
        Shape::Ball { radius } => (Box::new(shape2::Ball::new(0.0)), radius),
        Shape::Box { half } => (Box::new(shape2::Cuboid::new(half)), 0.0),
        _ => unreachable!("boxes and discs only"),
    }
}

/// The same shape in parry.
fn whole(shape: &Shape<Vec2>) -> Box<dyn shape2::Shape> {
    match *shape {
        Shape::Ball { radius } => Box::new(shape2::Ball::new(radius)),
        Shape::Box { half } => Box::new(shape2::Cuboid::new(half)),
        _ => unreachable!("boxes and discs only"),
    }
}

/// The contract's query slop for a pair: a hundredth of the smaller
/// shape's extent, at most a quarter of the smaller radius (physics.md,
/// 19). The rounding floor never applies at these sizes.
fn slop(a: &Shape<Vec2>, b: &Shape<Vec2>) -> f32 {
    let extent = |shape: &Shape<Vec2>| shape.half_extents().max_element();
    let mut slop = extent(a).min(extent(b)) / 100.0;
    for shape in [a, b] {
        if let Shape::Ball { radius } = *shape {
            slop = slop.min(radius / 4.0);
        }
    }
    slop
}

/// How far parry's distance between two cores `cores` apart may lie above
/// the true one: its GJK stops when its bounds agree to `sqrt(10 ε)` of
/// the distance (`oracle_parry.rs` says more).
fn parry_bound(cores: f32) -> f32 {
    (10.0 * f32::EPSILON).sqrt() * cores
}

/// A body and its motion over the step: where its centre is and which way
/// it points at a fraction of the step, by the motion the crate documents
/// (physics.md, planar bodies), worked in `f64`: the centre moves straight;
/// the heading turns by `atan(turn)` in all, and in between is the
/// normalised blend of the start and end headings' cosines and sines.
struct Swept {
    body: Body,
    motion: Motion,
}

impl Swept {
    fn angle_at(&self, fraction: f64) -> f64 {
        let start = f64::from(self.body.heading.y).atan2(f64::from(self.body.heading.x));
        let total = f64::from(self.motion.turn).atan();
        start + (fraction * total.sin()).atan2((1.0 - fraction) + fraction * total.cos())
    }

    fn centre_at(&self, fraction: f64) -> (f64, f64) {
        (
            f64::from(self.body.position.x) + fraction * f64::from(self.motion.translation.x),
            f64::from(self.body.position.y) + fraction * f64::from(self.motion.translation.y),
        )
    }

    fn pose_at(&self, fraction: f64) -> Pose {
        let (x, y) = self.centre_at(fraction);
        Pose::new(
            Vec2::new(x as f32, y as f32),
            self.angle_at(fraction) as f32,
        )
    }

    /// Parry's motion over the part of the step from `from` to `to`, as a
    /// constant velocity and spin: the documented motion, which blends the
    /// headings rather than turning at a constant rate, in sixteen such
    /// pieces turns within 1e-5 of it.
    fn piece(&self, from: f64, to: f64) -> NonlinearRigidMotion {
        let span = to - from;
        let (x0, y0) = self.centre_at(from);
        let (x1, y1) = self.centre_at(to);
        NonlinearRigidMotion::new(
            self.pose_at(from),
            Vec2::ZERO,
            Vec2::new(((x1 - x0) / span) as f32, ((y1 - y0) / span) as f32),
            ((self.angle_at(to) - self.angle_at(from)) / span) as f32,
        )
    }
}

/// The pieces parry's motion is cut into.
const PIECES: usize = 16;

/// What parry finds over the step.
#[derive(Debug, PartialEq)]
enum Parry {
    /// The shapes touch first at this fraction.
    Touch(f32),
    /// They touch or overlap at this fraction, so touch first no later:
    /// parry's search stopped within 1e-4 of the touch, or stepped past it
    /// into the overlap (its `Failed`).
    TouchBy(f32),
    /// They overlap at the start.
    Overlap,
    /// They never touch.
    Clear,
    /// Its search did not converge.
    Unknown,
}

fn parry_toi(a: &Swept, b: &Swept) -> Parry {
    let (shape_a, shape_b) = (whole(&a.body.shape), whole(&b.body.shape));
    for piece in 0..PIECES {
        let from = piece as f64 / PIECES as f64;
        let to = (piece + 1) as f64 / PIECES as f64;
        let hit = query::cast_shapes_nonlinear(
            &a.piece(from, to),
            &*shape_a,
            &b.piece(from, to),
            &*shape_b,
            0.0,
            (to - from) as f32,
            true,
        )
        .expect("boxes and discs are supported");
        if let Some(hit) = hit {
            return match hit.status {
                ShapeCastStatus::PenetratingOrWithinTargetDist if piece == 0 => Parry::Overlap,
                ShapeCastStatus::Converged | ShapeCastStatus::PenetratingOrWithinTargetDist => {
                    Parry::Touch(from as f32 + hit.time_of_impact)
                }
                ShapeCastStatus::Failed => {
                    let by = from as f32 + hit.time_of_impact;
                    if overlapping_at(a, b, by) || gap_at(a, b, by).0 <= 1e-4 {
                        Parry::TouchBy(by)
                    } else {
                        Parry::Unknown
                    }
                }
                ShapeCastStatus::OutOfIterations => Parry::Unknown,
            };
        }
    }
    Parry::Clear
}

/// Whether the shapes overlap at a fraction of the step, by parry.
fn overlapping_at(a: &Swept, b: &Swept, fraction: f32) -> bool {
    query::intersection_test(
        &a.pose_at(f64::from(fraction)),
        &*whole(&a.body.shape),
        &b.pose_at(f64::from(fraction)),
        &*whole(&b.body.shape),
    )
    .expect("boxes and discs are supported")
    .intersecting
}

/// The gap between the surfaces at a fraction of the step, by parry,
/// between the cores less the radii, and the distance between the cores.
fn gap_at(a: &Swept, b: &Swept, fraction: f32) -> (f32, f32) {
    let ((core_a, radius_a), (core_b, radius_b)) = (core(&a.body.shape), core(&b.body.shape));
    let cores = query::distance(
        &a.pose_at(f64::from(fraction)),
        &*core_a,
        &b.pose_at(f64::from(fraction)),
        &*core_b,
    )
    .expect("boxes and discs are supported")
    .distance;
    (cores - radius_a - radius_b, cores)
}

/// A body within `near` of the origin, turned at random, moving up to 3
/// units, about half the time toward somewhere near the origin, and
/// turning up to 0.6 radians' rule.
fn random_body(rng: &mut SplitMix64, id: u64, near: f32) -> Swept {
    let angle = draw(rng, -3.2, 3.2);
    let position = Vec2::new(draw(rng, -near, near), draw(rng, -near, near));
    let wander = Vec2::new(draw(rng, -1.5, 1.5), draw(rng, -1.5, 1.5));
    let translation = if rng.next_u64().is_multiple_of(2) {
        let toward = wander - position;
        toward * (3.0 / toward.length().max(3.0))
    } else {
        wander * 2.0
    };
    Swept {
        body: Body {
            id,
            shape: shape(rng),
            position,
            heading: Vec2::new(angle.cos(), angle.sin()),
            velocity: Vec2::ZERO,
            spin: 0.0,
            inverse_mass: 1.0,
            inverse_inertia: 1.0,
        },
        motion: Motion {
            translation,
            turn: draw(rng, -0.6, 0.6),
        },
    }
}

/// What the comparison met.
#[derive(Debug, Default)]
struct Tally {
    hits: usize,
    misses: usize,
    grazes: usize,
    overlaps: usize,
    unknown: usize,
    /// The furthest a hit's gap lay outside the band, beyond parry's
    /// precision; zero when every one lay inside.
    worst_outside: f32,
}

/// Defect: a port of the time of impact that tunnels (meets after the
/// shapes touch), meets far before they come close, misses a contact a
/// turn brings about, or is wired with the wrong turn. Oracle: parry's
/// nonlinear time of impact over the motion the crate documents, cut into
/// sixteen pieces of constant velocity and spin, and parry's distance and
/// overlap test. For 400 seeded pairs of turned boxes and discs, each
/// moving up to 3 units and turning up to 0.6 radians' rule over the step:
/// when parry finds the shapes touch (or overlap, where its search stepped
/// past the touch), the crate hits no later (to 1e-4 of the step, the pieces'
/// departure from the documented turn, at most 1e-5 radians over a reach
/// of 1.8, at the slowest closing speeds here), with the gap there, by
/// parry, three quarters to one and a quarter slops (to parry's own
/// precision, `parry_bound`, and 1e-5 of rounding); when parry finds no
/// touch, the crate reports none or a graze within that band; and an
/// overlap at the start is an overlap for both.
#[test]
fn the_time_of_impact_agrees_with_parry() {
    let mut rng = SplitMix64::new(0x0921_7012);
    let mut tally = Tally::default();
    for _ in 0..400 {
        let a = random_body(&mut rng, 1, 0.0);
        let b = random_body(&mut rng, 2, 3.5);
        let ours = planar::time_of_impact(&a.body, a.motion, &b.body, b.motion);
        let theirs = parry_toi(&a, &b);
        let slop = slop(&a.body.shape, &b.body.shape);
        let in_band = |fraction: f32, from_zero: bool| {
            let (gap, cores) = gap_at(&a, &b, fraction);
            let slack = parry_bound(cores) + 1e-5;
            let low = if from_zero {
                -slack
            } else {
                0.75 * slop - slack
            };
            let high = 1.25 * slop + slack;
            let outside = (low - gap).max(gap - high).max(0.0);
            (gap >= low && gap <= high, outside)
        };
        let case = format!(
            "{:?} by {:?}, {:?} by {:?}",
            a.body, a.motion, b.body, b.motion
        );
        match (theirs, ours) {
            (Parry::Unknown, _) => tally.unknown += 1,
            (Parry::Overlap, ours) => {
                tally.overlaps += 1;
                let touching = matches!(ours, Toi::Hit { fraction, .. } if fraction == 0.0);
                assert!(
                    ours == Toi::Overlapping || touching,
                    "{case}: parry overlaps, the crate {ours:?}"
                );
            }
            (Parry::Touch(touch) | Parry::TouchBy(touch), Toi::Hit { fraction, .. }) => {
                tally.hits += 1;
                assert!(
                    fraction <= touch + 1e-4,
                    "{case}: hit at {fraction}, parry touches at {touch}"
                );
                let (inside, outside) = in_band(fraction, fraction == 0.0);
                tally.worst_outside = tally.worst_outside.max(outside);
                assert!(inside, "{case}: the gap at {fraction} is outside the band");
            }
            (Parry::Touch(touch) | Parry::TouchBy(touch), ours) => {
                panic!("{case}: parry touches at {touch}, the crate {ours:?}")
            }
            (Parry::Clear, Toi::Separated) => tally.misses += 1,
            (Parry::Clear, Toi::Hit { fraction, .. }) => {
                tally.grazes += 1;
                let (gap, cores) = gap_at(&a, &b, fraction);
                assert!(
                    gap <= 1.25 * slop + parry_bound(cores) + 1e-5,
                    "{case}: a graze {gap} apart at {fraction}"
                );
            }
            (Parry::Clear, ours) => panic!("{case}: parry finds no touch, the crate {ours:?}"),
        }
    }
    // Enough of each outcome for the comparison to mean something.
    assert!(
        tally.hits > 100 && tally.misses > 50 && tally.overlaps > 10 && tally.unknown < 8,
        "{tally:?}"
    );
}

/// The least overlap of two boxes' projections over every edge normal of
/// both, in `f64`, and the next least: two boxes that overlap about as
/// deeply along two axes may each be separated along either.
fn box_axes(a: &Body, b: &Body) -> (f64, f64) {
    let corners = |body: &Body| {
        let Shape::Box { half } = body.shape else {
            unreachable!()
        };
        let (c, s) = (f64::from(body.heading.x), f64::from(body.heading.y));
        let length = c.hypot(s);
        let (c, s) = (c / length, s / length);
        let (hx, hy) = (f64::from(half.x), f64::from(half.y));
        let (px, py) = (f64::from(body.position.x), f64::from(body.position.y));
        let points: Vec<(f64, f64)> = [(-hx, -hy), (hx, -hy), (hx, hy), (-hx, hy)]
            .iter()
            .map(|&(x, y)| (px + c * x - s * y, py + s * x + c * y))
            .collect();
        (points, [(c, s), (-s, c)])
    };
    let ((points_a, axes_a), (points_b, axes_b)) = (corners(a), corners(b));
    let mut depths: Vec<f64> = axes_a
        .iter()
        .chain(&axes_b)
        .map(|&(x, y)| {
            let span = |points: &[(f64, f64)]| {
                points
                    .iter()
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
                        let d = p.0 * x + p.1 * y;
                        (lo.min(d), hi.max(d))
                    })
            };
            let ((lo_a, hi_a), (lo_b, hi_b)) = (span(&points_a), span(&points_b));
            hi_a.min(hi_b) - lo_a.max(lo_b)
        })
        .collect();
    depths.sort_by(f64::total_cmp);
    (depths[0], depths[1])
}

/// Defect: a manifold port with the wrong depth or normal (a flipped
/// reference edge, a lost radius, a normal from B toward A). Oracle:
/// parry's contact, its penetration depth and its normal from the first
/// shape. For 300 seeded overlapping pairs of turned boxes and discs, the
/// crate's depth equals parry's to 1e-4 (both are exact for these shapes;
/// that is `f32` rounding of coordinates up to 4), and its normal equals
/// parry's to 1e-3, except between two boxes whose two least overlaps
/// (worked in the test) are within 1e-4 of each other, where either axis
/// is a right answer and only the depth is compared.
#[test]
fn the_manifold_agrees_with_parry() {
    let mut rng = SplitMix64::new(0x0921_3a11);
    let (mut compared, mut ties) = (0, 0);
    let mut worst_depth = 0.0f32;
    while compared < 300 {
        let a = random_body(&mut rng, 1, 0.0).body;
        let b = random_body(&mut rng, 2, 1.8).body;
        let pose = |body: &Body| Pose::new(body.position, body.heading.y.atan2(body.heading.x));
        let Some(theirs) = query::contact(
            &pose(&a),
            &*whole(&a.shape),
            &pose(&b),
            &*whole(&b.shape),
            0.0,
        )
        .expect("boxes and discs are supported") else {
            continue;
        };
        if theirs.dist > -1e-3 {
            continue;
        }
        compared += 1;
        let ours = planar::manifold(&a, &b).expect("overlapping bodies have a manifold");
        let case = format!("{a:?} {b:?}: ours {ours:?}, parry's {theirs:?}");
        worst_depth = worst_depth.max((ours.depth + theirs.dist).abs());
        assert!((ours.depth + theirs.dist).abs() <= 1e-4, "{case}");
        let boxes = matches!((a.shape, b.shape), (Shape::Box { .. }, Shape::Box { .. }));
        if boxes {
            let (least, next) = box_axes(&a, &b);
            if next - least <= 1e-4 {
                ties += 1;
                continue;
            }
        }
        assert!(
            (ours.normal - theirs.normal1).abs().max_element() <= 1e-3,
            "{case}"
        );
    }
    assert!(ties < 30, "{ties} ties");
    assert!(worst_depth <= 1e-4);
}
