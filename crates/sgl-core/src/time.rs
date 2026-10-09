//! Fixed-step clock with render interpolation.
//!
//! The loop accumulates caller-supplied frame time and runs the fixed-step
//! simulation in constant `1/hz` slices. A clock picks one of two policies:
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

/// Default simulation rate: 60 Hz.
pub const DEFAULT_HZ: f32 = 60.0;

/// Clamp on the per-frame delta, as a multiple of `fixed_dt`. At 1.0 a frame
/// can consume at most one fixed step — the anti-oscillation choice.
const MAX_FRAME_DT_MULTIPLIER: f32 = 1.0;

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
/// clock.begin_frame(real_dt);
/// while clock.step() { /* one fixed sim step of clock.fixed_dt */ }
/// clock.finish();
/// let alpha = clock.alpha; // overstep fraction: lerp(prev, curr, alpha)
/// ```
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    /// Constant simulation step, `1 / hz` seconds.
    pub fixed_dt: f32,
    /// Unconsumed real time carried between frames (seconds).
    accumulator: f32,
    /// Clamp on the per-frame delta fed into the accumulator (seconds);
    /// unbounded under a catch-up policy.
    max_frame_dt: f32,
    /// `None` is the render-paced default.
    catch_up: Option<CatchUp>,
    /// Overstep fraction in `[0, 1)`, set by [`finish`](Self::finish); the
    /// render interpolation `alpha`. Carried debt is not part of it.
    pub alpha: f32,
    /// Fixed steps consumed during the current frame (cadence diagnostic).
    pub steps_this_frame: u32,
    /// Supplied time this frame will never simulate (seconds), set by
    /// [`begin_frame`](Self::begin_frame): the clamped-off delta when
    /// render-paced, whole discarded steps under a catch-up policy.
    pub dropped_dt: f32,
}

impl FixedClock {
    /// A render-paced clock at the default 60 Hz simulation rate.
    #[must_use]
    pub fn new() -> Self {
        Self::with_hz(DEFAULT_HZ)
    }

    /// A render-paced clock at an arbitrary fixed rate (`hz` must be
    /// positive): at most one step per frame.
    #[must_use]
    pub fn with_hz(hz: f32) -> Self {
        assert!(hz > 0.0, "FixedClock: hz must be positive, got {hz}");
        let fixed_dt = 1.0 / hz;
        Self {
            fixed_dt,
            accumulator: 0.0,
            max_frame_dt: fixed_dt * MAX_FRAME_DT_MULTIPLIER,
            catch_up: None,
            alpha: 0.0,
            steps_this_frame: 0,
            dropped_dt: 0.0,
        }
    }

    /// A clock that follows elapsed time with bounded catch-up (`hz` must be
    /// positive and `max_steps_per_frame` at least 1).
    #[must_use]
    pub fn with_catch_up(hz: f32, catch_up: CatchUp) -> Self {
        assert!(
            catch_up.max_steps_per_frame > 0,
            "FixedClock: max_steps_per_frame must be at least 1"
        );
        Self {
            // Finite, so an infinite or NaN delta is a maximal stall.
            max_frame_dt: f32::MAX,
            catch_up: Some(catch_up),
            ..Self::with_hz(hz)
        }
    }

    /// Begin a rendered frame: add `real_dt` to the accumulator under the
    /// clock's policy, record [`dropped_dt`](Self::dropped_dt), and reset
    /// the per-frame step counter.
    pub fn begin_frame(&mut self, real_dt: f32) {
        let added = real_dt.min(self.max_frame_dt);
        self.dropped_dt = (real_dt - added).max(0.0);
        self.accumulator += added;
        self.steps_this_frame = 0;
        if let Some(policy) = self.catch_up {
            let kept_steps = policy
                .max_steps_per_frame
                .saturating_add(policy.max_debt_steps);
            // Exact for any budget below 2^24 steps; larger is meaningless.
            #[allow(clippy::cast_precision_loss)]
            let kept = kept_steps as f32 * self.fixed_dt;
            if self.accumulator >= kept + self.fixed_dt {
                // Keep `kept_steps` whole steps and the exact fractional
                // remainder, so `alpha` stays continuous across the drop.
                let retained = (kept + self.accumulator % self.fixed_dt).min(self.accumulator);
                self.dropped_dt += self.accumulator - retained;
                self.accumulator = retained;
            }
        }
        self.dropped_dt = self.dropped_dt.min(f32::MAX);
    }

