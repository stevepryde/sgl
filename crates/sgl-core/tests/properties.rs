//! Property tests for the `sgl-core` contracts (core.md). Every property
//! names the defect it exists to catch and takes its expectation from the
//! spec, a published algorithm, a brute-force model, or a roundtrip — never
//! from the code under test. Runs from a fixed seed (testing.md 3);
//! `PROPTEST_CASES` widens a local run. Native only: proptest's `getrandom`
//! does not build for `wasm32-unknown-unknown`.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use sgl_core::anim::{AnimationSequence, SequenceLoop};
use sgl_core::collision::{
    Aabb, ColliderFlags, ColliderSet, CollisionConfig, move_and_collide, sweep_aabb,
};
use sgl_core::math::Vec2;
use sgl_core::random::Rng;
use sgl_core::time::{CatchUp, FixedClock};
use sgl_core::{Digest, Grid2, SplitMix64, StateHasher, derive_stream_seed};

const SEED: [u8; 32] = *b"sgl-core property tests seed  01";

fn check<S: Strategy>(strategy: S, test: impl Fn(S::Value) -> Result<(), TestCaseError>) {
    let config = Config {
        failure_persistence: None,
        ..Config::default()
    };
    let mut runner =
        TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &SEED));
    if let Err(failure) = runner.run(&strategy, test) {
        panic!("{failure}");
    }
}

fn finite(range: std::ops::Range<f32>) -> impl Strategy<Value = f32> {
    range.prop_filter("finite", |v| v.is_finite())
}

fn vec2(range: std::ops::Range<f32>) -> impl Strategy<Value = Vec2> {
    (finite(range.clone()), finite(range)).prop_map(|(x, y)| Vec2::new(x, y))
}

fn aabb() -> impl Strategy<Value = Aabb> {
    (vec2(-20.0..20.0), vec2(0.1..3.0)).prop_map(|(center, half)| Aabb { center, half })
}

// ---------------------------------------------------------------- hashing

/// One typed write; the same kind with a different value must change the
/// digest, whatever surrounds it.
#[derive(Clone, Debug, PartialEq)]
enum Write {
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    I32(i32),
    I64(i64),
    Bool(bool),
    Bytes(Vec<u8>),
    Seq(Vec<u32>),
}

impl Write {
    fn kind(&self) -> u8 {
        match self {
            Self::U8(_) => 0,
            Self::U16(_) => 1,
            Self::U32(_) => 2,
            Self::U64(_) => 3,
            Self::I32(_) => 4,
            Self::I64(_) => 5,
            Self::Bool(_) => 6,
            Self::Bytes(_) => 7,
            Self::Seq(_) => 8,
        }
    }

    fn apply(&self, hasher: &mut StateHasher) {
        match self {
            Self::U8(v) => hasher.u8(*v),
            Self::U16(v) => hasher.u16(*v),
            Self::U32(v) => hasher.u32(*v),
            Self::U64(v) => hasher.u64(*v),
            Self::I32(v) => hasher.i32(*v),
            Self::I64(v) => hasher.i64(*v),
            Self::Bool(v) => hasher.bool(*v),
            Self::Bytes(v) => hasher.bytes(v),
            Self::Seq(v) => hasher.sequence(v),
        }
    }
}

fn write_of_kind(kind: u8) -> BoxedStrategy<Write> {
    match kind {
        0 => any::<u8>().prop_map(Write::U8).boxed(),
        1 => any::<u16>().prop_map(Write::U16).boxed(),
        2 => any::<u32>().prop_map(Write::U32).boxed(),
        3 => any::<u64>().prop_map(Write::U64).boxed(),
        4 => any::<i32>().prop_map(Write::I32).boxed(),
        5 => any::<i64>().prop_map(Write::I64).boxed(),
        6 => any::<bool>().prop_map(Write::Bool).boxed(),
        7 => prop::collection::vec(any::<u8>(), 0..6)
            .prop_map(Write::Bytes)
            .boxed(),
        _ => prop::collection::vec(any::<u32>(), 0..4)
            .prop_map(Write::Seq)
            .boxed(),
    }
}

