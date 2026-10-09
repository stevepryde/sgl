//! Fixed-step clock with render interpolation.
//!
//! The loop accumulates caller-supplied frame time as an exact [`Duration`]
//! and runs the fixed-step simulation in constant steps of `1/hz` seconds
//! rounded to the nearest nanosecond. A clock picks one of two policies:
//!
//! - **Render-paced** ([`FixedClock::with_hz`], the default): the per-frame
//!   delta fed into the accumulator is **clamped** to one fixed step, so the
//!   accumulator never routinely crosses the 2-step threshold when the render
//!   rate sits near the fixed rate — the proven fix for the 60 Hz
//!   "rubber-banding" cadence oscillation. The clamp trades real-time
//!   catch-up for visual stability: simulation runs slower than real time
//!   whenever frames are longer than a step.
//! - **Bounded catch-up** ([`FixedClock::with_catch_up`]): the whole delta
//!   is accumulated and a frame runs up to [`CatchUp::max_steps_per_frame`]
//!   steps, so simulation follows elapsed time while frames are slower than
//!   the tick rate. Time beyond that budget is discarded, apart from up to
//!   [`CatchUp::max_debt_steps`] whole steps carried to later frames. For an
//!   authoritative or networked simulation that must keep pace with time.

use std::time::Duration;

/// Default simulation rate: 60 Hz.
pub const DEFAULT_HZ: f32 = 60.0;

/// The largest `f32` below 1: `alpha` stays in `[0, 1)` under rounding.
const ALPHA_MAX: f32 = 1.0 - f32::EPSILON / 2.0;

/// Bounded catch-up policy for [`FixedClock::with_catch_up`].
///
/// Each frame runs at most `max_steps_per_frame` steps. Whole steps still due
/// after that budget are carried to later frames up to `max_debt_steps`; the
/// rest is discarded and reported in [`FixedClock::dropped_dt`]. With
/// `max_debt_steps: 0` a stall runs one full budget and the next normal
/// frame runs normally; a larger debt replays more of the stall, at up to a
/// full budget per frame, until it is repaid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatchUp {
    /// Most fixed steps one frame may run (at least 1).
    pub max_steps_per_frame: u32,
    /// Whole steps carried past a frame's budget; the excess is discarded.
    pub max_debt_steps: u32,
}

/// A fixed-timestep accumulator + render interpolation factor.
///
/// Usage per rendered frame:
/// ```text
/// clock.begin_frame(frame_elapsed); // a Duration
/// while clock.step() { /* one fixed sim step of clock.fixed_dt() */ }
/// clock.finish();
/// let alpha = clock.alpha; // overstep fraction: lerp(prev, curr, alpha)
/// ```
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    /// Constant simulation step: `1 / hz` seconds rounded to the nearest
    /// nanosecond.
    fixed_step: Duration,
    /// Unconsumed supplied time carried between frames.
    accumulator: Duration,
    /// `None` is the render-paced default.
    catch_up: Option<CatchUp>,
    /// Overstep fraction in `[0, 1)`, set by [`finish`](Self::finish); the
    /// render interpolation `alpha`. Carried debt is not part of it.
    pub alpha: f32,
    /// Fixed steps consumed during the current frame (cadence diagnostic).
    pub steps_this_frame: u32,
    /// Supplied time this frame will never simulate, set by
    /// [`begin_frame`](Self::begin_frame): the clamped-off delta when
    /// render-paced, whole discarded steps under a catch-up policy.
    pub dropped_dt: Duration,
}

impl FixedClock {
    /// A render-paced clock at the default 60 Hz simulation rate.
    #[must_use]
    pub fn new() -> Self {
        Self::with_hz(DEFAULT_HZ)
    }

    /// A render-paced clock at an arbitrary fixed rate: at most one step per
    /// frame. The step is `Duration::from_secs_f64(1.0 / f64::from(hz))`;
    /// `hz` must be positive and give a step of at least one nanosecond.
    #[must_use]
    pub fn with_hz(hz: f32) -> Self {
        let fixed_step = Duration::try_from_secs_f64(1.0 / f64::from(hz))
            .ok()
            .filter(|step| !step.is_zero())
            .unwrap_or_else(|| {
                panic!("FixedClock: hz must be positive with a step of at least 1 ns, got {hz}")
            });
        Self {
            fixed_step,
            accumulator: Duration::ZERO,
            catch_up: None,
            alpha: 0.0,
            steps_this_frame: 0,
            dropped_dt: Duration::ZERO,
        }
    }

