//! Multi-step frame animation: frame runs, pauses, random pauses, and action
//! hooks played as one sequence with four loop modes.
//!
//! [`FrameAnimation`](super::FrameAnimation) plays a single frame-order array
//! at a fixed fps. [`AnimationSequence`] is its sibling for animations that are
//! a *script*: "play the idle frames, then hold for two to five seconds, then
//! start over".
//!
//! Pure `core`: [`tick`](AnimationSequence::tick) takes the caller's `dt` and
//! the caller's [`Rng`] — random pauses never reach for global randomness, so a
//! seeded run is reproducible.
//!
//! ## Ping-pong
//!
//! The ends are the first and last frame steps; steps outside them (a trailing
//! action, a leading pause) play once at the turnaround and are not replayed.
//! Each pass starts one element in, so neither end plays twice. With one frame
//! step this counts frames — `[0, 1, 2]` under
//! [`SequenceLoop::PingPongOnce`] plays `0, 1, 2, 1, 0` and completes, and
//! [`SequenceLoop::PingPongRepeat`] continues `1, 2, 1, 0, …`. With several it
//! counts steps: the reverse pass resumes at the step before the last frame
//! step and enters each reversed step at its *last* frame, and a repeat
//! resumes at the step after the first frame step. A sequence without frame
//! steps bounces between its first and last steps the same way.

use crate::random::Rng;

/// One step of an [`AnimationSequence`].
#[derive(Debug, Clone, PartialEq)]
pub enum SequenceStep {
    /// Play `frames` in order, holding each for `frame_duration` seconds.
    PlayFrames {
        /// Sheet-frame indices in play order. Never empty.
        frames: Vec<usize>,
        /// Seconds each frame is displayed.
        frame_duration: f32,
    },
    /// Hold the previous frame for a fixed number of seconds.
    Pause(f32),
    /// Hold the previous frame for a duration drawn from `[min, max)` when the
    /// step is entered (exactly `min` when `min == max`).
    RandomPause {
        /// Shortest possible hold, in seconds.
        min: f32,
        /// Exclusive upper bound of the hold, in seconds.
        max: f32,
    },
    /// Queue a caller-defined action id and continue immediately. The caller
    /// drains it with [`AnimationSequence::take_action`].
    Action(u32),
}

/// Loop behavior for an [`AnimationSequence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceLoop {
    /// Play the steps once, then report finished.
    Once,
    /// Wrap back to the first step forever, rewinding every step.
    Repeat,
    /// Play forward, then back, then forward again, forever.
    PingPongRepeat,
    /// Play forward, then back, then report finished.
    PingPongOnce,
}

/// A playing multi-step animation.
///
/// The caller drives it with [`tick`](AnimationSequence::tick) and reads
/// [`current_frame`](AnimationSequence::current_frame). Pauses, actions, and
/// completed sequences hold the last frame reached during playback, including
/// frames crossed within a single tick. The frame is `None` only until playback
/// first reaches a frame after construction or [`reset`](Self::reset).
#[derive(Debug, Clone)]
pub struct AnimationSequence {
    /// The authored steps. Never empty.
    steps: Vec<SequenceStep>,
    /// Per-step frame cursor, parallel to `steps`.
    cursors: Vec<usize>,
    /// Most recent frame traversed, held while no frame step is active.
    held_frame: Option<usize>,
    loop_mode: SequenceLoop,
    /// Cursor into `steps`. Equals `steps.len()` once `Once` runs off the end.
    step: usize,
    /// Elapsed time within the current step element (seconds).
    timer: f32,
    /// Duration of the current step element, resolved lazily so that a random
    /// pause draws from the caller's [`Rng`] on the tick that enters it.
    step_duration: Option<f32>,
    /// Ping-pong direction.
    forward: bool,
    /// Action ids queued by this tick, in the order they fired.
    actions: std::collections::VecDeque<u32>,
    finished: bool,
    just_completed: bool,
    /// Playback positions (step, frame cursor and, for ping-pong, direction)
    /// in one loop cycle. More consecutive advances than this without
    /// consuming time revisit a position: a zero-time cycle.
    cycle_positions: usize,
}

impl AnimationSequence {
    /// Start building a sequence.
    #[must_use]
    pub fn builder() -> SequenceBuilder {
        SequenceBuilder::new()
    }

    /// Advance by `dt` seconds, drawing random-pause durations from `rng`.
    /// Returns `true` only on the tick where a [`SequenceLoop::Once`] or
    /// [`SequenceLoop::PingPongOnce`] sequence completes (a completion edge,
    /// once).
    ///
    /// Large `dt` steps through as many frames and steps as it covers and
    /// carries the remainder, so long-run timing never drifts. A non-finite
    /// `dt` is ignored. In the repeating modes, a cycle that consumes no time
    /// cannot spin: a tick stops after as many consecutive zero-time advances
    /// (zero-duration frames and steps, or durations too small to lower the
    /// accumulated time in `f32`) as the sequence has playback positions —
    /// frames plus other steps, doubled for ping-pong — and drops the
    /// remainder. Every position in the cycle has then played at least once.
    /// The play-once modes need no such stop: they end after one bounded
    /// pass.
    pub fn tick(&mut self, dt: f32, rng: &mut Rng) -> bool {
        self.just_completed = false;
        self.actions.clear();
        if self.finished || !dt.is_finite() {
            return false;
        }
        self.timer += dt;

        let repeating = matches!(
            self.loop_mode,
            SequenceLoop::Repeat | SequenceLoop::PingPongRepeat
        );
        let mut idle_advances = 0;
        loop {
            let duration = self.resolved_duration(rng);
            if self.timer < duration {
                return false;
            }
            let before = self.timer;
            self.timer -= duration;
            if self.timer < before {
                idle_advances = 0;
            } else if repeating {
                idle_advances += 1;
                if idle_advances > self.cycle_positions {
                    self.timer = 0.0;
                    return false;
                }
            }
            if self.advance() {
                self.finished = true;
                self.just_completed = true;
                self.timer = 0.0;
                return true;
            }
        }
    }

