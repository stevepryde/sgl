//! The camera's history: the last submitted frame's unjittered view and
//! projection, with the jitter antialiasing applied to that frame, its time
//! (`FrameInput::elapsed_seconds`), at which a material's vertex function
//! evaluates the motion it writes, its visibility mask, its backdrop
//! panorama's yaw, which the sky's motion turns with, and the frames since
//! the last reset. Each `View` owns its own; a frame commits to it only
//! when the caller finishes that frame. Also what a stage keeps to describe
//! its GPU history as the view's last finished frame left it (`Kept`). It is kept in the render frame
//! of the scene's origin it was committed at, and a frame whose scene moved
//! its origin since sees it translated into its own (the architecture's
//! History contract).
use glam::{DMat4, DVec3, Mat4, Vec3};

/// One frame's camera: its unjittered view and projection, and the jitter
/// (NDC) antialiasing applied to its projection, zero without one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CameraFrame {
    pub view: Mat4,
    pub projection: Mat4,
    pub jitter: [f32; 2],
}

impl CameraFrame {
    /// Its projection as it rasterized, the jitter applied: the projection
    /// prepare draws the frame with.
    pub fn jittered_projection(&self) -> Mat4 {
        Mat4::from_translation(Vec3::new(self.jitter[0], self.jitter[1], 0.)) * self.projection
    }

    /// Its view-projection as it rasterized.
    pub fn jittered_view_projection(&self) -> Mat4 {
        self.jittered_projection() * self.view
    }

    /// The same camera in a render frame whose origin lies `by` from this
    /// one's: positions there are this frame's less `by`. The view is
    /// composed with the translation in double precision and rounded once.
    fn translated(self, by: DVec3) -> Self {
        if by == DVec3::ZERO {
            return self;
        }
        Self {
            view: (self.view.as_dmat4() * DMat4::from_translation(by)).as_mat4(),
            ..self
        }
    }
}

/// One frame's view of the camera history.
#[derive(Clone, Copy)]
pub(crate) struct HistoryFrame {
    /// This frame's unjittered view-projection.
    pub stable: Mat4,
    /// The previous frame's unjittered view-projection (this frame's after a reset).
    pub previous: Mat4,
    /// The last submitted frame's camera in this frame's render frame; none
    /// after a reset.
    pub previous_camera: Option<CameraFrame>,
    /// False after a reset: history from earlier frames must not be reused.
    pub valid: bool,
    /// Frames since the last reset.
    pub frames: u32,
    /// This frame's camera, which `finish` commits; its jitter is set once
    /// antialiasing chose it.
    pub camera: CameraFrame,
    /// The scene's render origin this frame (`Scene::origin`).
    pub origin: DVec3,
    /// This frame's time, which `finish` commits, and the last submitted
    /// frame's; this frame's after a reset.
    pub elapsed_seconds: f64,
    pub previous_elapsed_seconds: f64,
    /// This frame's visibility mask (`FrameInput::visibility_mask`), which
    /// `finish` commits.
    pub visibility_mask: u32,
    /// Whether it differs from the last submitted frame's: material groups
    /// appeared or departed without moving, which rearms TAA's rejection
    /// across the view. False after a reset, which restarts every history.
    pub visibility_changed: bool,
    /// This frame's backdrop panorama yaw (`Backdrop::Environment`), none
    /// for a colour, which `finish` commits; `CameraHistory::set_backdrop`
    /// sets it.
    pub backdrop_yaw: Option<f32>,
    /// The yaw the panorama had in the last submitted frame: what the sky
    /// turned from. This frame's own (0 for a colour) after a reset, or
    /// where either frame's backdrop is a colour, which has no direction to
    /// turn.
    pub previous_backdrop_yaw: f32,
}

