//! The camera's history: the last submitted frame's unjittered view and
//! projection, with the jitter antialiasing applied to that frame, and the
//! frames since the last reset. The renderer owns it; a frame commits to it
//! only when the caller finishes that frame. It is kept in the render frame
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
}

#[derive(Default)]
pub(crate) struct CameraHistory {
    previous: Option<CameraFrame>,
    frames: u32,
    /// The render origin `previous` is expressed in.
    origin: DVec3,
}

impl CameraHistory {
    /// The history of a frame seen by `camera` with the scene's origin at
    /// `origin`, restarted when `reset`. Nothing changes until `finish`, so
    /// an abandoned frame translates nothing twice.
    pub fn begin(&self, camera: CameraFrame, reset: bool, origin: DVec3) -> HistoryFrame {
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
        }
    }

    /// Commits `frame`, which the caller submitted.
    pub fn finish(&mut self, frame: HistoryFrame) {
        self.previous = Some(frame.camera);
        self.frames = frame.frames.saturating_add(1);
        self.origin = frame.origin;
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
        let first = history.begin(camera(eye), false, DVec3::ZERO);
        history.finish(first);
        let moved = Vec3::new(5000., 0., -7000.);
        let point = Vec3::new(5010.5, 11., -7031.);
        let before = first.camera.jittered_view_projection() * point.extend(1.);
        // An abandoned frame after the move, then the frame that finishes.
        for _ in 0..2 {
            let frame = history.begin(camera(eye - moved), false, moved.as_dvec3());
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
        let reset = history.begin(camera(eye - moved), true, moved.as_dvec3());
        assert!(reset.previous_camera.is_none() && !reset.valid);
    }
}
