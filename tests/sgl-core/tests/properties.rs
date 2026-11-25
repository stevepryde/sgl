//! Property tests for the `sgl-core` contracts (core.md). Every property
//! names the defect it exists to catch and takes its expectation from the
//! spec, a published algorithm, a brute-force model, or a roundtrip — never
//! from the code under test. Runs from a fixed seed (testing.md 3);
//! `PROPTEST_CASES` widens a local run. Native only: proptest's `getrandom`
//! does not build for `wasm32-unknown-unknown`.
#![cfg(not(target_arch = "wasm32"))]

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use sgl_core::anim::{AnimationSequence, SequenceLoop};
use sgl_core::random::Rng;
use sgl_core::time::{CatchUp, FixedClock};
use sgl_core::{Digest, Grid2, SplitMix64, StateHasher, derive_stream_seed};
use std::time::Duration;

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

/// Frame times up to `max` whole nanoseconds.
fn frame_times(max: u64, frames: std::ops::Range<usize>) -> impl Strategy<Value = Vec<Duration>> {
    prop::collection::vec((0..=max).prop_map(Duration::from_nanos), frames)
}

/// Defect: accumulator drift or a broken clamp, so a stall produces a burst
/// of steps, the cadence oscillates, or time is lost. Oracle: core.md 2 —
/// each frame adds `min(elapsed, step)` and runs at most one step; the
/// clamped-off time is reported exactly in `dropped_dt`, and the clamped
/// time not yet simulated is under one step. The 60 Hz step is 1 s / 60 to
/// the nearest nanosecond, worked by hand.
#[test]
fn fixed_clock_runs_the_clamped_time_one_step_per_frame() {
    const STEP_60HZ_NS: u128 = 16_666_667;
    check(frame_times(300_000_000, 1..400), |frames| {
        let mut clock = FixedClock::with_hz(60.0);
        let (mut steps, mut clamped) = (0u128, 0u128);
        for elapsed in frames {
            clock.begin_frame(elapsed);
            while clock.step() {}
            clock.finish();
            prop_assert!(clock.steps_this_frame <= 1, "clamp allows one step");
            prop_assert!((0.0..1.0).contains(&clock.alpha), "alpha {}", clock.alpha);
            let added = elapsed.as_nanos().min(STEP_60HZ_NS);
            prop_assert_eq!(clock.dropped_dt.as_nanos(), elapsed.as_nanos() - added);
            clamped += added;
            steps += u128::from(clock.steps_this_frame);
            let held = clamped.checked_sub(steps * STEP_60HZ_NS);
            prop_assert!(
                held.is_some_and(|held| held < STEP_60HZ_NS),
                "held {held:?} ns outside [0, one step)"
            );
        }
        Ok(())
    });
}

/// Defect: a catch-up clock that loses or invents time, overruns its budget,
/// or carries more debt than allowed. Oracle: core.md 2 — supplied time is
/// exactly simulated, reported as dropped, or still held (under the debt
/// plus one step), and a frame runs at most its budget.
#[test]
fn catch_up_clock_conserves_supplied_time() {
    let strategy = (
        1u32..10,
        0u32..10,
        finite(10.0..120.0),
        frame_times(500_000_000, 1..200),
    );
    check(strategy, |(budget, debt, hz, frames)| {
        let policy = CatchUp {
            max_steps_per_frame: budget,
            max_debt_steps: debt,
        };
        let mut clock = FixedClock::with_catch_up(hz, policy);
        let step = clock.fixed_step().as_nanos();
        let (mut supplied, mut simulated, mut dropped) = (0u128, 0u128, 0u128);
        for elapsed in frames {
            clock.begin_frame(elapsed);
            while clock.step() {}
            clock.finish();
            prop_assert!(clock.steps_this_frame <= budget, "budget overrun");
            prop_assert!((0.0..1.0).contains(&clock.alpha), "alpha {}", clock.alpha);
            supplied += elapsed.as_nanos();
            simulated += u128::from(clock.steps_this_frame) * step;
            dropped += clock.dropped_dt.as_nanos();
            let held = supplied.checked_sub(simulated + dropped);
            prop_assert!(
                held.is_some_and(|held| held < (u128::from(debt) + 1) * step),
                "held {held:?} ns outside [0, {} steps)",
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
        let grid = Grid2::filled(w, h, 0u8).expect("small grid");
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