/// What a stage keeps to describe its GPU history: as the view's last
/// finished frame left it (none where the stage did not run in that frame),
/// and what the frame being rendered leaves. A frame rendered and not
/// finished leaves nothing kept, so the next frame's state derives from the
/// last finished frame's (the architecture's History contract, "What a
/// frame commits"). The view settles every stage's once per render, when
/// it knows whether that render was finished (`View::settle`). It does not
/// keep the GPU history itself: a stage that releases its targets in a
/// render without it restarts after that render, finished or not (F-4).
pub(crate) struct Kept<T> {
    committed: Option<T>,
    rendered: Option<T>,
}

impl<T> Default for Kept<T> {
    fn default() -> Self {
        Self {
            committed: None,
            rendered: None,
        }
    }
}

impl<T> Kept<T> {
    /// As the last finished frame left it; none where the stage did not
    /// run in that frame.
    pub fn last(&self) -> Option<&T> {
        self.committed.as_ref()
    }

    /// What the frame being rendered leaves, until it is settled.
    pub fn left(&self) -> Option<&T> {
        self.rendered.as_ref()
    }

    /// The frame being rendered leaves `state`.
    pub fn leave(&mut self, state: T) {
        self.rendered = Some(state);
    }

    /// Settles the last render: a finished one's state becomes the last
    /// finished frame's (none where it left none: the stage did not run in
    /// it); an unfinished one's is dropped.
    pub fn settle(&mut self, finished: bool) {
        let rendered = self.rendered.take();
        if finished {
            self.committed = rendered;
        }
    }

    /// Forgets both: the GPU history they describe is gone.
    pub fn forget(&mut self) {
        self.committed = None;
        self.rendered = None;
    }
}

/// A stage's run of temporal history in one frame: its frame count since
/// it last restarted (0 restarts it), and the half of each history pair
/// the frame writes, the other holding the last frame's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StageRun {
    pub count: u32,
    pub current: usize,
}

impl StageRun {
    /// The run of a frame after the last finished frame's run `last` (none
    /// where the stage did not run in it): the next count, never 0, and the
    /// other half where it `continues` from `last`, else a restart (count
    /// 0). The half alternates from `last` either way, so the count
    /// skipping 0 as it wraps never writes one half twice in a row.
    pub fn after(last: Option<&StageRun>, continues: bool) -> StageRun {
        let current = last.map_or(0, |last| 1 - last.current);
        match last {
            Some(last) if continues => StageRun {
                count: last.count.wrapping_add(1).max(1),
                current,
            },
            _ => StageRun { count: 0, current },
        }
    }
}

/// A stage's temporal history, kept as the last finished frame left it.
#[derive(Default)]
pub(crate) struct StageHistory {
    kept: Kept<StageRun>,
}

impl StageHistory {
    /// The frame's run: it continues the last finished frame's run unless
    /// `!frame.valid || restart` or the stage did not run in that frame:
    /// count + 1 (never 0) and the other half; else count 0. Leaves it as
    /// the frame's. A frame rendered again gets the same run, so it reads
    /// and writes the same halves.
    pub fn begin(&mut self, frame: &HistoryFrame, restart: bool) -> StageRun {
        let run = StageRun::after(self.kept.last(), frame.valid && !restart);
        self.kept.leave(run);
        run
    }

    /// See [`Kept::settle`].
    pub fn settle(&mut self, finished: bool) {
        self.kept.settle(finished);
    }

    /// See [`Kept::forget`]: the stage's history targets were recreated.
    pub fn forget(&mut self) {
        self.kept.forget();
    }
}

#[derive(Default)]
pub(crate) struct CameraHistory {
    previous: Option<CameraFrame>,
    frames: u32,
    /// The render origin `previous` is expressed in.
    origin: DVec3,
    /// The last submitted frame's time.
    elapsed_seconds: f64,
    /// The last submitted frame's visibility mask.
    visibility_mask: u32,
    /// The last submitted frame's backdrop panorama yaw.
    backdrop_yaw: Option<f32>,
}

