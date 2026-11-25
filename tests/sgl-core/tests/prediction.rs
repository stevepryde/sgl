//! Property tests for `sgl_core::prediction` (core.md). Every property names
//! the defect it exists to catch and takes its expectation from an
//! independent oracle: the forward run of the game's own step function, the
//! inputs as the client sent them, or the spec's statement. Runs from a fixed
//! seed (testing.md 3); `PROPTEST_CASES` widens a local run. Native only:
//! proptest's `getrandom` does not build for `wasm32-unknown-unknown`.
#![cfg(not(target_arch = "wasm32"))]

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use sgl_core::prediction::{Correction, InputQueue, Predictor, Pushed, Sample, TickInput};
use std::time::Duration;

const SEED: [u8; 32] = *b"sgl-core prediction tests seed01";

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

// ------------------------------------------------------------- predictor

/// A game step whose result depends on the order of inputs and on the tick
/// it is told, so a replay that skips, repeats, reorders or mislabels an
/// input lands somewhere else. The predictor hands it the input by
/// reference, whatever its type.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn step(state: &mut i64, tick: u64, input: &i32) {
    *state = state
        .wrapping_mul(31)
        .wrapping_add(i64::from(*input))
        .wrapping_add(tick.wrapping_mul(1_000_003).cast_signed());
}

/// Defect: a replay that drops, repeats or misnumbers an input, starts from
/// the wrong place, or leaves a stale interpolation sample. Oracle: the
/// forward run — the step applied to the inputs in order, by the test, with
/// no predictor. A client that predicts every input and is acknowledged at
/// random ticks with the forward run's true state there must end where the
/// forward run ends, never count a correction, keep exactly the inputs after
/// the last acknowledgement pending, and keep the forward run's last two
/// states as its sample.
#[test]
fn replay_with_the_true_state_equals_the_forward_run() {
    let strategy = (
        0u64..1_000_000,
        any::<i64>(),
        prop::collection::vec((any::<i32>(), prop::option::of(0usize..8)), 1..64),
    );
    check(strategy, |(start, initial, frames)| {
        let mut forward = vec![initial];
        for (k, (input, _)) in frames.iter().enumerate() {
            let mut next = forward[k];
            step(&mut next, start + k as u64, input);
            forward.push(next);
        }

        let mut predictor = Predictor::new(initial, start, frames.len());
        let mut acked: Option<usize> = None;
        for (k, &(input, lag)) in frames.iter().enumerate() {
            prop_assert_eq!(predictor.predict(input, step), Ok(start + k as u64));
            // Acknowledge `lag` ticks behind the newest, never going back.
            let Some(j) = lag.and_then(|lag| k.checked_sub(lag)) else {
                continue;
            };
            let j = acked.map_or(j, |acked| j.max(acked));
            acked = Some(j);
            let reconciled = predictor
                .reconcile(start + j as u64, forward[j + 1], step)
                .map_err(|(error, _)| TestCaseError::fail(error.to_string()))?;
            prop_assert!(!reconciled.corrected);
            prop_assert_eq!(reconciled.replayed, k - j);
            prop_assert_eq!(*predictor.state(), forward[k + 1]);
            let mut want = Sample::new(forward[k]);
            want.push(forward[k + 1]);
            prop_assert_eq!(predictor.sample(), &want);
        }
        prop_assert_eq!(*predictor.state(), forward[frames.len()]);
        prop_assert_eq!(predictor.corrections(), 0);
        let first_pending = acked.map_or(0, |j| j + 1);
        let pending: Vec<_> = predictor.pending().map(|(t, i)| (t, *i)).collect();
        let sent: Vec<_> = frames[first_pending..]
            .iter()
            .enumerate()
            .map(|(n, (input, _))| (start + (first_pending + n) as u64, *input))
            .collect();
        prop_assert_eq!(pending, sent);
        Ok(())
    });
}

// ------------------------------------------------------------ input queue

