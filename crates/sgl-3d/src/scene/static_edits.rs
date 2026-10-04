//! The world bounds that static edits touched since the last submitted frame
//! (the architecture's "Edits and frames"): adding, removing or changing a
//! static instance, and replacing the geometry of a model one uses. A cache
//! of static content marks what they reach as stale. They are kept until
//! `finish_frame`, so an abandoned frame leaves them for the next, and are
//! merged conservatively. A cache rebuilt whole, which no frame need show
//! the bounds to (the static instance BVH), counts the edits instead.
use glam::{BVec3, DVec3, Mat4, Vec3};

/// The most boxes kept: past it, a new box merges with the one whose union
/// with it grows least.
const MAX_BOXES: usize = 16;

/// The axis-aligned bounds of `bounds` at `pose`; at the identity, `bounds`.
/// The corners are posed in double precision and rounded outward, so the
/// box holds the posed box whatever the rounding.
pub(crate) fn posed_bounds(bounds: [Vec3; 2], pose: Mat4) -> [Vec3; 2] {
    let pose = pose.as_dmat4();
    let [low, high] = bounds.map(|corner| corner.as_dvec3());
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    for corner in 0..8 {
        let p = pose.transform_point3(DVec3::select(
            BVec3::new(corner & 1 != 0, corner & 2 != 0, corner & 4 != 0),
            high,
            low,
        ));
        min = min.min(p);
        max = max.max(p);
    }
    outward(min, max)
}

/// The `f32` box that holds the `f64` box from `min` to `max`.
fn outward(min: DVec3, max: DVec3) -> [Vec3; 2] {
    let down = |v: f64| {
        let rounded = v as f32;
        if f64::from(rounded) > v {
            rounded.next_down()
        } else {
            rounded
        }
    };
    let up = |v: f64| {
        let rounded = v as f32;
        if f64::from(rounded) < v {
            rounded.next_up()
        } else {
            rounded
        }
    };
    [
        Vec3::from_array(min.to_array().map(down)),
        Vec3::from_array(max.to_array().map(up)),
    ]
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
    /// Static edits recorded so far.
    edits: u64,
}

impl StaticEdits {
    /// Records the world `bounds` a static edit touched. Bounds of empty
    /// geometry touch nothing.
    pub fn record(&mut self, bounds: [Vec3; 2]) {
        self.edits += 1;
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

    /// The static edits recorded so far, whether or not a frame was
    /// submitted since: a cache built after this many is up to date with
    /// them until it changes.
    pub fn edits(&self) -> u64 {
        self.edits
    }

    /// Moves the render origin by `by` (`Scene::move_origin`): the pending
    /// bounds, rounded outward, so they still hold what the edits touched.
    /// It is no static edit.
    pub fn move_origin(&mut self, by: Vec3) {
        let by = by.as_dvec3();
        for bounds in &mut self.pending {
            *bounds = outward(bounds[0].as_dvec3() - by, bounds[1].as_dvec3() - by);
        }
    }

    /// Commits a submitted frame: its edits are no longer pending.
    pub fn finish(&mut self) {
        self.pending.clear();
        self.finished += 1;
    }
}