    /// The sheet-frame index to display right now. Pauses, actions, and
    /// completed sequences hold the last frame reached during playback.
    /// Returns `None` only before the first frame is reached after construction
    /// or [`reset`](Self::reset).
    #[must_use]
    pub fn current_frame(&self) -> Option<usize> {
        match self.steps.get(self.step) {
            Some(SequenceStep::PlayFrames { frames, .. }) => {
                frames.get(self.cursors[self.step]).copied()
            }
            _ => self.held_frame,
        }
    }

    /// Whether the sequence completed on the most recent
    /// [`tick`](AnimationSequence::tick).
    #[must_use]
    pub fn just_completed(&self) -> bool {
        self.just_completed
    }

    /// Whether a `Once` or `PingPongOnce` sequence has played out.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Take the next action id queued by the most recent
    /// [`tick`](AnimationSequence::tick), oldest first. Drain it in a loop: one
    /// tick can cross several [`SequenceStep::Action`] steps. The queue is
    /// cleared at the start of every tick.
    pub fn take_action(&mut self) -> Option<u32> {
        self.actions.pop_front()
    }

    /// Rewind to the first frame of the first step (restarts a finished
    /// sequence).
    pub fn reset(&mut self) {
        self.step = 0;
        self.timer = 0.0;
        self.step_duration = None;
        self.forward = true;
        self.finished = false;
        self.just_completed = false;
        self.actions.clear();
        self.cursors.fill(0);
        self.held_frame = None;
    }

    /// The current step element's duration, resolving (and remembering) a
    /// random pause on first use.
    fn resolved_duration(&mut self, rng: &mut Rng) -> f32 {
        if let Some(duration) = self.step_duration {
            return duration;
        }
        let duration = match &self.steps[self.step] {
            SequenceStep::PlayFrames { frame_duration, .. } => *frame_duration,
            SequenceStep::Pause(duration) => *duration,
            SequenceStep::RandomPause { min, max } => min + rng.f32() * (max - min),
            SequenceStep::Action(_) => 0.0,
        };
        self.step_duration = Some(duration);
        duration
    }

    /// Advance one frame within the current step, or move on to the next step.
    /// Returns `true` when the sequence completes.
    fn advance(&mut self) -> bool {
        let cursor = self.cursors[self.step];
        let next_frame = match &self.steps[self.step] {
            SequenceStep::PlayFrames { frames, .. } => {
                self.held_frame = Some(frames[cursor]);
                if self.forward {
                    (cursor + 1 < frames.len()).then_some(cursor + 1)
                } else {
                    cursor.checked_sub(1)
                }
            }
            _ => None,
        };
        if let Some(cursor) = next_frame {
            self.cursors[self.step] = cursor;
            self.step_duration = None;
            return false;
        }
        if let SequenceStep::Action(id) = &self.steps[self.step] {
            self.actions.push_back(*id);
        }
        self.next_step()
    }

    /// Move the step cursor per the loop mode. Returns `true` when the sequence
    /// completes.
    fn next_step(&mut self) -> bool {
        self.step_duration = None;
        match self.loop_mode {
            SequenceLoop::Once => {
                self.step += 1;
                self.step >= self.steps.len()
            }
            SequenceLoop::Repeat => {
                self.step += 1;
                if self.step >= self.steps.len() {
                    self.step = 0;
                    self.cursors.fill(0);
                }
                false
            }
            SequenceLoop::PingPongRepeat | SequenceLoop::PingPongOnce => {
                if self.forward {
                    if self.step + 1 < self.steps.len() {
                        self.step += 1;
                        self.cursors[self.step] = 0;
                        false
                    } else {
                        self.turn_back()
                    }
                } else if self.step > 0 {
                    self.step -= 1;
                    self.cursors[self.step] = self.element_count(self.step) - 1;
                    false
                } else {
                    self.restart_forward()
                }
            }
        }
    }

    /// Start the reverse pass after the forward pass played its last step.
    /// The end is the last frame shown: steps after the last frame step
    /// played once at the turnaround and are not replayed. With one frame
    /// step the pass resumes one frame inside it; with several it resumes at
    /// the step before the last frame step, entered at its last element.
    /// Returns `true` when a `PingPongOnce` completes.
    fn turn_back(&mut self) -> bool {
        let (first, last) = self.frame_ends();
        self.forward = false;
        let resume = if last > first {
            last - 1
        } else if self.element_count(last) > 1 {
            self.step = last;
            self.cursors[last] = self.element_count(last) - 2;
            return false;
        } else if last > 0 {
            // A one-element end has nothing left to reverse through.
            last - 1
        } else {
            return self.restart_forward();
        };
        self.step = resume;
        self.cursors[resume] = self.element_count(resume) - 1;
        false
    }

    /// Start the next forward pass after the reverse pass played step 0, or
    /// complete a `PingPongOnce`. The start is the first frame shown: steps
    /// before the first frame step played once at the turnaround and are not
    /// replayed. With one frame step the pass resumes one frame inside it;
    /// with several it resumes at the step after the first frame step.
    fn restart_forward(&mut self) -> bool {
        if self.loop_mode == SequenceLoop::PingPongOnce {
            return true;
        }
        let (first, last) = self.frame_ends();
        self.forward = true;
        self.cursors.fill(0);
        if last > first {
            self.step = first + 1;
        } else {
            self.step = first;
            self.cursors[first] = usize::from(self.element_count(first) > 1);
        }
        false
    }