impl CameraHistory {
    /// The history of a frame seen by `camera` at `elapsed_seconds` with the
    /// scene's origin at `origin` and `visibility_mask` enabling its material
    /// groups, restarted when `reset`. Nothing changes until `finish`, so an
    /// abandoned frame translates nothing twice and a mask change it saw is
    /// seen again by the next.
    pub fn begin(
        &self,
        camera: CameraFrame,
        reset: bool,
        origin: DVec3,
        elapsed_seconds: f64,
        visibility_mask: u32,
    ) -> HistoryFrame {
        let (previous, frames) = if reset {
            (None, 0)
        } else {
            (
                self.previous
                    .map(|previous| previous.translated(origin - self.origin)),
                self.frames,
            )
        };
        let stable = camera.projection * camera.view;
        HistoryFrame {
            stable,
            previous: previous.map_or(stable, |previous| previous.projection * previous.view),
            previous_camera: previous,
            valid: previous.is_some(),
            frames,
            camera,
            origin,
            elapsed_seconds,
            previous_elapsed_seconds: if previous.is_some() {
                self.elapsed_seconds
            } else {
                elapsed_seconds
            },
            visibility_mask,
            visibility_changed: previous.is_some() && visibility_mask != self.visibility_mask,
            backdrop_yaw: None,
            previous_backdrop_yaw: 0.,
        }
    }

    /// Gives `frame` its backdrop panorama's yaw, `None` for a colour, and
    /// the yaw the last submitted frame's panorama had where the frame
    /// follows it and both show a panorama.
    pub fn set_backdrop(&self, frame: &mut HistoryFrame, yaw: Option<f32>) {
        frame.backdrop_yaw = yaw;
        frame.previous_backdrop_yaw = match (frame.valid, self.backdrop_yaw, yaw) {
            (true, Some(previous), Some(_)) => previous,
            _ => yaw.unwrap_or(0.),
        };
    }

    /// Commits `frame`, which the caller submitted.
    pub fn finish(&mut self, frame: HistoryFrame) {
        self.previous = Some(frame.camera);
        self.frames = frame.frames.wrapping_add(1);
        self.origin = frame.origin;
        self.elapsed_seconds = frame.elapsed_seconds;
        self.visibility_mask = frame.visibility_mask;
        self.backdrop_yaw = frame.backdrop_yaw;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec4;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Plausible defects: the previous camera kept in the old render frame,
    // translated the wrong way, translated again by a frame that was not
    // finished, or left untranslated by a reset. The oracle is the world:
    // a point the game expresses in the new frame (less the move) projects
    // through the translated previous camera where the same point projected
    // through the previous camera before the move.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_moved_origin_reprojects_the_same_world_point() {
        let projection = crate::perspective(1.1, 1.5, 0.1);
        let eye = Vec3::new(5003.25, 12.5, -7020.75);
        let camera = |eye: Vec3| CameraFrame {
            view: glam::camera::rh::view::look_at_mat4(
                eye,
                eye + Vec3::new(1., -0.2, -1.),
                Vec3::Y,
            ),
            projection,
            jitter: [0.25, -0.5],
        };
        let mut history = CameraHistory::default();
        let first = history.begin(camera(eye), false, DVec3::ZERO, 0., 0);
        history.finish(first);
        let moved = Vec3::new(5000., 0., -7000.);
        let point = Vec3::new(5010.5, 11., -7031.);
        let before = first.camera.jittered_view_projection() * point.extend(1.);
        // An abandoned frame after the move, then the frame that finishes.
        for _ in 0..2 {
            let frame = history.begin(camera(eye - moved), false, moved.as_dvec3(), 0., 0);
            let previous = frame.previous_camera.expect("history continues");
            let after = previous.jittered_view_projection() * (point - moved).extend(1.);
            let ndc = |clip: Vec4| clip.truncate() / clip.w;
            assert!(
                (ndc(before) - ndc(after)).abs().max_element() < 1e-4,
                "{:?} became {:?}",
                ndc(before),
                ndc(after)
            );
            assert_eq!(previous.jitter, first.camera.jitter);
        }
        let reset = history.begin(camera(eye - moved), true, moved.as_dvec3(), 0., 0);
        assert!(reset.previous_camera.is_none() && !reset.valid);
    }