    /// Consume one fixed step if enough time has accumulated and the frame's
    /// step budget allows. Drive the fixed simulation in a
    /// `while clock.step() { .. }` loop.
    pub fn step(&mut self) -> bool {
        let budget = self.catch_up.map_or(u32::MAX, |p| p.max_steps_per_frame);
        if self.accumulator >= self.fixed_dt && self.steps_this_frame < budget {
            self.accumulator -= self.fixed_dt;
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
        self.alpha = ((self.accumulator % self.fixed_dt).max(0.0) / self.fixed_dt).min(ALPHA_MAX);
    }
}

impl Default for FixedClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #249: the per-frame clamp follows the configured rate, not 60 Hz: a
    /// 30 Hz clock fed half a second runs exactly one step and rests.
    #[wasm_bindgen_test(unsupported = test)]
    fn custom_hz_clamps_to_its_own_step() {
        let mut clock = FixedClock::with_hz(30.0);
        clock.begin_frame(0.5);
        assert!(clock.step());
        assert!(!clock.step());
        clock.finish();
        assert_eq!(clock.alpha.to_bits(), 0);
        assert_eq!(clock.steps_this_frame, 1);
    }

    /// Drive the clock with a synthetic constant `real_dt` for `frames` frames
    /// and return the per-frame step counts, asserting the alpha contract.
    fn run(real_dt: f32, frames: usize) -> Vec<u32> {
        let mut clock = FixedClock::new();
        let mut counts = Vec::with_capacity(frames);
        for _ in 0..frames {
            clock.begin_frame(real_dt);
            while clock.step() {}
            clock.finish();
            counts.push(clock.steps_this_frame);
            assert!(
                (0.0..1.0).contains(&clock.alpha),
                "alpha must be in [0,1): got {}",
                clock.alpha
            );
        }
        counts
    }