/// Two write sequences over the same type skeleton.
fn same_skeleton() -> impl Strategy<Value = (Vec<Write>, Vec<Write>)> {
    prop::collection::vec(0u8..9, 1..6).prop_flat_map(|kinds| {
        let a: Vec<_> = kinds.iter().map(|&k| write_of_kind(k)).collect();
        let b: Vec<_> = kinds.iter().map(|&k| write_of_kind(k)).collect();
        (a, b)
    })
}

fn digest(writes: &[Write]) -> Digest {
    let mut hasher = StateHasher::new();
    for write in writes {
        write.apply(&mut hasher);
    }
    hasher.finish()
}

/// Defect: a writer that drops its length prefix or truncates a value, so
/// two different states hash alike. Oracle: a canonical encoding is
/// injective over values of one type skeleton.
#[test]
fn same_skeleton_different_values_never_share_a_digest() {
    check(same_skeleton(), |(a, b)| {
        prop_assert_eq!(digest(&a), digest(&a));
        prop_assert_eq!(
            a.iter().map(Write::kind).collect::<Vec<_>>(),
            b.iter().map(Write::kind).collect::<Vec<_>>()
        );
        if a != b {
            prop_assert_ne!(digest(&a), digest(&b));
        }
        Ok(())
    });
}

/// Defect: byte writes concatenated without a length prefix, so
/// `("ab","c")` and `("a","bc")` collide. Oracle: framing must separate
/// every resplit of the same bytes.
#[test]
fn resplitting_bytes_changes_the_digest() {
    let strategy = (
        prop::collection::vec(any::<u8>(), 1..12),
        0usize..12,
        0usize..12,
    );
    check(strategy, |(bytes, cut_a, cut_b)| {
        let cut_a = cut_a.min(bytes.len());
        let cut_b = cut_b.min(bytes.len());
        let split = |cut: usize| {
            let mut h = StateHasher::new();
            h.bytes(&bytes[..cut]);
            h.bytes(&bytes[cut..]);
            h.finish()
        };
        if cut_a != cut_b {
            prop_assert_ne!(split(cut_a), split(cut_b));
        }
        let mut whole = StateHasher::new();
        whole.bytes(&bytes);
        prop_assert_ne!(whole.finish(), split(cut_a));
        Ok(())
    });
}

/// Defect: raw content digests routed through the framed hasher (or the
/// reverse), so two consumers disagree on the same bytes. Oracle: the
/// `blake3` crate for the raw form; the framed form must differ from it.
#[test]
fn raw_digest_is_plain_blake3_and_differs_from_the_framed_one() {
    check(prop::collection::vec(any::<u8>(), 0..64), |bytes| {
        prop_assert_eq!(
            Digest::hash_bytes(&bytes).0,
            *blake3::hash(&bytes).as_bytes()
        );
        let mut framed = StateHasher::new();
        framed.bytes(&bytes);
        prop_assert_ne!(framed.finish(), Digest::hash_bytes(&bytes));
        Ok(())
    });
}

// -------------------------------------------------------------------- rng

/// `SplitMix64` as published (Steele, Lea & Flood 2014; Vigna's reference C).
fn reference_splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Defect: a constant or shift altered while "optimizing" the frozen
/// stream, desyncing every replay. Oracle: the published algorithm.
#[test]
fn splitmix64_matches_the_published_algorithm_from_any_seed() {
    check((any::<u64>(), 1usize..64), |(seed, draws)| {
        let mut rng = SplitMix64::new(seed);
        let mut state = seed;
        for _ in 0..draws {
            prop_assert_eq!(rng.next_u64(), reference_splitmix64(&mut state));
        }
        Ok(())
    });
}

/// Defect: an off-by-one in the inclusive reduction. Oracle: the range.
#[test]
fn range_inclusive_stays_inside_the_range() {
    check(
        (any::<u64>(), any::<u32>(), any::<u32>()),
        |(seed, a, b)| {
            let (min, max) = (a.min(b), a.max(b));
            let value = SplitMix64::new(seed).range_inclusive(min, max);
            prop_assert!((min..=max).contains(&value));
            Ok(())
        },
    );
}

