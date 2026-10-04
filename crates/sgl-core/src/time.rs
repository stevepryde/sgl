//! Fixed-step clock with render interpolation.
//!
//! The loop accumulates real frame time and runs the fixed-step simulation in
//! constant `1/hz` slices. The per-frame delta fed into the accumulator is
//! **clamped** to one fixed step so the accumulator never routinely crosses
//! the 2-step threshold when the render rate sits near the fixed rate — the
//! proven fix for the 60 Hz "rubber-banding" cadence oscillation. The clamp
//! trades real-time catch-up under heavy stalls for visual stability, which is
//! the right trade for a render-paced client loop.

/// Default simulation rate: 60 Hz.
pub const DEFAULT_HZ: f32 = 60.0;

/// Clamp on the per-frame delta, as a multiple of `fixed_dt`. At 1.0 a frame
/// can consume at most one fixed step — the anti-oscillation choice.
const MAX_FRAME_DT_MULTIPLIER: f32 = 1.0;

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
    /// Clamp on the per-frame delta fed into the accumulator (seconds).
    max_frame_dt: f32,
    /// Overstep fraction in `[0, 1)`, set by [`finish`](Self::finish); the
    /// render interpolation `alpha`.
    pub alpha: f32,
    /// Fixed steps consumed during the current frame (cadence diagnostic).
    pub steps_this_frame: u32,
}

impl FixedClock {
    /// A clock at the default 60 Hz simulation rate.
    #[must_use]
    pub fn new() -> Self {
        Self::with_hz(DEFAULT_HZ)
    }

    /// A clock at an arbitrary fixed rate (`hz` must be positive).
    #[must_use]
    pub fn with_hz(hz: f32) -> Self {
        assert!(hz > 0.0, "FixedClock: hz must be positive, got {hz}");
        let fixed_dt = 1.0 / hz;
        Self {
            fixed_dt,
            accumulator: 0.0,
            max_frame_dt: fixed_dt * MAX_FRAME_DT_MULTIPLIER,
            alpha: 0.0,
            steps_this_frame: 0,
        }
    }

    /// Begin a rendered frame: add `real_dt` (clamped) to the accumulator and
    /// reset the per-frame step counter.
    pub fn begin_frame(&mut self, real_dt: f32) {
        self.accumulator += real_dt.min(self.max_frame_dt);
        self.steps_this_frame = 0;
    }

    /// Consume one fixed step if enough time has accumulated. Drive the fixed
    /// simulation in a `while clock.step() { .. }` loop.
    pub fn step(&mut self) -> bool {
        if self.accumulator >= self.fixed_dt {
            self.accumulator -= self.fixed_dt;
            self.steps_this_frame += 1;
            true
        } else {
            false
        }
    }

    /// Finish the frame: compute the render interpolation `alpha` from the
    /// leftover accumulator (the overstep fraction).
    pub fn finish(&mut self) {
        self.alpha = self.accumulator / self.fixed_dt;
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

    /// A custom-rate clock steps at its own cadence.
    #[wasm_bindgen_test(unsupported = test)]
    fn custom_hz_sets_fixed_dt() {
        let clock = FixedClock::with_hz(30.0);
        assert!((clock.fixed_dt - 1.0 / 30.0).abs() < f32::EPSILON);
    }
}