    /// The first and last steps that play frames, or the first and last
    /// steps when none do.
    fn frame_ends(&self) -> (usize, usize) {
        let plays = |step: &SequenceStep| matches!(step, SequenceStep::PlayFrames { .. });
        match (
            self.steps.iter().position(plays),
            self.steps.iter().rposition(plays),
        ) {
            (Some(first), Some(last)) => (first, last),
            _ => (0, self.steps.len() - 1),
        }
    }

    /// Playback elements in `step`: its frames, or one for any other step.
    fn element_count(&self, step: usize) -> usize {
        match &self.steps[step] {
            SequenceStep::PlayFrames { frames, .. } => frames.len(),
            _ => 1,
        }
    }
}

/// Why a sequence could not be built. Every variant is an authoring error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceError {
    /// The builder was given no steps.
    NoSteps,
    /// The step at `step` plays an empty frame list.
    EmptyFrames { step: usize },
    /// The step at `step` was given a negative or `NaN` duration.
    BadDuration { step: usize },
    /// The random pause at `step` has `min > max`, or a negative or `NaN`
    /// bound.
    BadRandomRange { step: usize },
}

impl std::fmt::Display for SequenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSteps => write!(f, "animation sequence has no steps"),
            Self::EmptyFrames { step } => {
                write!(f, "animation sequence step {step} plays no frames")
            }
            Self::BadDuration { step } => write!(
                f,
                "animation sequence step {step} needs a finite, non-negative duration"
            ),
            Self::BadRandomRange { step } => write!(
                f,
                "animation sequence step {step} needs a finite random pause range with min <= max"
            ),
        }
    }
}

impl std::error::Error for SequenceError {}

/// Builder for an [`AnimationSequence`]. Steps play in the order they are
/// added; [`build`](SequenceBuilder::build) rejects authoring errors.
#[derive(Debug, Clone)]
pub struct SequenceBuilder {
    steps: Vec<SequenceStep>,
    loop_mode: SequenceLoop,
}

impl SequenceBuilder {
    fn new() -> Self {
        Self {
            steps: Vec::new(),
            loop_mode: SequenceLoop::Once,
        }
    }

    /// Set the loop mode (default [`SequenceLoop::Once`]).
    #[must_use]
    pub fn loop_mode(mut self, loop_mode: SequenceLoop) -> Self {
        self.loop_mode = loop_mode;
        self
    }

    /// Play an explicit frame order, holding each frame for `frame_duration`
    /// seconds.
    #[must_use]
    pub fn play_frames(mut self, frames: impl Into<Vec<usize>>, frame_duration: f32) -> Self {
        self.steps.push(SequenceStep::PlayFrames {
            frames: frames.into(),
            frame_duration,
        });
        self
    }

    /// Play the inclusive frame range `first..=last`.
    #[must_use]
    pub fn play_frame_range(self, first: usize, last: usize, frame_duration: f32) -> Self {
        let frames: Vec<usize> = if first <= last {
            (first..=last).collect()
        } else {
            Vec::new()
        };
        self.play_frames(frames, frame_duration)
    }

    /// Hold for `duration` seconds.
    #[must_use]
    pub fn pause(mut self, duration: f32) -> Self {
        self.steps.push(SequenceStep::Pause(duration));
        self
    }

    /// Hold for a duration drawn from `[min, max)` when the step is entered.
    #[must_use]
    pub fn random_pause(mut self, min: f32, max: f32) -> Self {
        self.steps.push(SequenceStep::RandomPause { min, max });
        self
    }

    /// Queue `id` for the caller when playback reaches this point.
    #[must_use]
    pub fn action(mut self, id: u32) -> Self {
        self.steps.push(SequenceStep::Action(id));
        self
    }

    /// Validate and build. An empty step list, an empty `PlayFrames` step, and
    /// a negative or `NaN` duration are authoring errors, not runtime states.
    pub fn build(self) -> Result<AnimationSequence, SequenceError> {
        if self.steps.is_empty() {
            return Err(SequenceError::NoSteps);
        }
        for (step, item) in self.steps.iter().enumerate() {
            match item {
                SequenceStep::PlayFrames {
                    frames,
                    frame_duration,
                } => {
                    if frames.is_empty() {
                        return Err(SequenceError::EmptyFrames { step });
                    }
                    if !frame_duration.is_finite() || *frame_duration < 0.0 {
                        return Err(SequenceError::BadDuration { step });
                    }
                }
                SequenceStep::Pause(duration) => {
                    if !duration.is_finite() || *duration < 0.0 {
                        return Err(SequenceError::BadDuration { step });
                    }
                }
                SequenceStep::RandomPause { min, max } => {
                    if !min.is_finite() || !max.is_finite() || *min < 0.0 || max < min {
                        return Err(SequenceError::BadRandomRange { step });
                    }
                }
                SequenceStep::Action(_) => {}
            }
        }
        let positions: usize = self
            .steps
            .iter()
            .map(|step| match step {
                SequenceStep::PlayFrames { frames, .. } => frames.len(),
                _ => 1,
            })
            .sum();
        let directions = match self.loop_mode {
            SequenceLoop::PingPongRepeat | SequenceLoop::PingPongOnce => 2,
            SequenceLoop::Repeat | SequenceLoop::Once => 1,
        };
        Ok(AnimationSequence {
            cycle_positions: positions * directions,
            cursors: vec![0; self.steps.len()],
            held_frame: None,
            steps: self.steps,
            loop_mode: self.loop_mode,
            step: 0,
            timer: 0.0,
            step_duration: None,
            forward: true,
            actions: std::collections::VecDeque::new(),
            finished: false,
            just_completed: false,
        })
    }
}