/// Defect: a domain component that no longer reaches the seed (dropped or
/// masked), so two streams that must differ share one. Oracle: each
/// component alone must change the seed.
#[test]
fn every_stream_seed_component_matters() {
    let strategy = (
        any::<u64>(),
        any::<u64>(),
        any::<u32>(),
        any::<u64>(),
        any::<u64>(),
        any::<u32>(),
        any::<u64>(),
    );
    check(strategy, |(base, d, a, b, d2, a2, b2)| {
        let seed = derive_stream_seed(base, d, a, b);
        if d2 != d {
            prop_assert_ne!(seed, derive_stream_seed(base, d2, a, b));
        }
        if a2 != a {
            prop_assert_ne!(seed, derive_stream_seed(base, d, a2, b));
        }
        if b2 != b {
            prop_assert_ne!(seed, derive_stream_seed(base, d, a, b2));
        }
        prop_assert_eq!(seed, derive_stream_seed(base, d, a, b));
        Ok(())
    });
}

// ------------------------------------------------------------------- time

/// Defect: accumulator drift or a broken clamp, so a stall produces a burst
/// of steps or the cadence oscillates. Oracle: core.md 2 — each frame adds
/// `min(dt, fixed_dt)` and runs at most one step; steps over a whole
/// schedule are the integer part of the clamped time.
#[test]
fn fixed_clock_runs_the_clamped_time_one_step_per_frame() {
    let strategy = prop::collection::vec(finite(0.0..0.3), 1..400);
    check(strategy, |dts| {
        let mut clock = FixedClock::with_hz(60.0);
        let fixed = f64::from(clock.fixed_dt);
        let mut steps = 0u32;
        let mut clamped_total = 0.0f64;
        for dt in dts {
            clock.begin_frame(dt);
            while clock.step() {
                steps += 1;
            }
            clock.finish();
            prop_assert!(
                clock.steps_this_frame <= 1,
                "clamp allows one step per frame"
            );
            prop_assert!(
                (0.0..1.0).contains(&clock.alpha),
                "alpha {} out of range",
                clock.alpha
            );
            clamped_total += f64::from(dt).min(fixed);
        }
        let expected = (clamped_total / fixed).floor() as u32;
        prop_assert!(
            steps.abs_diff(expected) <= 1,
            "{steps} steps for {clamped_total} s of clamped time (expected {expected})"
        );
        Ok(())
    });
}

/// Defect: a catch-up clock that loses or invents time, overruns its budget,
/// or carries more debt than allowed. Oracle: core.md 2 — supplied time is
/// either simulated, reported as dropped, or still held (at most the debt
/// plus a fractional step), and a frame runs at most its budget.
#[test]
fn catch_up_clock_conserves_supplied_time() {
    let strategy = (
        1u32..10,
        0u32..10,
        finite(10.0..120.0),
        prop::collection::vec(finite(0.0..0.5), 1..200),
    );
    check(strategy, |(budget, debt, hz, dts)| {
        let policy = CatchUp {
            max_steps_per_frame: budget,
            max_debt_steps: debt,
        };
        let mut clock = FixedClock::with_catch_up(hz, policy);
        let fixed = f64::from(clock.fixed_dt);
        let (mut supplied, mut simulated, mut dropped) = (0.0f64, 0.0f64, 0.0f64);
        for dt in dts {
            clock.begin_frame(dt);
            while clock.step() {}
            clock.finish();
            prop_assert!(clock.steps_this_frame <= budget, "budget overrun");
            prop_assert!((0.0..1.0).contains(&clock.alpha), "alpha {}", clock.alpha);
            prop_assert!(
                clock.dropped_dt >= 0.0,
                "negative drop {}",
                clock.dropped_dt
            );
            supplied += f64::from(dt);
            simulated += f64::from(clock.steps_this_frame) * fixed;
            dropped += f64::from(clock.dropped_dt);
            let held = supplied - simulated - dropped;
            prop_assert!(
                held > -1e-3 && held < (f64::from(debt) + 1.0) * fixed + 1e-3,
                "held {held} s outside [0, {} steps]",
                debt + 1
            );
        }
        Ok(())
    });
}

// ------------------------------------------------------------------- grid