    /// The core guarantee: with the clamp, cadence stays at <=1 step per frame
    /// and never oscillates 1<->2, across common render rates.
    #[wasm_bindgen_test(unsupported = test)]
    fn cadence_never_oscillates_with_clamp() {
        for hz in [60.0_f32, 75.0, 120.0, 144.0] {
            let counts = run(1.0 / hz, 600);
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
        let counts = run(1.0 / 60.0, 300);
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
        for hz in [120.0_f32, 144.0, 240.0] {
            let counts = run(1.0 / hz, 300);
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
        let counts = run(1.0 / 30.0, 300);
        let steady = &counts[2..];
        assert!(
            steady.iter().all(|&c| c == 1),
            "30 Hz should clamp to exactly 1 step/frame, got sample: {:?}",
            &steady[..steady.len().min(20)]
        );
    }

    /// Run one frame and return the steps it took.
    fn frame(clock: &mut FixedClock, real_dt: f32) -> u32 {
        clock.begin_frame(real_dt);
        while clock.step() {}
        clock.finish();
        assert!((0.0..1.0).contains(&clock.alpha), "alpha {}", clock.alpha);
        clock.steps_this_frame
    }

    /// #270: with enough budget, simulation follows elapsed time below the
    /// tick rate. One second as ten 100 ms frames at 30 Hz is ~30 ticks;
    /// the render-paced default runs one per frame.
    #[wasm_bindgen_test(unsupported = test)]
    fn catch_up_follows_elapsed_time() {
        let (hz, dt, frames) = (30.0_f32, 0.1_f32, 10);
        let policy = CatchUp {
            max_steps_per_frame: 8,
            max_debt_steps: 0,
        };
        let mut clock = FixedClock::with_catch_up(hz, policy);
        let ticks: u32 = (0..frames).map(|_| frame(&mut clock, dt)).sum();
        let expected = (dt * frames as f32 * hz).round() as u32;
        assert!(
            ticks.abs_diff(expected) <= 1,
            "{ticks} ticks, want ~{expected}"
        );
        assert_eq!(clock.dropped_dt.to_bits(), 0);

        let mut paced = FixedClock::with_hz(hz);
        let ticks: u32 = (0..frames).map(|_| frame(&mut paced, dt)).sum();
        assert_eq!(ticks, frames);
    }

    /// After multi-step frames, `alpha` is the fraction of a step left over
    /// from elapsed time: 0.11 s frames at 30 Hz are 3.3 steps each, so no
    /// tested frame ends on a whole step.
    #[wasm_bindgen_test(unsupported = test)]
    fn catch_up_interpolates_the_elapsed_remainder() {
        let (hz, dt) = (30.0_f32, 0.11_f32);
        let policy = CatchUp {
            max_steps_per_frame: 8,
            max_debt_steps: 0,
        };
        let mut clock = FixedClock::with_catch_up(hz, policy);
        let mut ticks = 0;
        for n in 1..=8_u32 {
            ticks += frame(&mut clock, dt);
            let due = f64::from(n) * f64::from(dt) * f64::from(hz);
            assert_eq!(ticks, due.floor() as u32, "frame {n}");
            let fraction = (due - due.floor()) as f32;
            assert!(
                (clock.alpha - fraction).abs() < 1e-3,
                "frame {n}: alpha {}",
                clock.alpha
            );
        }
    }

    /// A 5.5-step-past-five-second stall at 30 Hz is 150.5 steps due. The
    /// budget runs `max_steps_per_frame`, carries at most `max_debt_steps`,
    /// and drops the rest; the following 1/30 s frames repay the debt at up
    /// to a full budget each, then run one step, keeping the half step.
    #[wasm_bindgen_test(unsupported = test)]
    fn stall_is_capped_and_excess_follows_the_policy() {
        let hz = 30.0_f32;
        let step = 1.0 / hz;
        let stall = 150.5 * step;
        let due = 150_u32;
        for (budget, debt) in [(8_u32, 0_u32), (8, 4), (8, 20)] {
            let policy = CatchUp {
                max_steps_per_frame: budget,
                max_debt_steps: debt,
            };
            let mut clock = FixedClock::with_catch_up(hz, policy);
            assert_eq!(frame(&mut clock, stall), budget);
            let dropped_steps = clock.dropped_dt / step;
            let want_dropped = (due - budget - debt) as f32;
            assert!(
                (dropped_steps - want_dropped).abs() < 1e-2,
                "{budget}/{debt}: dropped {dropped_steps} steps, want {want_dropped}"
            );
            assert!((clock.alpha - 0.5).abs() < 1e-3);

            // Each normal frame adds one step to the carried debt.
            let mut owed = debt;
            for n in 0..6 {
                let want = (owed + 1).min(budget);
                assert_eq!(frame(&mut clock, step), want, "{budget}/{debt} frame {n}");
                assert_eq!(clock.dropped_dt.to_bits(), 0);
                assert!((clock.alpha - 0.5).abs() < 1e-3);
                owed = owed + 1 - want;
            }
        }
    }

    /// An infinite or NaN delta is a maximal stall, never a poisoned clock:
    /// `dropped_dt` stays finite and later 1/30 s frames at 30 Hz run one
    /// step each (two at most, carrying the stall's fractional remainder).
    #[wasm_bindgen_test(unsupported = test)]
    fn non_finite_delta_recovers_on_the_next_frame() {
        let hz = 30.0_f32;
        let policy = CatchUp {
            max_steps_per_frame: 8,
            max_debt_steps: 4,
        };
        for bad in [f32::INFINITY, f32::NAN] {
            for mut clock in [
                FixedClock::with_hz(hz),
                FixedClock::with_catch_up(hz, policy),
            ] {
                frame(&mut clock, bad);
                assert!(clock.dropped_dt.is_finite(), "{bad}: {}", clock.dropped_dt);
                // Drain any carried debt, then count steps over 30 frames.
                for _ in 0..4 {
                    frame(&mut clock, 1.0 / hz);
                }
                let frames = 30_u32;
                let ticks: u32 = (0..frames).map(|_| frame(&mut clock, 1.0 / hz)).sum();
                assert!(
                    ticks.abs_diff(frames) <= 1,
                    "{bad}: {ticks} ticks in {frames} frames"
                );
                assert_eq!(clock.dropped_dt.to_bits(), 0);
            }
        }
    }

    /// A custom-rate clock steps at its own cadence.
    #[wasm_bindgen_test(unsupported = test)]
    fn custom_hz_sets_fixed_dt() {
        let clock = FixedClock::with_hz(30.0);
        assert!((clock.fixed_dt - 1.0 / 30.0).abs() < f32::EPSILON);
    }
}
