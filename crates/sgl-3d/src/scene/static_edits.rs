//! The world bounds that static edits touched since the last submitted frame
//! (the architecture's "Edits and frames"): adding, removing or changing a
//! static instance, and replacing the geometry of a model one uses. A cache
//! of static content marks what each of them reaches as stale. They are kept
//! until `finish_frame`, so an abandoned frame leaves them for the next: each
//! edit's own bounds, merged conservatively only past `MAX_BOXES`, far more
//! than a streaming frame records. A cache rebuilt whole, which no frame need
//! show the bounds to (the static instance BVH), counts the edits instead.
use glam::{BVec3, DVec3, Mat4, Vec3};

/// The most boxes kept. Testing one box against one light's range costs
/// about 1.5 ns, so a full list costs a cache of 100 lights 0.15 ms; a
/// streamed world's frame records tens. Past it, the boxes merge in pairs
/// of spatial neighbours (`halve`).
const MAX_BOXES: usize = 1024;

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

/// `boxes` merged in pairs of neighbours along the Z-order curve of their
/// centres, quantised to 10 bits an axis across the centres' bounds: half as
/// many boxes, each holding the two it replaced. Returns the pairs merged.
fn halve(boxes: &mut Vec<[Vec3; 2]>) -> usize {
    let centre = |bounds: &[Vec3; 2]| (bounds[0] + bounds[1]) * 0.5;
    let (low, high) = boxes.iter().fold(
        (Vec3::INFINITY, Vec3::NEG_INFINITY),
        |(low, high), bounds| (low.min(centre(bounds)), high.max(centre(bounds))),
    );
    let scale = 1023. / (high - low).max(Vec3::splat(f32::MIN_POSITIVE));
    let spread = |bits: u32| {
        // Each of 10 bits to every third place.
        let mut x = bits & 0x3ff;
        x = (x | (x << 16)) & 0x0300_00ff;
        x = (x | (x << 8)) & 0x0300_f00f;
        x = (x | (x << 4)) & 0x030c_30c3;
        (x | (x << 2)) & 0x0924_9249
    };
    let order = |bounds: &[Vec3; 2]| {
        let cell = ((centre(bounds) - low) * scale).as_uvec3();
        spread(cell.x) | spread(cell.y) << 1 | spread(cell.z) << 2
    };
    boxes.sort_by_cached_key(order);
    let pairs = boxes.len() / 2;
    let kept = boxes.len().div_ceil(2);
    for at in 0..kept {
        let pair = &boxes[2 * at..(2 * at + 2).min(boxes.len())];
        boxes[at] = pair
            .iter()
            .copied()
            .reduce(union)
            .expect("pairs are never empty");
    }
    boxes.truncate(kept);
    pairs
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
        let merged = if self.pending.len() == MAX_BOXES {
            halve(&mut self.pending)
        } else {
            0
        };
        crate::counters::static_edit(merged);
        self.pending.push(bounds);
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Plausible defects: boxes lost or shrunk where the list overflows and
    // halves (a pair's second box dropped, a union taken wrongly), so an
    // edit's bounds stop reaching the caches that show it. The oracle is the
    // contract: every edit's bounds lie within a pending box, and the list
    // stays within its cap.
    #[wasm_bindgen_test(unsupported = test)]
    fn overflowing_edits_stay_within_the_boxes_kept() {
        let mut edits = StaticEdits::default();
        // Scattered over a kilometre: the list halves at the 1025th record
        // and every 512 after, five times in all.
        let recorded: Vec<[Vec3; 2]> = (0..MAX_BOXES * 3 + 7)
            .map(|i| {
                let t = i as f32;
                let low =
                    Vec3::new((t * 12.9898).sin(), (t * 4.1414).sin(), (t * 78.233).sin()) * 500.;
                [low, low + Vec3::new(16., 3. + t % 5., 16.)]
            })
            .collect();
        for &bounds in &recorded {
            edits.record(bounds);
        }
        let pending = edits.pending();
        assert!(pending.len() <= MAX_BOXES, "{} boxes kept", pending.len());
        for bounds in &recorded {
            assert!(
                pending
                    .iter()
                    .any(|kept| kept[0].cmple(bounds[0]).all() && kept[1].cmpge(bounds[1]).all()),
                "{bounds:?} is in no kept box"
            );
        }
    }
}