    /// A clock that follows elapsed time with bounded catch-up (`hz` as for
    /// [`with_hz`](Self::with_hz) and `max_steps_per_frame` at least 1).
    #[must_use]
    pub fn with_catch_up(hz: f32, catch_up: CatchUp) -> Self {
        assert!(
            catch_up.max_steps_per_frame > 0,
            "FixedClock: max_steps_per_frame must be at least 1"
        );
        Self {
            catch_up: Some(catch_up),
            ..Self::with_hz(hz)
        }
    }

    /// The exact simulation step.
    #[must_use]
    pub fn fixed_step(&self) -> Duration {
        self.fixed_step
    }

    /// The simulation step in seconds, for the game's integration.
    #[must_use]
    pub fn fixed_dt(&self) -> f32 {
        self.fixed_step.as_secs_f32()
    }

    /// Begin a rendered frame: add the frame's elapsed time to the
    /// accumulator under the clock's policy, record
    /// [`dropped_dt`](Self::dropped_dt), and reset the per-frame step
    /// counter.
    pub fn begin_frame(&mut self, elapsed: Duration) {
        self.steps_this_frame = 0;
        let Some(policy) = self.catch_up else {
            let added = elapsed.min(self.fixed_step);
            self.dropped_dt = elapsed.saturating_sub(added);
            self.accumulator = self.accumulator.saturating_add(added);
            return;
        };
        self.dropped_dt = Duration::ZERO;
        self.accumulator = self.accumulator.saturating_add(elapsed);
        let step = self.fixed_step.as_nanos();
        let held = self.accumulator.as_nanos();
        let kept_steps = u128::from(policy.max_steps_per_frame) + u128::from(policy.max_debt_steps);
        if held / step > kept_steps {
            // Keep `kept_steps` whole steps and the fractional remainder, so
            // `alpha` stays continuous across the drop.
            let retained = kept_steps * step + held % step;
            self.dropped_dt = Duration::from_nanos_u128(held - retained);
            self.accumulator = self.accumulator.saturating_sub(self.dropped_dt);
        }
    }

    /// Consume one fixed step if enough time has accumulated and the frame's
    /// step budget allows. Drive the fixed simulation in a
    /// `while clock.step() { .. }` loop.
    pub fn step(&mut self) -> bool {
        let budget = self.catch_up.map_or(u32::MAX, |p| p.max_steps_per_frame);
        if self.accumulator >= self.fixed_step && self.steps_this_frame < budget {
            self.accumulator = self.accumulator.saturating_sub(self.fixed_step);
            self.steps_this_frame += 1;
            true
        } else {
            false
        }
    }

    /// Finish the frame: compute the render interpolation `alpha` from the
    /// leftover accumulator (the overstep fraction, excluding whole steps of
    /// carried debt).
    pub fn finish(&mut self) {
        let step = self.fixed_step.as_nanos();
        // The rounded f64 ratio only feeds presentation.
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let fraction = ((self.accumulator.as_nanos() % step) as f64 / step as f64) as f32;
        self.alpha = fraction.min(ALPHA_MAX);
    }
}

