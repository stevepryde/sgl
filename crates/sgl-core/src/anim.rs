//! Pure frame-animation driver over an explicit **frame order** array.
//!
//! Ping-pong and other patterns are authored as sheet-frame order data. The
//! driver owns a cursor into the order array
//! and an elapsed-time accumulator; the caller queries
//! [`current_frame`](FrameAnimation::current_frame) and cuts the sheet.
//!
//! Pure `core`: no render, no wgpu, no clocks — [`tick`](FrameAnimation::tick)
//! takes the caller's `dt`.
//!
//! [`AnimationSequence`] is the sibling driver for animations scripted as a
//! list of steps: frame runs, pauses, random pauses, and action hooks, with
//! ping-pong loop modes.

mod sequence;

pub use sequence::{AnimationSequence, SequenceBuilder, SequenceError, SequenceLoop, SequenceStep};

/// Loop behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopMode {
    /// Play the order once, then hold the last frame and report finished.
    Once,
    /// Wrap back to the start of the order forever.
    Repeat,
}

/// A playing frame animation: an explicit frame-order array stepped at a
/// fixed fps.
///
/// Frame `order[i]` is displayed for `t ∈ [i·d, (i+1)·d)` where `d = 1/fps`;
/// accumulating **exactly** one frame duration advances the cursor.
#[derive(Debug, Clone)]
pub struct FrameAnimation {
    /// Sheet-frame indices in play order (e.g. `[0, 1, 2, 1]` ping-pong as
    /// data). Never empty.
    order: Vec<usize>,
    /// Seconds each order entry is displayed (`1 / fps`).
    frame_duration: f32,
    loop_mode: LoopMode,
    /// Cursor into `order`.
    position: usize,
    /// Elapsed time within the current order entry (seconds).
    timer: f32,
    finished: bool,
}

impl FrameAnimation {
    /// An animation playing `order` at `fps`. Starts at the first entry.
    ///
    /// # Panics
    /// If `order` is empty or `fps` is not positive — both are authoring
    /// errors, not runtime conditions.
    pub fn new(order: impl Into<Vec<usize>>, fps: f32, loop_mode: LoopMode) -> Self {
        let order = order.into();
        assert!(!order.is_empty(), "FrameAnimation: empty frame order");
        assert!(fps > 0.0, "FrameAnimation: fps must be positive, got {fps}");
        Self {
            order,
            frame_duration: 1.0 / fps,
            loop_mode,
            position: 0,
            timer: 0.0,
            finished: false,
        }
    }

    /// Start at `position` within the order (clamped to the last entry) —
    /// For example, an animation may start at order position 1.
    #[must_use]
    pub fn start_at(mut self, position: usize) -> Self {
        self.position = position.min(self.order.len() - 1);
        self
    }

    /// Advance by `dt` seconds. Returns `true` only on the tick where a
    /// [`LoopMode::Once`] animation finishes (a completion edge, once).
    ///
    /// Large `dt` steps multiple frames; the leftover carries into the new
    /// frame so long-run timing never drifts.
    pub fn tick(&mut self, dt: f32) -> bool {
        if self.finished {
            return false;
        }
        self.timer += dt;
        while self.timer >= self.frame_duration {
            self.timer -= self.frame_duration;
            if self.position + 1 < self.order.len() {
                self.position += 1;
            } else {
                match self.loop_mode {
                    LoopMode::Repeat => self.position = 0,
                    LoopMode::Once => {
                        // Hold the last frame; drop leftover time.
                        self.finished = true;
                        self.timer = 0.0;
                        return true;
                    }
                }
            }
        }
        false
    }

    /// The sheet-frame index to display right now.
    #[must_use]
    pub fn current_frame(&self) -> usize {
        self.order[self.position]
    }

    /// The cursor position within the order array.
    #[must_use]
    pub fn position(&self) -> usize {
        self.position
    }

