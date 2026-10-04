//! The camera's history: the previous frame's unjittered view-projection and
//! the frames since the last reset. The renderer owns it; a frame commits to
//! it only when the caller finishes that frame.
use glam::Mat4;

/// One frame's view of the camera history.
#[derive(Clone, Copy)]
pub(crate) struct HistoryFrame {
    /// This frame's unjittered view-projection.
    pub stable: Mat4,
    /// The previous frame's unjittered view-projection (this frame's after a reset).
    pub previous: Mat4,
    /// False after a reset: history from earlier frames must not be reused.
    pub valid: bool,
    /// Frames since the last reset.
    pub frames: u32,
}

#[derive(Default)]
pub(crate) struct CameraHistory {
    previous: Option<Mat4>,
    frames: u32,
}

impl CameraHistory {
    /// The history of a frame whose view-projection is `stable`, restarted
    /// when `reset`. Nothing changes until `finish`.
    pub fn begin(&self, stable: Mat4, reset: bool) -> HistoryFrame {
        let (previous, frames) = if reset {
            (None, 0)
        } else {
            (self.previous, self.frames)
        };
        HistoryFrame {
            stable,
            previous: previous.unwrap_or(stable),
            valid: previous.is_some(),
            frames,
        }
    }

    /// Commits `frame`, which the caller submitted.
    pub fn finish(&mut self, frame: HistoryFrame) {
        self.previous = Some(frame.stable);
        self.frames = frame.frames.saturating_add(1);
    }
}