impl Default for FixedClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// The 30 Hz step worked by hand: 1 s / 30 to the nearest nanosecond.
    const STEP_30HZ_NS: u128 = 33_333_333;

    fn catch_up(max_steps_per_frame: u32, max_debt_steps: u32) -> CatchUp {
        CatchUp {
            max_steps_per_frame,
            max_debt_steps,
        }
    }

    /// Run one frame and return the steps it took.
    fn frame(clock: &mut FixedClock, elapsed: Duration) -> u32 {
        clock.begin_frame(elapsed);
        while clock.step() {}
        clock.finish();
        assert!((0.0..1.0).contains(&clock.alpha), "alpha {}", clock.alpha);
        clock.steps_this_frame
    }

    /// The fraction of a 30 Hz step left over after `elapsed_ns`.
    #[allow(clippy::cast_precision_loss)]
    fn phase_30hz(elapsed_ns: u128) -> f64 {
        (elapsed_ns % STEP_30HZ_NS) as f64 / STEP_30HZ_NS as f64
    }

    /// #249: the per-frame clamp follows the configured rate, not 60 Hz: a
    /// 30 Hz clock fed half a second runs exactly one step and rests,
    /// reporting the rest of the half second as dropped.
    #[wasm_bindgen_test(unsupported = test)]
    fn custom_hz_clamps_to_its_own_step() {
        let mut clock = FixedClock::with_hz(30.0);
        assert_eq!(frame(&mut clock, Duration::from_millis(500)), 1);
        assert_eq!(clock.alpha.to_bits(), 0);
        assert_eq!(clock.dropped_dt.as_nanos(), 500_000_000 - STEP_30HZ_NS);
    }

    /// Drive a 60 Hz render-paced clock with a constant frame time
    /// `1 / render_hz` for `frames` frames and return the per-frame steps.
    fn run(render_hz: f64, frames: usize) -> Vec<u32> {
        let mut clock = FixedClock::new();
        let elapsed = Duration::from_secs_f64(1.0 / render_hz);
        (0..frames).map(|_| frame(&mut clock, elapsed)).collect()
    }

    /// The core guarantee: with the clamp, cadence stays at <=1 step per frame
    /// and never oscillates 1<->2, across common render rates.
    #[wasm_bindgen_test(unsupported = test)]
    fn cadence_never_oscillates_with_clamp() {
        for hz in [60.0, 75.0, 120.0, 144.0] {
            let counts = run(hz, 600);
            // Skip warm-up: the first frames may run 0 steps while the
            // accumulator fills. Steady-state cadence is what matters.
            let steady = &counts[2..];
            assert!(
                steady.iter().all(|&c| c <= 1),
                "{hz} Hz: a frame ran >1 step (sample: {:?})",
                &steady[..steady.len().min(20)]
            );
        }
    }

    /// At exactly the fixed rate (60 Hz) the steady cadence is a solid 1 step
    /// per frame (the historically jittery case).
    #[wasm_bindgen_test(unsupported = test)]
    fn at_fixed_rate_cadence_is_one() {
        let counts = run(60.0, 300);
        let steady = &counts[2..];
        assert!(
            steady.iter().all(|&c| c == 1),
            "60 Hz steady cadence should be all 1s, got sample: {:?}",
            &steady[..steady.len().min(20)]
        );
    }

    /// Faster-than-fixed render rates never exceed 1 step/frame.
    #[wasm_bindgen_test(unsupported = test)]
    fn faster_render_never_exceeds_one_step() {
        for hz in [120.0, 144.0, 240.0] {
            let counts = run(hz, 300);
            assert!(
                counts.iter().all(|&c| c <= 1),
                "{hz} Hz exceeded 1 step/frame"
            );
        }
    }

    /// At half the fixed rate (30 Hz render) roughly one step per frame still
    /// runs (the clamp caps each frame at one step of virtual time).
    #[wasm_bindgen_test(unsupported = test)]
    fn slow_render_is_clamped_to_one_step() {
        let counts = run(30.0, 300);
        let steady = &counts[2..];
        assert!(
            steady.iter().all(|&c| c == 1),
            "30 Hz should clamp to exactly 1 step/frame, got sample: {:?}",
            &steady[..steady.len().min(20)]
        );
    }

    /// A game that times frames with `Duration::from_secs_f64(1.0 / 30.0)`
    /// at 30 Hz gets one tick per frame from the first frame under either
    /// policy: ticks so far are the elapsed nanoseconds divided by the step.
    #[wasm_bindgen_test(unsupported = test)]
    fn one_step_frames_tick_from_the_first_frame() {
        let elapsed = Duration::from_secs_f64(1.0 / 30.0);
        for mut clock in [
            FixedClock::with_hz(30.0),
            FixedClock::with_catch_up(30.0, catch_up(8, 0)),
        ] {
            let mut ticks = 0;
            for n in 1..=300 {
                ticks += u128::from(frame(&mut clock, elapsed));
                assert_eq!(ticks, n * elapsed.as_nanos() / STEP_30HZ_NS, "frame {n}");
                assert!(clock.dropped_dt.is_zero());
            }
        }
    }

    /// #270: with enough budget, simulation follows elapsed time below the
    /// tick rate. One second as ten 100 ms frames at 30 Hz is exactly
    /// floor(1 s / step) ticks; the render-paced default runs one per frame.
    #[wasm_bindgen_test(unsupported = test)]
    fn catch_up_follows_elapsed_time() {
        let (elapsed, frames) = (Duration::from_millis(100), 10);
        let mut clock = FixedClock::with_catch_up(30.0, catch_up(8, 0));
        let ticks: u32 = (0..frames).map(|_| frame(&mut clock, elapsed)).sum();
        assert_eq!(u128::from(ticks), 1_000_000_000 / STEP_30HZ_NS);
        assert!(clock.dropped_dt.is_zero());

        let mut paced = FixedClock::with_hz(30.0);
        let ticks: u32 = (0..frames).map(|_| frame(&mut paced, elapsed)).sum();
        assert_eq!(ticks, frames);
    }

    /// After multi-step frames, `alpha` is the fraction of a step left over
    /// from elapsed time: 110 ms frames at 30 Hz are 3.3 steps each, so no
    /// tested frame ends on a whole step.
    #[wasm_bindgen_test(unsupported = test)]
    fn catch_up_interpolates_the_elapsed_remainder() {
        let elapsed = Duration::from_millis(110);
        let mut clock = FixedClock::with_catch_up(30.0, catch_up(8, 0));
        let mut ticks = 0;
        for n in 1..=8 {
            ticks += u128::from(frame(&mut clock, elapsed));
            let elapsed_ns = n * elapsed.as_nanos();
            assert_eq!(ticks, elapsed_ns / STEP_30HZ_NS, "frame {n}");
            let want = phase_30hz(elapsed_ns);
            assert!(
                (f64::from(clock.alpha) - want).abs() < 1e-6,
                "frame {n}: alpha {} want {want}",
                clock.alpha
            );
        }
    }

    /// A 5 s stall at 30 Hz is 150 whole steps and 50 ns. With a budget of 8
    /// and no debt the frame runs 8, drops exactly the other 142 whole
    /// steps, and keeps the 50 ns phase; the next 1/30 s frame runs one.
    #[wasm_bindgen_test(unsupported = test)]
    fn five_second_stall_drops_whole_steps_and_keeps_the_phase() {
        let stall_ns = 5_000_000_000;
        let mut clock = FixedClock::with_catch_up(30.0, catch_up(8, 0));
        assert_eq!(frame(&mut clock, Duration::from_secs(5)), 8);
        assert_eq!(clock.dropped_dt.as_nanos(), 142 * STEP_30HZ_NS);
        let phase = phase_30hz(stall_ns);
        assert!((f64::from(clock.alpha) - phase).abs() < 1e-9);

        assert_eq!(frame(&mut clock, Duration::from_secs_f64(1.0 / 30.0)), 1);
        assert!(clock.dropped_dt.is_zero());
        assert!((f64::from(clock.alpha) - phase).abs() < 1e-9);
    }

    /// A stall of 150 and a half steps at 30 Hz. The budget runs
    /// `max_steps_per_frame`, carries at most `max_debt_steps`, and drops
    /// exactly the rest; the following one-step frames repay the debt at up
    /// to a full budget each, then run one step, keeping the half step.
    #[wasm_bindgen_test(unsupported = test)]
    fn stall_is_capped_and_excess_follows_the_policy() {
        let half = STEP_30HZ_NS / 2;
        let stall = Duration::from_nanos_u128(150 * STEP_30HZ_NS + half);
        let one_step = Duration::from_nanos_u128(STEP_30HZ_NS);
        let phase = phase_30hz(half);
        for (budget, debt) in [(8_u32, 0_u32), (8, 4), (8, 20)] {
            let mut clock = FixedClock::with_catch_up(30.0, catch_up(budget, debt));
            assert_eq!(frame(&mut clock, stall), budget);
            let dropped_steps = 150 - u128::from(budget + debt);
            assert_eq!(clock.dropped_dt.as_nanos(), dropped_steps * STEP_30HZ_NS);
            assert!((f64::from(clock.alpha) - phase).abs() < 1e-6);

            // Each normal frame adds one step to the carried debt.
            let mut owed = debt;
            for n in 0..6 {
                let want = (owed + 1).min(budget);
                assert_eq!(
                    frame(&mut clock, one_step),
                    want,
                    "{budget}/{debt} frame {n}"
                );
                assert!(clock.dropped_dt.is_zero());
                assert!((f64::from(clock.alpha) - phase).abs() < 1e-6);
                owed = owed + 1 - want;
            }
        }
    }

    /// The longest representable stall, twice in a row, saturates instead of
    /// overflowing: a render-paced frame drops all but one step, a catch-up
    /// frame runs its budget, and later one-step frames tick once each.
    #[wasm_bindgen_test(unsupported = test)]
    fn maximal_stall_saturates_and_recovers() {
        let one_step = Duration::from_nanos_u128(STEP_30HZ_NS);
        let mut paced = FixedClock::with_hz(30.0);
        for _ in 0..2 {
            assert_eq!(frame(&mut paced, Duration::MAX), 1);
            assert_eq!(paced.dropped_dt, Duration::MAX.saturating_sub(one_step));
        }
        let mut caught_up = FixedClock::with_catch_up(30.0, catch_up(8, 4));
        for _ in 0..2 {
            assert_eq!(frame(&mut caught_up, Duration::MAX), 8);
        }
        for mut clock in [paced, caught_up] {
            // Drain any carried debt, then count steps over 30 frames.
            for _ in 0..4 {
                frame(&mut clock, one_step);
            }
            let ticks: u32 = (0..30).map(|_| frame(&mut clock, one_step)).sum();
            assert_eq!(ticks, 30);
            assert!(clock.dropped_dt.is_zero());
        }
    }
}