    /// Whether a [`LoopMode::Once`] animation has played out.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Rewind to the first order entry (restarts a finished animation).
    pub fn reset(&mut self) {
        self.position = 0;
        self.timer = 0.0;
        self.finished = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #249: `start_at` positions the cursor (clamped to the order's end) and
    /// `position` reports it; ticking continues from there.
    #[wasm_bindgen_test(unsupported = test)]
    fn start_at_positions_the_cursor() {
        let anim = FrameAnimation::new(vec![5, 6, 7, 8], 10.0, LoopMode::Repeat).start_at(2);
        assert_eq!(anim.position(), 2);
        assert_eq!(anim.current_frame(), 7);
        let mut anim = anim;
        anim.tick(0.1);
        assert_eq!(anim.position(), 3);
        let clamped = FrameAnimation::new(vec![5, 6], 10.0, LoopMode::Repeat).start_at(9);
        assert_eq!(clamped.position(), 1);
    }

    /// A `[0,1,2,1]` order at 10 fps, repeating. Exact 0.1 s
    /// ticks advance exactly one order entry each (boundary semantics).
    #[wasm_bindgen_test(unsupported = test)]
    fn repeat_order_cycles_at_exact_fps_boundaries() {
        let mut a = FrameAnimation::new([0, 1, 2, 1], 10.0, LoopMode::Repeat);
        let mut seen = vec![a.current_frame()];
        for _ in 0..8 {
            assert!(!a.tick(0.1), "Repeat never reports finished");
            seen.push(a.current_frame());
        }
        assert_eq!(seen, vec![0, 1, 2, 1, 0, 1, 2, 1, 0]);
    }

    /// Just below a frame boundary the frame must NOT advance; reaching it
    /// exactly must.
    #[wasm_bindgen_test(unsupported = test)]
    fn sub_boundary_tick_does_not_advance() {
        let mut a = FrameAnimation::new([0, 1], 10.0, LoopMode::Repeat);
        a.tick(0.099);
        assert_eq!(a.current_frame(), 0, "0.099 s < 0.1 s: still frame 0");
        a.tick(0.001);
        assert_eq!(a.current_frame(), 1, "accumulated exactly 0.1 s: frame 1");
    }

    /// A large dt steps several frames and carries the remainder (no drift).
    #[wasm_bindgen_test(unsupported = test)]
    fn large_dt_steps_multiple_frames() {
        let mut a = FrameAnimation::new([0, 1, 2, 1], 10.0, LoopMode::Repeat);
        a.tick(0.25); // 2 full frames + 0.05 leftover
        assert_eq!(a.current_frame(), 2);
        a.tick(0.05); // leftover reaches the 3rd boundary
        assert_eq!(a.current_frame(), 1);
    }

    /// `Once` holds the last frame, reports the completion edge exactly once,
    /// and stays finished.
    #[wasm_bindgen_test(unsupported = test)]
    fn once_finishes_on_the_edge_and_holds() {
        let mut a = FrameAnimation::new([0, 1, 2], 5.0, LoopMode::Once);
        assert!(!a.tick(0.2));
        assert_eq!(a.current_frame(), 1);
        assert!(!a.tick(0.2));
        assert_eq!(a.current_frame(), 2);
        // Stepping past the last frame finishes.
        assert!(a.tick(0.2), "completion edge");
        assert!(a.is_finished());
        assert_eq!(a.current_frame(), 2, "holds the last frame");
        // No re-fire, no movement.
        assert!(!a.tick(1.0));
        assert_eq!(a.current_frame(), 2);
    }

    /// An order `[0,2,1,2]` at 20 fps, starting at position 1.
    #[wasm_bindgen_test(unsupported = test)]
    fn chicken_starts_at_frame_two_and_follows_its_order() {
        let mut a = FrameAnimation::new([0, 2, 1, 2], 20.0, LoopMode::Repeat).start_at(1);
        let mut seen = vec![a.current_frame()];
        for _ in 0..5 {
            a.tick(0.05);
            seen.push(a.current_frame());
        }
        assert_eq!(seen, vec![2, 1, 2, 0, 2, 1]);
    }

    /// `reset` rewinds and un-finishes.
    #[wasm_bindgen_test(unsupported = test)]
    fn reset_restarts_a_finished_animation() {
        let mut a = FrameAnimation::new([3, 4], 10.0, LoopMode::Once);
        a.tick(1.0);
        assert!(a.is_finished());
        a.reset();
        assert!(!a.is_finished());
        assert_eq!(a.current_frame(), 3);
        assert!(!a.tick(0.1));
        assert_eq!(a.current_frame(), 4);
    }
}