/// Defect: an index computed with the wrong stride or an unchecked length,
/// so a cell reads its neighbour. Oracle: the bounds definition.
#[test]
fn grid_cells_are_addressable_exactly_inside_their_bounds() {
    let strategy = (0u16..40, 0u16..40, 0u16..64, 0u16..64, 0usize..2000);
    check(strategy, |(w, h, x, y, wrong_len)| {
        let grid = Grid2::filled(w, h, 0u8);
        prop_assert_eq!(grid.get(x, y).is_some(), x < w && y < h);
        prop_assert_eq!(grid.row(y).is_some(), y < h);
        let cells = usize::from(w) * usize::from(h);
        prop_assert!(Grid2::from_cells(w, h, vec![0u8; cells]).is_ok());
        if wrong_len != cells {
            prop_assert!(Grid2::from_cells(w, h, vec![0u8; wrong_len]).is_err());
        }
        Ok(())
    });
}

// -------------------------------------------------------------- collision

/// Separation of two boxes per axis (negative = penetration on that axis).
fn separation(a: &Aabb, b: &Aabb) -> Vec2 {
    (a.center - b.center).abs() - (a.half + b.half)
}

/// Defect: a slab sign error or wrong entering axis, so a body stops short,
/// tunnels, or is pushed sideways off a floor. Oracle: geometry — a hit is
/// a touching contact on the reported face, reached inside the motion, and
/// a miss has no penetration anywhere along the sampled path.
#[test]
fn swept_aabb_hits_touch_the_reported_face_and_misses_never_penetrate() {
    check(
        (aabb(), aabb(), vec2(-30.0..30.0)),
        |(body, target, delta)| {
            prop_assume!(delta != Vec2::ZERO);
            match sweep_aabb(&body, delta, &target) {
                Some(hit) => {
                    prop_assert!((0.0..=1.0).contains(&hit.t), "t = {}", hit.t);
                    prop_assert!(
                        [Vec2::X, Vec2::NEG_X, Vec2::Y, Vec2::NEG_Y].contains(&hit.normal),
                        "normal {}",
                        hit.normal
                    );
                    prop_assert!(hit.normal.dot(delta) < 0.0, "normal must oppose the motion");
                    let sep = separation(&body.translated(delta * hit.t), &target);
                    let along = sep.dot(hit.normal.abs());
                    prop_assert!(along.abs() < 1e-3, "contact face separation {along}");
                }
                None => {
                    if !body.overlaps(&target) {
                        for i in 0..=64 {
                            let at = body.translated(delta * (i as f32 / 64.0));
                            let sep = separation(&at, &target);
                            prop_assert!(
                                sep.x > -1e-3 || sep.y > -1e-3,
                                "missed sweep penetrates at step {i}: {sep}"
                            );
                        }
                    }
                }
            }
            Ok(())
        },
    );
}

/// Defect: a y-up/y-down asymmetry in the sweep. Oracle: mirroring every
/// input across the x axis mirrors the answer.
#[test]
fn swept_aabb_mirrors_across_the_x_axis() {
    check(
        (aabb(), aabb(), vec2(-30.0..30.0)),
        |(body, target, delta)| {
            let flip = |v: Vec2| Vec2::new(v.x, -v.y);
            let mirror = |a: &Aabb| Aabb {
                center: flip(a.center),
                half: a.half,
            };
            let hit = sweep_aabb(&body, delta, &target);
            let mirrored = sweep_aabb(&mirror(&body), flip(delta), &mirror(&target));
            match (hit, mirrored) {
                (None, None) => {}
                (Some(h), Some(m)) => {
                    prop_assert_eq!(h.t.to_bits(), m.t.to_bits());
                    prop_assert_eq!(flip(h.normal), m.normal);
                }
                other => prop_assert!(false, "asymmetric result {other:?}"),
            }
            Ok(())
        },
    );
}

fn solids() -> impl Strategy<Value = Vec<Aabb>> {
    prop::collection::vec(
        (-8i32..8, -8i32..8, 1u8..3, 1u8..3).prop_map(|(x, y, hw, hh)| Aabb {
            center: Vec2::new(x as f32, y as f32),
            half: Vec2::new(f32::from(hw) * 0.5, f32::from(hh) * 0.5),
        }),
        0..8,
    )
}

fn config(up: Vec2) -> CollisionConfig {
    CollisionConfig::new(up, 0.01, 0.1, 0.7, 1e-4)
}