/// Defect: a queue that hands an input out twice, out of order, under the
/// wrong tick, keeps a late one, or fails to repeat on a gap. Oracle: the
/// inputs as the client sent them and the network's delivery schedule. The
/// client sends tick `k`'s input at time `k` and resends it on the next
/// ticks (one entry per copy); copies are lost, delayed (reordering them)
/// and duplicated. The server runs tick `k` at time `k + lead`, after that
/// time's arrivals. Each tick must hand out its own input if any copy
/// arrived by then, and otherwise repeat the last one handed out; every copy
/// after the first is a duplicate if it arrives in time and late if not.
#[test]
fn input_queue_hands_out_each_tick_once_in_order_under_loss() {
    // Each copy: whether the network loses it, how many ticks after its send
    // it arrives, and a key that orders arrivals in the same tick.
    let copies = prop::collection::vec((any::<bool>(), 0usize..8, any::<u32>()), 1..4);
    let strategy = (
        0usize..4,
        prop::collection::vec((any::<u32>(), copies), 1..40),
    );
    check(strategy, |(lead, sent)| {
        // (arrival time, order key, tick)
        let mut arrivals: Vec<(usize, u32, usize)> = Vec::new();
        for (tick, (_, copies)) in sent.iter().enumerate() {
            for (resend, &(lost, delay, key)) in copies.iter().enumerate() {
                if !lost {
                    arrivals.push((tick + resend + delay, key, tick));
                }
            }
        }
        arrivals.sort_unstable();

        let mut queue = InputQueue::new(0, 64);
        let mut next_arrival = arrivals.iter().peekable();
        let mut arrived = vec![false; sent.len()];
        let (mut last, mut gaps, mut late) = (None, 0, 0);
        for time in 0..sent.len() + lead {
            while let Some(&(_, _, tick)) = next_arrival.next_if(|(at, _, _)| *at == time) {
                let pushed = queue.push(tick as u64, sent[tick].0);
                let want = if time > tick + lead {
                    late += 1;
                    Pushed::Late
                } else if std::mem::replace(&mut arrived[tick], true) {
                    Pushed::Duplicate
                } else {
                    Pushed::Queued
                };
                prop_assert_eq!(pushed, Ok(want), "tick {} at time {}", tick, time);
            }
            let Some(tick) = time.checked_sub(lead) else {
                continue;
            };
            let input = sent[tick].0;
            let want = if arrived[tick] {
                last = Some(input);
                TickInput::Received(&input)
            } else {
                gaps += 1;
                last.as_ref()
                    .map_or(TickInput::Missing, TickInput::Repeated)
            };
            prop_assert_eq!(queue.take(), (tick as u64, want));
        }
        // Copies still in flight after the last tick ran are late.
        for &(_, _, tick) in next_arrival {
            prop_assert_eq!(queue.push(tick as u64, sent[tick].0), Ok(Pushed::Late));
            late += 1;
        }
        prop_assert_eq!(queue.gaps(), gaps);
        prop_assert_eq!(queue.late(), late);
        prop_assert!(queue.is_empty());
        Ok(())
    });
}

// ------------------------------------------------------------- correction

/// Defect: a correction that overshoots (crosses to the other side), grows,
/// never reaches zero, or vanishes early. Oracle: core.md — the offset fades
/// from what was added to exactly zero at the fade time, never changing
/// sign or growing on the way.
#[test]
fn correction_reaches_zero_at_its_fade_time_and_never_overshoots() {
    let strategy = (
        1u64..1_000_000_000,
        prop_oneof![-1e6f32..-1e-3, 1e-3f32..1e6],
        prop::collection::vec(0u64..200_000_000, 1..64),
    );
    check(strategy, |(fade_ns, added, frames)| {
        let fade = Duration::from_nanos(fade_ns);
        let mut correction = Correction::<f32>::new(fade);
        correction.add(added);
        prop_assert_eq!(correction.offset().to_bits(), added.to_bits());
        let (mut elapsed, mut previous) = (0u64, added);
        for frame in frames {
            correction.advance(Duration::from_nanos(frame));
            elapsed += frame;
            let offset = correction.offset();
            prop_assert!(
                offset.abs() <= previous.abs(),
                "{} grew from {}",
                offset,
                previous
            );
            prop_assert!(offset == 0.0 || offset.signum() == added.signum());
            prop_assert_eq!(offset == 0.0, elapsed >= fade_ns, "at {} ns", elapsed);
            previous = offset;
        }
        Ok(())
    });
}