impl Default for SequenceBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #249: a tick over nothing but zero-duration steps makes exactly one
    /// full pass — every action fires once per step per tick, and the
    /// sequence keeps repeating on the next tick.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_tick_over_zero_duration_steps_makes_one_full_pass() {
        let mut sequence = AnimationSequence::builder()
            .loop_mode(SequenceLoop::Repeat)
            .action(1)
            .action(2)
            .build()
            .unwrap();
        let mut rng = rng();
        for _ in 0..2 {
            sequence.tick(0.5, &mut rng);
            let mut fired = Vec::new();
            while let Some(action) = sequence.take_action() {
                fired.push(action);
            }
            assert_eq!(fired.len(), 2, "one action per step per tick: {fired:?}");
        }
    }

    /// #249: a two-step ping-pong that repeats restarts its forward pass at
    /// the second step, entered at its first frame, so the first step (which
    /// just played in reverse) does not play twice in a row.
    #[wasm_bindgen_test(unsupported = test)]
    fn two_step_pingpong_repeat_restarts_one_step_in() {
        let mut sequence = AnimationSequence::builder()
            .loop_mode(SequenceLoop::PingPongRepeat)
            .play_frames(vec![0, 1], 0.1)
            .play_frames(vec![2, 3], 0.1)
            .build()
            .unwrap();
        let mut rng = rng();
        let mut seen = vec![sequence.current_frame()];
        for _ in 0..9 {
            sequence.tick(0.1, &mut rng);
            seen.push(sequence.current_frame());
        }
        assert_eq!(seen, [0, 1, 2, 3, 1, 0, 2, 3, 1, 0].map(Some).to_vec());
    }

    /// #249: a tick past several frame boundaries keeps the remainder, so
    /// the next small tick crosses the following boundary on time.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_remainder_carries_into_the_next_tick() {
        let mut sequence = AnimationSequence::builder()
            .loop_mode(SequenceLoop::Repeat)
            .play_frames(vec![0, 1, 2, 3, 4], 0.1)
            .build()
            .unwrap();
        let mut rng = rng();
        sequence.tick(0.25, &mut rng);
        assert_eq!(sequence.current_frame(), Some(2));
        sequence.tick(0.05, &mut rng);
        assert_eq!(
            sequence.current_frame(),
            Some(3),
            "0.05 s remainder carried"
        );
    }

    /// Frames shown after each `dt` tick until completion (or `ticks`), and
    /// every action fired along the way.
    fn play_with_actions(
        seq: &mut AnimationSequence,
        dt: f32,
        ticks: usize,
    ) -> (Vec<Option<usize>>, Vec<u32>) {
        let mut rng = rng();
        let mut seen = vec![seq.current_frame()];
        let mut fired = Vec::new();
        for _ in 0..ticks {
            let completed = seq.tick(dt, &mut rng);
            while let Some(action) = seq.take_action() {
                fired.push(action);
            }
            if completed {
                break;
            }
            seen.push(seq.current_frame());
        }
        (seen, fired)
    }

    /// #314: a trailing action does not move the turnaround off the frame
    /// step: each end frame shows once per direction change, the action
    /// fires once per turnaround, and a repeat restarts inside the frame
    /// step rather than skipping it.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_trailing_action_does_not_repeat_the_end_frame() {
        let build = |loop_mode| {
            AnimationSequence::builder()
                .loop_mode(loop_mode)
                .play_frames(vec![0, 1, 2], 0.1)
                .action(5)
                .build()
                .unwrap()
        };
        let (seen, fired) = play_with_actions(&mut build(SequenceLoop::PingPongOnce), 0.1, 10);
        assert_eq!(seen, [0, 1, 2, 1, 0].map(Some).to_vec());
        assert_eq!(fired, [5]);

        let (seen, fired) = play_with_actions(&mut build(SequenceLoop::PingPongRepeat), 0.1, 8);
        assert_eq!(seen, [0, 1, 2, 1, 0, 1, 2, 1, 0].map(Some).to_vec());
        assert_eq!(fired, [5, 5]);
    }

    /// #314: a leading action is the start end: the reverse pass reaches it
    /// once per bounce, and a repeat restarts one frame inside the frame step
    /// rather than skipping the reverse pass or replaying frame 0.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_leading_action_does_not_move_the_start() {
        let build = |loop_mode| {
            AnimationSequence::builder()
                .loop_mode(loop_mode)
                .action(9)
                .play_frames(vec![0, 1, 2], 0.1)
                .build()
                .unwrap()
        };
        let (seen, fired) = play_with_actions(&mut build(SequenceLoop::PingPongRepeat), 0.1, 8);
        assert_eq!(
            seen,
            [
                None,
                Some(1),
                Some(2),
                Some(1),
                Some(0),
                Some(1),
                Some(2),
                Some(1),
                Some(0)
            ]
        );
        assert_eq!(fired, [9, 9]);

        let mut once = build(SequenceLoop::PingPongOnce);
        let (seen, fired) = play_with_actions(&mut once, 0.1, 10);
        assert_eq!(
            seen,
            [None, Some(1), Some(2), Some(1), Some(0)],
            "completes on tick 5"
        );
        assert_eq!(fired, [9, 9]);
        assert!(once.is_finished());
    }

    /// #314: with several frame steps and a trailing action, the reverse
    /// pass skips the last frame step as it does without the action.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_trailing_action_keeps_multi_step_pingpong_endpoints() {
        let mut seq = AnimationSequence::builder()
            .loop_mode(SequenceLoop::PingPongOnce)
            .play_frames(vec![0, 1], 0.1)
            .play_frames(vec![2, 3], 0.1)
            .action(5)
            .build()
            .unwrap();
        let (seen, fired) = play_with_actions(&mut seq, 0.1, 10);
        assert_eq!(seen, [0, 1, 2, 3, 1, 0].map(Some).to_vec());
        assert_eq!(fired, [5]);
    }

    /// #249: two ping-pong steps of two frames play 0 1 2 3, turn around one
    /// step in entering it at its last frame, and finish: 1 0.
    #[wasm_bindgen_test(unsupported = test)]
    fn two_step_pingpong_enters_reversed_steps_at_their_last_frame() {
        let mut sequence = AnimationSequence::builder()
            .loop_mode(SequenceLoop::PingPongOnce)
            .play_frames(vec![0, 1], 0.1)
            .play_frames(vec![2, 3], 0.1)
            .build()
            .unwrap();
        let mut rng = rng();
        let mut seen = vec![sequence.current_frame()];
        for _ in 0..6 {
            sequence.tick(0.1, &mut rng);
            seen.push(sequence.current_frame());
        }
        // The finished sequence keeps showing its last frame.
        assert_eq!(
            seen,
            vec![
                Some(0),
                Some(1),
                Some(2),
                Some(3),
                Some(1),
                Some(0),
                Some(0)
            ]
        );
        assert!(sequence.is_finished());
    }

    /// #249: a single one-frame step ping-pongs without underflowing the
    /// reversed cursor, and finishes.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_single_one_frame_step_pingpongs_without_panicking() {
        let mut sequence = AnimationSequence::builder()
            .loop_mode(SequenceLoop::PingPongOnce)
            .play_frames(vec![7], 0.1)
            .build()
            .unwrap();
        let mut rng = rng();
        for _ in 0..4 {
            sequence.tick(0.1, &mut rng);
            assert!(matches!(sequence.current_frame(), Some(7) | None));
        }
        assert!(sequence.is_finished());
    }

    /// #299: a one-frame `PingPongOnce` shows its frame for one frame
    /// duration, completes then, and reports completion once.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_one_frame_pingpong_once_completes_after_one_frame_duration() {
        let mut sequence = AnimationSequence::builder()
            .loop_mode(SequenceLoop::PingPongOnce)
            .play_frames(vec![0], 0.25)
            .build()
            .unwrap();
        let mut rng = rng();
        assert!(sequence.tick(0.25, &mut rng), "completes after 0.25 s");
        assert_eq!(sequence.current_frame(), Some(0));
        assert!(!sequence.tick(0.25, &mut rng), "completion fires once");
    }

    /// #299: a lone action under `PingPongOnce` fires once and completes.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_lone_action_pingpong_once_fires_once() {
        let mut sequence = AnimationSequence::builder()
            .loop_mode(SequenceLoop::PingPongOnce)
            .action(5)
            .build()
            .unwrap();
        assert!(sequence.tick(0.0, &mut rng()));
        assert_eq!(sequence.take_action(), Some(5));
        assert_eq!(sequence.take_action(), None);
    }

    /// #249: the builder's boundaries — a zero frame duration and a random
    /// pause with equal bounds are valid; an inverted random range is not.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_builder_accepts_zero_durations_and_equal_random_bounds() {
        assert!(
            AnimationSequence::builder()
                .play_frames(vec![0], 0.0)
                .random_pause(0.5, 0.5)
                .build()
                .is_ok()
        );
        assert_eq!(
            AnimationSequence::builder()
                .random_pause(0.6, 0.5)
                .build()
                .err(),
            Some(SequenceError::BadRandomRange { step: 0 })
        );
    }

    fn rng() -> Rng {
        Rng::from_seed(1)
    }

    /// The frames displayed by `ticks` ticks of `dt`, starting with the frame
    /// shown before the first tick. Stops at the completion edge.
    fn play(seq: &mut AnimationSequence, dt: f32, ticks: usize) -> Vec<Option<usize>> {
        let mut rng = rng();
        let mut seen = vec![seq.current_frame()];
        for _ in 0..ticks {
            if seq.tick(dt, &mut rng) {
                break;
            }
            seen.push(seq.current_frame());
        }
        seen
    }

    fn single(frames: [usize; 3], loop_mode: SequenceLoop) -> AnimationSequence {
        AnimationSequence::builder()
            .loop_mode(loop_mode)
            .play_frames(frames, 0.1)
            .build()
            .expect("valid sequence")
    }

    /// Single-step ping-pong turns around one frame in, so neither end plays
    /// twice: `0,1,2,1,0` then complete.
    #[wasm_bindgen_test(unsupported = test)]
    fn single_step_pingpong_once_plays_up_and_back() {
        let mut seq = single([0, 1, 2], SequenceLoop::PingPongOnce);
        assert_eq!(
            play(&mut seq, 0.1, 10),
            vec![Some(0), Some(1), Some(2), Some(1), Some(0)]
        );
        assert!(seq.is_finished() && seq.just_completed());
        let mut rng = rng();
        assert!(!seq.tick(0.1, &mut rng), "the completion edge fires once");
        assert!(!seq.just_completed());
    }

    /// Ping-pong repeat keeps bouncing: `0,1,2,1,0,1,2,1,0,…`.
    #[wasm_bindgen_test(unsupported = test)]
    fn single_step_pingpong_repeat_bounces_forever() {
        let mut seq = single([0, 1, 2], SequenceLoop::PingPongRepeat);
        assert_eq!(
            play(&mut seq, 0.1, 8),
            vec![
                Some(0),
                Some(1),
                Some(2),
                Some(1),
                Some(0),
                Some(1),
                Some(2),
                Some(1),
                Some(0),
            ]
        );
        assert!(!seq.is_finished());
    }

    /// Multi-step ping-pong reverses the *step* order, turning around one step
    /// in, and enters each reversed step at its last frame.
    #[wasm_bindgen_test(unsupported = test)]
    fn multi_step_pingpong_reverses_step_order() {
        let mut seq = AnimationSequence::builder()
            .loop_mode(SequenceLoop::PingPongOnce)
            .play_frames([0, 1], 0.1)
            .play_frames([2, 3], 0.1)
            .play_frames([4, 5], 0.1)
            .build()
            .expect("valid sequence");
        // Forward through all three steps, then back down through steps 1 and
        // 0 — step 2 is the turnaround and does not replay — each entered at
        // its last frame.
        assert_eq!(
            play(&mut seq, 0.1, 20),
            [0, 1, 2, 3, 4, 5, 3, 2, 1, 0].map(Some).to_vec()
        );
        assert!(seq.just_completed());
    }

    /// `Repeat` wraps to the first step and rewinds every step's frame cursor,
    /// so the second pass replays the first pass exactly.
    #[wasm_bindgen_test(unsupported = test)]
    fn repeat_rewinds_every_step_on_wrap() {
        let mut seq = AnimationSequence::builder()
            .loop_mode(SequenceLoop::Repeat)
            .play_frames([0, 1], 0.1)
            .play_frames([7, 8], 0.1)
            .build()
            .expect("valid sequence");
        assert_eq!(
            play(&mut seq, 0.1, 8),
            [0, 1, 7, 8, 0, 1, 7, 8, 0].map(Some).to_vec()
        );
        assert!(!seq.is_finished());
    }

    /// A large `dt` steps several frames and carries the remainder instead of
    /// discarding it, so a slow frame does not slow the animation down.
    #[wasm_bindgen_test(unsupported = test)]
    fn large_dt_steps_several_frames_and_carries_the_remainder() {
        let mut seq = AnimationSequence::builder()
            .loop_mode(SequenceLoop::Repeat)
            .play_frames([0, 1, 2, 3], 0.1)
            .build()
            .expect("valid sequence");
        let mut rng = rng();
        // 0.35 s = three whole frames plus 0.05 s.
        assert!(!seq.tick(0.35, &mut rng));
        assert_eq!(seq.current_frame(), Some(3));
        // The carried 0.05 s completes the fourth frame.
        assert!(!seq.tick(0.05, &mut rng));
        assert_eq!(seq.current_frame(), Some(0));
    }

    /// One tick's leftover time carries across step boundaries too.
    #[wasm_bindgen_test(unsupported = test)]
    fn leftover_time_carries_across_steps() {
        let mut seq = AnimationSequence::builder()
            .play_frames([0], 0.1)
            .pause(0.1)
            .play_frames([5, 6], 0.1)
            .build()
            .expect("valid sequence");
        let mut rng = rng();
        // 0.25 s = frame step (0.1) + pause (0.1) + 0.05 s into the last step.
        assert!(!seq.tick(0.25, &mut rng));
        assert_eq!(seq.current_frame(), Some(5));
        assert!(!seq.tick(0.05, &mut rng));
        assert_eq!(seq.current_frame(), Some(6));
    }

    /// The authored timeline shows frame 1 from 0.125 s through the pause,
    /// even when no tick stops on that frame before entering the pause.
    #[wasm_bindgen_test(unsupported = test)]
    fn large_ticks_hold_the_last_frame_through_pauses() {
        let frames = AnimationSequence::builder().play_frames([0, 1], 0.125);
        for builder in [frames.clone().pause(1.0), frames.random_pause(1.0, 2.0)] {
            for schedule in [&[0.25][..], &[0.125, 0.125][..]] {
                let mut seq = builder.clone().build().unwrap();
                let mut rng = rng();
                for dt in schedule {
                    assert!(!seq.tick(*dt, &mut rng));
                }
                assert_eq!(seq.current_frame(), Some(1), "entered the pause");
                assert!(!seq.tick(0.5, &mut rng));
                assert_eq!(seq.current_frame(), Some(1), "still in the pause");
            }
        }
    }

    /// Completion must retain the authored endpoint even if the entire run
    /// was crossed in one tick; ping-pong ends back on its first frame.
    #[wasm_bindgen_test(unsupported = test)]
    fn completing_tick_holds_the_last_traversed_frame() {
        for (loop_mode, endpoint) in [(SequenceLoop::Once, 5), (SequenceLoop::PingPongOnce, 3)] {
            let mut seq = AnimationSequence::builder()
                .loop_mode(loop_mode)
                .play_frames([3, 4, 5], 0.125)
                .build()
                .unwrap();
            let mut rng = rng();
            assert!(seq.tick(1.0, &mut rng));
            assert_eq!(seq.current_frame(), Some(endpoint));
            assert!(!seq.tick(1.0, &mut rng));
            assert_eq!(seq.current_frame(), Some(endpoint));
        }
    }

    /// On the reverse pass a pause holds the frame just played, not the
    /// last frame of the step that precedes the pause in authored order.
    #[wasm_bindgen_test(unsupported = test)]
    fn reversed_pause_holds_the_frame_at_the_turnaround() {
        let mut seq = AnimationSequence::builder()
            .loop_mode(SequenceLoop::PingPongOnce)
            .play_frames([0, 1], 0.125)
            .pause(0.5)
            .play_frames([2, 3], 0.125)
            .build()
            .unwrap();
        let mut rng = rng();
        assert!(!seq.tick(1.0, &mut rng));
        assert_eq!(seq.current_frame(), Some(3), "reverse pause at 1.0 s");
        assert!(seq.tick(0.75, &mut rng));
        assert_eq!(seq.current_frame(), Some(0), "reverse run ends at 1.75 s");
    }

    /// A leading pause has no frame to hold; resetting into it must discard
    /// the completed run's endpoint until playback reaches a frame again.
    #[wasm_bindgen_test(unsupported = test)]
    fn reset_clears_the_held_frame_before_a_leading_pause() {
        let mut seq = AnimationSequence::builder()
            .pause(0.25)
            .play_frames([4, 7], 0.125)
            .build()
            .unwrap();
        let mut rng = rng();
        assert!(seq.tick(0.5, &mut rng));
        assert_eq!(seq.current_frame(), Some(7));
        seq.reset();
        assert!(!seq.tick(0.125, &mut rng));
        assert_eq!(seq.current_frame(), None, "no frame reached after reset");
        assert!(!seq.tick(0.125, &mut rng));
        assert_eq!(seq.current_frame(), Some(4), "first frame reached again");
    }

    /// A `Repeat` sequence made only of zero-duration steps must not spin
    /// inside one tick: it stops after one pass and drops the remainder.
    #[wasm_bindgen_test(unsupported = test)]
    fn zero_duration_steps_cannot_spin_within_a_tick() {
        let mut seq = AnimationSequence::builder()
            .loop_mode(SequenceLoop::Repeat)
            .action(3)
            .pause(0.0)
            .random_pause(0.0, 0.0)
            .build()
            .expect("valid sequence");
        let mut rng = rng();
        assert!(!seq.tick(60.0, &mut rng), "Repeat never completes");
        assert_eq!(seq.take_action(), Some(3));
        assert_eq!(seq.take_action(), None, "one pass fires one action");
        assert!(!seq.tick(60.0, &mut rng));
    }

    /// Actions queue in order and drain once; a tick that crosses two action
    /// steps yields both.
    #[wasm_bindgen_test(unsupported = test)]
    fn actions_queue_in_order_and_drain_once() {
        let mut seq = AnimationSequence::builder()
            .play_frames([0], 0.1)
            .action(7)
            .action(9)
            .play_frames([1], 0.1)
            .build()
            .expect("valid sequence");
        let mut rng = rng();
        assert_eq!(
            seq.take_action(),
            None,
            "nothing fires before the first tick"
        );
        assert!(!seq.tick(0.1, &mut rng));
        assert_eq!(seq.current_frame(), Some(1), "both actions were crossed");
        assert_eq!(seq.take_action(), Some(7));
        assert_eq!(seq.take_action(), Some(9));
        assert_eq!(seq.take_action(), None);
        assert!(seq.tick(0.1, &mut rng), "the last frame completes the run");
        assert_eq!(seq.take_action(), None, "actions do not re-fire");
    }

    /// `min == max` pauses for exactly that long: a 0.5 s pause ends on the
    /// fourth 0.125 s tick, not before.
    #[wasm_bindgen_test(unsupported = test)]
    fn an_exact_random_pause_lasts_exactly_min() {
        let mut seq = AnimationSequence::builder()
            .play_frames([4], 0.125)
            .random_pause(0.5, 0.5)
            .build()
            .expect("valid sequence");
        let mut rng = rng();
        assert!(!seq.tick(0.125, &mut rng), "off the frame, onto the pause");
        assert_eq!(
            seq.current_frame(),
            Some(4),
            "a pause holds its prior frame"
        );
        for tick in 1..4 {
            assert!(
                !seq.tick(0.125, &mut rng),
                "still pausing after {tick} ticks"
            );
        }
        assert!(seq.tick(0.125, &mut rng), "0.5 s of pause has elapsed");
    }

    /// A random pause stays inside its authored bounds and is reproducible
    /// from the seed: the same seed replays the same run, and a global source
    /// of randomness could not.
    #[wasm_bindgen_test(unsupported = test)]
    fn random_pauses_are_bounded_and_seed_reproducible() {
        let build = || {
            AnimationSequence::builder()
                .random_pause(0.4, 0.8)
                .build()
                .expect("valid sequence")
        };
        // Ticks of 0.05 s to cross a hold in [0.4, 0.8) → 8..=16 ticks.
        let ticks_to_finish = |seed: u64| {
            let mut seq = build();
            let mut rng = Rng::from_seed(seed);
            (1..=100)
                .find(|_| seq.tick(0.05, &mut rng))
                .expect("the pause ends")
        };
        for seed in 0..32 {
            let ticks = ticks_to_finish(seed);
            assert!(
                (8..=16).contains(&ticks),
                "seed {seed} paused {ticks} ticks"
            );
            assert_eq!(ticks, ticks_to_finish(seed), "seed {seed} is reproducible");
        }
    }

    /// Only random pauses consume the caller's RNG, so adding an animation to
    /// a frame cannot shift a seeded simulation's random stream.
    #[wasm_bindgen_test(unsupported = test)]
    fn steps_without_randomness_do_not_draw_from_the_rng() {
        let mut seq = AnimationSequence::builder()
            .loop_mode(SequenceLoop::Repeat)
            .play_frames([0, 1], 0.1)
            .pause(0.2)
            .action(1)
            .build()
            .expect("valid sequence");
        let mut used = Rng::from_seed(5);
        for _ in 0..50 {
            seq.tick(0.1, &mut used);
        }
        let mut untouched = Rng::from_seed(5);
        assert_eq!(used.u32(0..u32::MAX), untouched.u32(0..u32::MAX));
    }

    /// `reset` rewinds a finished sequence to its first frame.
    #[wasm_bindgen_test(unsupported = test)]
    fn reset_restarts_a_finished_sequence() {
        let mut seq = single([3, 4, 5], SequenceLoop::Once);
        let mut rng = rng();
        while !seq.is_finished() {
            seq.tick(0.1, &mut rng);
        }
        assert_eq!(seq.current_frame(), Some(5), "Once holds its last frame");
        seq.reset();
        assert!(!seq.is_finished() && !seq.just_completed());
        assert_eq!(seq.current_frame(), Some(3));
        assert!(!seq.tick(0.1, &mut rng));
        assert_eq!(seq.current_frame(), Some(4));
    }

    /// Authoring errors are rejected at build time rather than panicking or
    /// stalling at runtime.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_builder_rejects_authoring_errors() {
        let err = |builder: SequenceBuilder| builder.build().expect_err("authoring error");
        assert_eq!(err(AnimationSequence::builder()), SequenceError::NoSteps);
        assert_eq!(
            err(AnimationSequence::builder().play_frames(Vec::new(), 0.1)),
            SequenceError::EmptyFrames { step: 0 }
        );
        assert_eq!(
            err(AnimationSequence::builder().play_frame_range(4, 2, 0.1)),
            SequenceError::EmptyFrames { step: 0 },
            "a backwards range plays nothing"
        );
        assert_eq!(
            err(AnimationSequence::builder().play_frames([0], -0.1)),
            SequenceError::BadDuration { step: 0 }
        );
        assert_eq!(
            err(AnimationSequence::builder()
                .play_frames([0], 0.1)
                .pause(f32::NAN)),
            SequenceError::BadDuration { step: 1 }
        );
        assert_eq!(
            err(AnimationSequence::builder().pause(f32::INFINITY)),
            SequenceError::BadDuration { step: 0 }
        );
        assert_eq!(
            err(AnimationSequence::builder().random_pause(2.0, 1.0)),
            SequenceError::BadRandomRange { step: 0 }
        );
        assert_eq!(
            err(AnimationSequence::builder().random_pause(f32::NAN, 1.0)),
            SequenceError::BadRandomRange { step: 0 }
        );
        assert!(
            AnimationSequence::builder()
                .random_pause(0.0, 0.0)
                .build()
                .is_ok(),
            "a zero-length random pause is legitimate"
        );
    }

    /// #313: a non-finite `dt` neither hangs a repeating sequence nor
    /// poisons its clock — the next finite tick advances on time.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_non_finite_dt_is_ignored() {
        let mut seq = AnimationSequence::builder()
            .loop_mode(SequenceLoop::PingPongRepeat)
            .play_frames(vec![0, 1, 2], 0.1)
            .build()
            .expect("valid sequence");
        let mut rng = rng();
        for dt in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(!seq.tick(dt, &mut rng));
            assert_eq!(seq.current_frame(), Some(0));
        }
        seq.tick(0.1, &mut rng);
        assert_eq!(seq.current_frame(), Some(1));
    }

    /// #313: a play-once sequence whose frame durations are too small to
    /// lower the accumulated time still plays to completion in one tick that
    /// covers it.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_once_sequence_with_vanishing_durations_completes_in_one_tick() {
        let mut seq = AnimationSequence::builder()
            .play_frames(vec![0, 1, 2, 3], 1e-30)
            .build()
            .expect("valid sequence");
        assert!(seq.tick(0.016, &mut rng()), "completion edge");
        assert_eq!(seq.current_frame(), Some(3));
    }

    /// #297: zero-duration frames before an action count as playback
    /// positions, not steps: the action fires on the first tick, the frame
    /// run's last frame holds through the pause, and the sequence finishes
    /// once a second has elapsed. A repeating copy fires the action on the
    /// first tick too.
    #[wasm_bindgen_test(unsupported = test)]
    fn zero_duration_frames_do_not_delay_a_following_action() {
        for loop_mode in [SequenceLoop::Once, SequenceLoop::Repeat] {
            let mut seq = AnimationSequence::builder()
                .loop_mode(loop_mode)
                .play_frames([0, 1, 2, 3], 0.0)
                .action(7)
                .pause(1.0)
                .build()
                .expect("valid sequence");
            let mut rng = rng();
            assert!(!seq.tick(0.5, &mut rng));
            assert_eq!(seq.take_action(), Some(7), "{loop_mode:?}");
            assert_eq!(seq.current_frame(), Some(3));
            let completed = seq.tick(0.5, &mut rng);
            assert_eq!(completed, loop_mode == SequenceLoop::Once, "{loop_mode:?}");
        }
    }

    /// #297: a zero-time ping-pong cycle reaches the action after its
    /// zero-duration frames within the tick.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_zero_time_pingpong_cycle_reaches_its_action() {
        let mut seq = AnimationSequence::builder()
            .loop_mode(SequenceLoop::PingPongRepeat)
            .play_frames([0, 1, 2], 0.0)
            .action(4)
            .build()
            .expect("valid sequence");
        assert!(!seq.tick(1.0, &mut rng()));
        assert_eq!(seq.take_action(), Some(4));
    }

    /// #313: a frame duration too small to lower the accumulated time in
    /// `f32` returns instead of spinning, in both repeating modes.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_vanishing_frame_duration_does_not_hang_a_repeating_sequence() {
        for loop_mode in [SequenceLoop::Repeat, SequenceLoop::PingPongRepeat] {
            let mut seq = AnimationSequence::builder()
                .loop_mode(loop_mode)
                .play_frames(vec![0], 1e-30)
                .build()
                .expect("valid sequence");
            assert!(!seq.tick(0.016, &mut rng()));
            assert_eq!(seq.current_frame(), Some(0));
        }
    }

    /// `play_frame_range` is the inclusive range the Aseprite tags use.
    #[wasm_bindgen_test(unsupported = test)]
    fn play_frame_range_is_inclusive() {
        let mut seq = AnimationSequence::builder()
            .loop_mode(SequenceLoop::Once)
            .play_frame_range(8, 11, 0.1)
            .build()
            .expect("valid sequence");
        assert_eq!(play(&mut seq, 0.1, 10), [8, 9, 10, 11].map(Some).to_vec());
    }
}
