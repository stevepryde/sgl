//! Tests the mutation run of #924 showed missing in `sgl_core::prediction`
//! (core.md 9). Each names the defect it catches; the expectations come
//! from the spec's sentences, never from the code. Runs natively and in a
//! wasm runtime.

use sgl_core::prediction::{InputQueue, Playback, SnapshotBuffer};
use wasm_bindgen_test::wasm_bindgen_test;

/// Defect: an input queue that hands out no input, or the wrong tick, or
/// that reads as empty while it holds inputs. Oracle: core.md 9
/// (`InputQueue` hands each tick out once, in order: its own input if it
/// arrived, else the last one taken). From tick 100: tick 100's input is
/// handed out as itself; tick 101, which never arrived, repeats it; tick
/// 102 is handed out as itself; the queue holds an input until it is
/// taken, and a queue that never received one hands out none.
#[wasm_bindgen_test(unsupported = test)]
fn an_input_queue_hands_out_each_tick_its_input_or_the_last_one() {
    let mut queue = InputQueue::new(100, 8);
    assert!(queue.is_empty());
    let _ = queue.push(100, 'a').unwrap();
    let _ = queue.push(102, 'c').unwrap();
    assert!(!queue.is_empty());
    let taken: Vec<(u64, Option<char>)> = (0..3)
        .map(|_| {
            let (tick, input) = queue.take();
            (tick, input.input().copied())
        })
        .collect();
    assert_eq!(
        taken,
        [(100, Some('a')), (101, Some('a')), (102, Some('c'))]
    );
    assert!(queue.is_empty());
    assert_eq!(queue.next_tick(), 103);
    let mut fresh: InputQueue<char> = InputQueue::new(7, 4);
    assert_eq!(fresh.take(), (7, sgl_core::prediction::TickInput::Missing));
    assert_eq!(fresh.next_tick(), 8);
}

/// Defect: a snapshot sample that reaches the next tick when the clock's
/// `alpha` is 1 or more, or a buffer that reads as empty while it holds
/// snapshots. Oracle: core.md 9 (`SnapshotBuffer` samples `delay_ticks`
/// behind the game's tick with `interpolate(a, b, t)` on the two snapshots
/// around the instant) and the sampler's own statement (`alpha` is clamped
/// to `[0, 1)`). Snapshots 0, 10 and 20 at ticks 0, 1 and 2, sampled at tick
/// 0 with an `alpha` of 1, 1.5 or the largest below 1, draw strictly
/// between the first two, never the second itself.
#[wasm_bindgen_test(unsupported = test)]
fn a_sample_stays_inside_its_tick_whatever_alpha_says() {
    for alpha in [1.0f32, 1.5, 1.0 - f32::EPSILON / 2.0] {
        let mut buffer = SnapshotBuffer::new(
            8,
            Playback {
                delay_ticks: 0,
                max_extrapolation_ticks: 4,
            },
        );
        assert!(buffer.is_empty());
        for (tick, value) in [(0u64, 0.0f64), (1, 10.0), (2, 20.0)] {
            buffer.push(tick, value).unwrap();
        }
        assert!(!buffer.is_empty());
        let drawn = buffer
            .sample(0, alpha, |a, b, t| a + (b - a) * f64::from(t))
            .expect("snapshots are held")
            .into_state();
        assert!(drawn > 9.9 && drawn < 10.0, "alpha {alpha}: {drawn}");
    }
}