/// Defect: a slide pass that pushes the body into the next solid, or a skin
/// applied with the wrong sign. Oracle: the body never overlaps a solid
/// after the move; mirroring the world across the x axis (and `up`) mirrors
/// the move and keeps the floor/ceiling/wall classification.
#[test]
fn move_and_collide_never_ends_inside_a_solid_and_mirrors_with_up() {
    let body = (vec2(-10.0..10.0), vec2(0.2..1.0)).prop_map(|(center, half)| Aabb { center, half });
    check((solids(), body, vec2(-6.0..6.0)), |(boxes, body, delta)| {
        prop_assume!(boxes.iter().all(|b| !b.overlaps(&body)));
        let build = |flip: bool| {
            let mut set = ColliderSet::new(2.0);
            for b in &boxes {
                let center = if flip {
                    Vec2::new(b.center.x, -b.center.y)
                } else {
                    b.center
                };
                set.insert(
                    Aabb {
                        center,
                        half: b.half,
                    },
                    ColliderFlags::SOLID,
                );
            }
            set
        };
        let set = build(false);
        let (effective, contacts) = move_and_collide(body, delta, &set, &config(Vec2::Y));
        let moved = body.translated(effective);
        for b in &boxes {
            let sep = separation(&moved, b);
            prop_assert!(sep.x > -1e-3 || sep.y > -1e-3, "ended inside {b:?}: {sep}");
        }
        prop_assert!(
            effective.length() <= delta.length() + 1e-4,
            "moved further than asked"
        );

        let flipped_body = Aabb {
            center: Vec2::new(body.center.x, -body.center.y),
            half: body.half,
        };
        let (mirrored, mirrored_contacts) = move_and_collide(
            flipped_body,
            Vec2::new(delta.x, -delta.y),
            &build(true),
            &config(Vec2::NEG_Y),
        );
        prop_assert!(
            (mirrored.x - effective.x).abs() < 1e-4 && (mirrored.y + effective.y).abs() < 1e-4,
            "{effective} vs {mirrored}"
        );
        prop_assert_eq!(
            (contacts.floor, contacts.ceiling, contacts.wall),
            (
                mirrored_contacts.floor,
                mirrored_contacts.ceiling,
                mirrored_contacts.wall
            )
        );
        Ok(())
    });
}

/// Defect: a broadphase cell rounding that drops a candidate, which the
/// sweep then tunnels through. Oracle: an O(n) overlap scan.
#[test]
fn broadphase_query_reports_every_overlapping_collider() {
    let strategy = (finite(0.5..4.0), solids(), aabb());
    check(strategy, |(cell, boxes, region)| {
        let mut set = ColliderSet::new(cell);
        for b in &boxes {
            set.insert(*b, ColliderFlags::SOLID);
        }
        let candidates = set.query(&region);
        for (index, b) in boxes.iter().enumerate() {
            if b.overlaps(&region) {
                prop_assert!(
                    candidates.contains(&index),
                    "collider {index} {b:?} missing for {region:?}"
                );
            }
        }
        Ok(())
    });
}

// -------------------------------------------------------------- sequences

/// Defect: randomness or step bookkeeping leaking into a replay, or a
/// finished `Once` sequence coming back to life. Oracle: core.md 6 — the
/// same seed and schedule replay the same frames, and finished is terminal.
#[test]
fn sequences_replay_from_a_seed_and_finish_terminally() {
    let strategy = (
        any::<u64>(),
        prop::collection::vec(finite(0.001..0.3), 1..80),
    );
    check(strategy, |(seed, schedule)| {
        let run = || {
            let mut sequence = AnimationSequence::builder()
                .loop_mode(SequenceLoop::Once)
                .play_frames(vec![0, 1, 2], 0.1)
                .random_pause(0.05, 0.2)
                .play_frames(vec![3, 4], 0.05)
                .build()
                .expect("valid sequence");
            let mut rng = Rng::from_seed(seed);
            let mut trace = Vec::new();
            let mut finished_at = None;
            for (i, dt) in schedule.iter().enumerate() {
                sequence.tick(*dt, &mut rng);
                trace.push(sequence.current_frame());
                if sequence.is_finished() {
                    finished_at.get_or_insert(i);
                } else {
                    assert!(
                        finished_at.is_none(),
                        "finished sequence resumed at tick {i}"
                    );
                }
                if finished_at.is_some() {
                    assert_eq!(
                        sequence.current_frame(),
                        Some(4),
                        "finished sequence holds its authored final frame"
                    );
                }
            }
            trace
        };
        prop_assert_eq!(run(), run());
        Ok(())
    });
}