    // Plausible defect: the frame count stops at its cap, so every later
    // frame looks like a gap and TAA, SSR, fog and the traced stages restart
    // their history each frame. The oracle is the stages' continuity rule:
    // a frame continues the last when its count is the last's plus one,
    // wrapping.
    #[wasm_bindgen_test(unsupported = test)]
    fn consecutive_frames_continue_across_the_frame_count_s_wrap() {
        let camera = CameraFrame {
            view: Mat4::IDENTITY,
            projection: crate::perspective(1.1, 1.5, 0.1),
            jitter: [0.; 2],
        };
        let mut history = CameraHistory {
            frames: u32::MAX - 1,
            ..CameraHistory::default()
        };
        let mut last = None;
        for _ in 0..4 {
            let frame = history.begin(camera, false, DVec3::ZERO, 0., 0);
            if let Some(last) = last {
                assert_eq!(frame.frames, u32::wrapping_add(last, 1));
            }
            last = Some(frame.frames);
            history.finish(frame);
        }
    }

    // Plausible defects: a mask change never reported (the old image of a
    // hidden group fades out over many frames at TAA's relaxed rejection),
    // the comparison value committed when a frame is rendered rather than
    // submitted (an abandoned frame or a frame rendered again loses the
    // change), the change reported on every later frame (stabilization
    // never returns), or a change in the high bits ignored although
    // `Material::enabled` tests every bit. The oracle is the architecture's
    // rule (Frame item 11) that appearances and departures rearm the view
    // until a submitted frame; the expected values follow from the masks
    // each frame passes, compared by hand.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_changed_visibility_mask_is_reported_until_a_frame_is_finished() {
        let camera = CameraFrame {
            view: Mat4::IDENTITY,
            projection: crate::perspective(1.1, 1.5, 0.1),
            jitter: [0.; 2],
        };
        let mut history = CameraHistory::default();
        let frame = |history: &mut CameraHistory, reset: bool, mask: u32, finish: bool| {
            let frame = history.begin(camera, reset, DVec3::ZERO, 0., mask);
            if finish {
                history.finish(frame);
            }
            frame.visibility_changed
        };
        assert!(
            !frame(&mut history, true, 0b11, true),
            "the first frame restarts history: nothing to compare"
        );
        assert!(!frame(&mut history, false, 0b11, true), "an unchanged mask");
        assert!(frame(&mut history, false, 0b01, false), "group 2 departs");
        assert!(
            frame(&mut history, false, 0b01, false),
            "the abandoned frame did not consume the change"
        );
        assert!(
            frame(&mut history, false, 0b01, true),
            "nor did a frame rendered twice"
        );
        assert!(
            !frame(&mut history, false, 0b01, true),
            "the change was submitted"
        );
        assert!(
            frame(&mut history, false, 0b01 | 1 << 30, true),
            "a high group appears"
        );
        assert!(
            !frame(&mut history, true, 0b11, true),
            "a reset restarts history, which needs no signal"
        );
        assert!(
            !frame(&mut history, false, 0b11, true),
            "the reset frame committed its mask"
        );
        assert!(
            frame(&mut history, false, 0b01, false),
            "group 2 departs in a frame that is abandoned"
        );
        assert!(
            !frame(&mut history, false, 0b11, true),
            "a mask changed back before any frame was finished changed nothing"
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn the_history_halves_alternate_across_the_frame_count_s_wrap() {
        let mut run = StageRun {
            count: u32::MAX - 2,
            current: 1,
        };
        let mut written = vec![run.current];
        for _ in 0..6 {
            run = StageRun::after(Some(&run), true);
            assert_ne!(run.count, 0, "only a restart sets the count to 0");
            written.push(run.current);
        }
        for pair in written.windows(2) {
            assert_ne!(
                pair[0], pair[1],
                "consecutive frames wrote one half: {written:?}"
            );
        }
    }

    // Plausible defects: a stage's history restarts after a frame that was
    // rendered and not finished (B-4 of the 2026-11-18 history audit), a
    // frame rendered again writes the other half and so overwrites the
    // history it reads, an unfinished frame's restart or skip is kept, or
    // a stage that did not run in the last finished frame continues from
    // an older one. The oracle is the History contract's table ("What a
    // frame commits"; F-3, F-4): each row below gives a frame's outcome and
    // the run the contract requires of it, written by hand. It tests the
    // kept run alone (`StageHistory` over `CameraHistory`): the view's
    // reset decision, which holds an unfinished reset (F-1), and which
    // stages release their targets in a render without them (F-4) lie
    // above it.
    #[wasm_bindgen_test(unsupported = test)]
    fn kept_state_survives_an_unfinished_frame_and_ends_when_the_stage_skips_one() {
        let camera = CameraFrame {
            view: Mat4::IDENTITY,
            projection: crate::perspective(1.1, 1.5, 0.1),
            jitter: [0.; 2],
        };
        let mut history = CameraHistory::default();
        let mut stage = StageHistory::default();
        #[derive(Clone, Copy, PartialEq)]
        enum Outcome {
            Finished,
            Unfinished,
        }
        use Outcome::{Finished, Unfinished};
        // (camera cut, the stage runs, restart asked, outcome, the run
        // required where the stage runs).
        let rows = [
            (true, true, false, Finished, Some((0, 0))),
            (false, true, false, Finished, Some((1, 1))),
            (false, true, false, Finished, Some((2, 0))),
            // Rendered and dropped, then rendered again: the same run.
            (false, true, false, Unfinished, Some((3, 1))),
            (false, true, false, Unfinished, Some((3, 1))),
            (false, true, false, Finished, Some((3, 1))),
            // A restart in a frame that is not finished is not kept.
            (false, true, true, Unfinished, Some((0, 0))),
            (false, true, false, Finished, Some((4, 0))),
            // A cut's restart in a frame that is not finished is not kept
            // by the stage either: the run after it derives from the last
            // finished frame. The view does not draw that next frame as a
            // continuation: it holds the unfinished reset and renders the
            // next frame as a reset too (F-1, `View::settle`).
            (true, true, false, Unfinished, Some((0, 1))),
            (false, true, false, Finished, Some((5, 1))),
            // Skipped in an unfinished frame: the kept run is unchanged, so
            // the next run continues from the last finished frame. In the
            // renderer that holds for a stage that keeps its targets across
            // such a render (ambient occlusion, the ray-traced shadows); one
            // whose targets the render released restarts anyway (F-4).
            (false, false, false, Unfinished, None),
            (false, true, false, Finished, Some((6, 0))),
            // Skipped in a finished frame: the next restarts.
            (false, false, false, Finished, None),
            (false, true, false, Finished, Some((0, 0))),
            (false, true, false, Finished, Some((1, 1))),
            // A cut finished in a frame the stage skips ends the history.
            (true, false, false, Finished, None),
            (false, true, false, Finished, Some((0, 0))),
        ];
        for (row, (cut, runs, restart, outcome, required)) in rows.into_iter().enumerate() {
            let frame = history.begin(camera, cut, DVec3::ZERO, 0., 0);
            let run = runs.then(|| stage.begin(&frame, restart));
            assert_eq!(
                run.map(|run| (run.count, run.current)),
                required,
                "row {row}"
            );
            if outcome == Finished {
                history.finish(frame);
            }
            stage.settle(outcome == Finished);
        }
    }
}
