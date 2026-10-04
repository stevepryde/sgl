//! The world bounds that static edits touched since the last submitted frame
//! (the architecture's "Edits and frames"): adding, removing or changing a
//! static instance, and replacing the geometry of a model one uses. A cache
//! of static content marks what they reach as stale. They are kept until
//! `finish_frame`, so an abandoned frame leaves them for the next, and are
//! merged conservatively.
use glam::{Mat4, Vec3};

/// The most boxes kept: past it, a new box merges with the one whose union
/// with it grows least.
const MAX_BOXES: usize = 16;

/// The axis-aligned bounds of `bounds` at `pose`; at the identity, `bounds`.
pub(crate) fn posed_bounds(bounds: [Vec3; 2], pose: Mat4) -> [Vec3; 2] {
    let mut posed = [Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)];
    for x in [bounds[0].x, bounds[1].x] {
        for y in [bounds[0].y, bounds[1].y] {
            for z in [bounds[0].z, bounds[1].z] {
                let p = pose.transform_point3(Vec3::new(x, y, z));
                posed = [posed[0].min(p), posed[1].max(p)];
            }
        }
    }
    posed
}

fn union(a: [Vec3; 2], b: [Vec3; 2]) -> [Vec3; 2] {
    [a[0].min(b[0]), a[1].max(b[1])]
}

/// A box's size, as the sum of its extents, which a flat box keeps.
fn size(bounds: [Vec3; 2]) -> f32 {
    (bounds[1] - bounds[0]).element_sum()
}

#[derive(Default)]
pub(crate) struct StaticEdits {
    pending: Vec<[Vec3; 2]>,
    /// Frames finished so far.
    finished: u64,
}

impl StaticEdits {
    /// Records the world `bounds` a static edit touched. Bounds of empty
    /// geometry touch nothing.
    pub fn record(&mut self, bounds: [Vec3; 2]) {
        if !bounds[0].is_finite() || !bounds[1].is_finite() {
            return;
        }
        if self.pending.len() < MAX_BOXES {
            self.pending.push(bounds);
            return;
        }
        let growth = |pending: &[Vec3; 2]| size(union(*pending, bounds)) - size(*pending);
        let nearest = self
            .pending
            .iter_mut()
            .min_by(|a, b| growth(a).total_cmp(&growth(b)))
            .expect("MAX_BOXES is positive");
        *nearest = union(*nearest, bounds);
    }

    /// The bounds static edits touched since the last submitted frame.
    pub fn pending(&self) -> &[[Vec3; 2]] {
        &self.pending
    }

    /// The frames finished so far: a cache that examined the pending bounds
    /// of every frame since it last drew is up to date with them.
    pub fn finished(&self) -> u64 {
        self.finished
    }

    /// Commits a submitted frame: its edits are no longer pending.
    pub fn finish(&mut self) {
        self.pending.clear();
        self.finished += 1;
    }
}
