//! Axis-aligned boxes and the closed-form swept-AABB slab test.

use crate::math::Vec2;

/// An axis-aligned box centered at `center` with per-axis `half`-extents.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    /// Center in world units.
    pub center: Vec2,
    /// Half-extents in world units. Zero is legal (a point or a line).
    pub half: Vec2,
}

impl Aabb {
    /// A box centered at `center` with half-extents `half`.
    #[must_use]
    pub const fn new(center: Vec2, half: Vec2) -> Self {
        Self { center, half }
    }

    /// A box spanning the `min`..`max` corner pair.
    #[must_use]
    pub fn from_min_max(min: Vec2, max: Vec2) -> Self {
        Self {
            center: (min + max) * 0.5,
            half: (max - min) * 0.5,
        }
    }

    /// The lower corner.
    #[must_use]
    pub fn min(&self) -> Vec2 {
        self.center - self.half
    }

    /// The upper corner.
    #[must_use]
    pub fn max(&self) -> Vec2 {
        self.center + self.half
    }

    /// The same box moved by `delta`.
    #[must_use]
    pub fn translated(&self, delta: Vec2) -> Self {
        Self {
            center: self.center + delta,
            half: self.half,
        }
    }

    /// Whether the two boxes share interior area. Strict: boxes that merely
    /// touch along an edge do **not** overlap, so a body resting exactly on a
    /// surface is not reported as inside it.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        let (a_min, a_max) = (self.min(), self.max());
        let (b_min, b_max) = (other.min(), other.max());
        a_min.x < b_max.x && a_max.x > b_min.x && a_min.y < b_max.y && a_max.y > b_min.y
    }
}

/// A swept contact: when along the motion it happens, and which face was hit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    /// Fraction of the swept `delta` traveled before contact, in `[0, 1]`.
    /// `0` means already touching along the entering axis; `1` means contact
    /// exactly at the end of the motion.
    pub t: f32,
    /// Axis-aligned unit normal of the surface struck, pointing from that
    /// surface toward the body.
    pub normal: Vec2,
}

/// Sweeps `body` by `delta` against the static box `target` and returns the
/// first contact, or `None` if the motion does not enter `target` within
/// `[0, 1]`.
///
/// A closed-form slab test: on each axis, entry and exit times come from the
/// gaps between the boxes' own edges ([`Aabb::min`] and [`Aabb::max`], as
/// [`Aabb::overlaps`] reads them), so boxes that touch enter at exactly
/// `t = 0`. The normal comes from the **last axis to enter** — the axis whose
/// entry time is greatest — signed against the motion on that axis.
///
/// Semantics at the edges, all of which return `None` rather than an invented
/// contact:
///
/// * **Already overlapping.** Entry lies behind the start, so an embedded body
///   reports no hit and is free to move out. It never receives a normal from a
///   face it is already past, which would otherwise pin it inside.
/// * **Moving away.** Same reason: entry is negative.
/// * **Zero or non-finite `delta`.** There is no motion to find an entry time
///   along.
///
/// A tie between the two axes (an exact corner hit) resolves to the horizontal
/// face — the floor or ceiling — because a body arriving exactly at a corner
/// while falling should land rather than be shoved sideways.
#[must_use]
pub fn sweep_aabb(body: &Aabb, delta: Vec2, target: &Aabb) -> Option<Hit> {
    if delta == Vec2::ZERO || !delta.is_finite() {
        return None;
    }

    let (b_min, b_max) = (body.min(), body.max());
    let (t_min, t_max) = (target.min(), target.max());

    let (entry_x, exit_x) = slab(b_min.x, b_max.x, delta.x, t_min.x, t_max.x)?;
    let (entry_y, exit_y) = slab(b_min.y, b_max.y, delta.y, t_min.y, t_max.y)?;

    let entry = entry_x.max(entry_y);
    let exit = exit_x.min(exit_y);

    // `entry > exit`: the slabs are never crossed at the same time — the sweep
    // passes beside the target. Outside `[0, 1]`: contact is behind the start
    // (already overlapping or moving away) or beyond the end of the motion. A
    // NaN entry fails the range test and lands here too.
    if entry > exit || !(0.0..=1.0).contains(&entry) {
        return None;
    }

    let normal = if entry_x > entry_y {
        // The x axis entered last: a vertical face.
        Vec2::new(if delta.x > 0.0 { -1.0 } else { 1.0 }, 0.0)
    } else {
        // The y axis entered last, ties included: a horizontal face.
        Vec2::new(0.0, if delta.y > 0.0 { -1.0 } else { 1.0 })
    };

    // `+ 0.0` turns the `-0.0` a touching body moving toward `-axis` gets
    // into `0.0`, so mirrored sweeps report the same `t`.
    Some(Hit {
        t: entry + 0.0,
        normal,
    })
}

/// Entry and exit times of the interval `[b_lo, b_hi]` moving by `d` across
/// the interval `[t_lo, t_hi]`, from the gaps between their edges.
///
/// Motion smaller than `f32::EPSILON` counts as parallel: it cannot carry the
/// body across a slab it is outside of, and dividing by it would manufacture
/// enormous or infinite times. A parallel body already overlapping the slab
/// (strictly, as [`Aabb::overlaps`] tests) is unconstrained on this axis
/// (`-inf`..`inf`) and lets the other axis decide; one outside or only
/// touching can never enter.
fn slab(b_lo: f32, b_hi: f32, d: f32, t_lo: f32, t_hi: f32) -> Option<(f32, f32)> {
    if d.abs() < f32::EPSILON {
        if b_hi > t_lo && b_lo < t_hi {
            Some((f32::NEG_INFINITY, f32::INFINITY))
        } else {
            None
        }
    } else if d > 0.0 {
        Some(((t_lo - b_hi) / d, (t_hi - b_lo) / d))
    } else {
        Some(((t_hi - b_lo) / d, (t_lo - b_hi) / d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #249: boxes that share an edge or a corner do not overlap on any side,
    /// and one nudged inward on any side does — the strict `<`/`>` in
    /// [`Aabb::overlaps`] is the contract that lets a body rest on a surface.
    #[wasm_bindgen_test(unsupported = test)]
    fn touching_on_every_side_is_not_overlap_but_a_nudge_is() {
        let body = Aabb::new(Vec2::ZERO, Vec2::splat(1.0));
        for (dx, dy) in [(2.0, 0.0), (-2.0, 0.0), (0.0, 2.0), (0.0, -2.0), (2.0, 2.0)] {
            let touching = Aabb::new(Vec2::new(dx, dy), Vec2::splat(1.0));
            assert!(!body.overlaps(&touching), "touching at {dx},{dy}");
            let nudged = Aabb::new(Vec2::new(dx * 0.999, dy * 0.999), Vec2::splat(1.0));
            assert!(body.overlaps(&nudged), "nudged at {dx},{dy}");
        }
    }

    /// #249: a sweep whose path grazes the target's corner exactly (entry
    /// and exit coincide) is a contact at that instant, on the face the
    /// motion entered last; a body sliding along a touching edge with no
    /// motion toward it is not a contact at all.
    #[wasm_bindgen_test(unsupported = test)]
    fn corner_grazes_hit_and_edge_slides_do_not() {
        let target = Aabb::new(Vec2::new(4.0, 4.0), Vec2::splat(1.0));
        let graze = sweep_aabb(
            &Aabb::new(Vec2::new(0.0, 4.0), Vec2::splat(1.0)),
            Vec2::new(4.0, 4.0),
            &target,
        )
        .expect("grazing the corner is a contact");
        assert_eq!(graze.t.to_bits(), 0.5f32.to_bits());
        assert_eq!(graze.normal, Vec2::NEG_X);

        // Bottom edge of the body exactly on the target's top edge, moving
        // sideways across it: parallel on y and only touching, so no hit.
        let slide = sweep_aabb(
            &Aabb::new(Vec2::new(0.0, 6.0), Vec2::splat(1.0)),
            Vec2::new(8.0, 0.0),
            &target,
        );
        assert_eq!(slide, None);
        // The same slide one hair lower is an immediate hit.
        let scrape = sweep_aabb(
            &Aabb::new(Vec2::new(0.0, 5.999), Vec2::splat(1.0)),
            Vec2::new(8.0, 0.0),
            &target,
        );
        assert!(scrape.is_some());
    }

    /// #427: a body resting exactly on a floor at coordinates that round
    /// (its bottom edge equals the floor's top edge, so it does not overlap)
    /// hits the floor at once when it moves down, rather than missing it.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_body_exactly_touching_at_rounding_coordinates_hits_at_once() {
        let body = aabb(1.090_340_4, 4.711_776_3, 0.4, 0.249_400_84);
        let floor = aabb(1.090_340_4, 4.337_375_6, 50.0, 0.125);
        assert_eq!(body.min().y.to_bits(), floor.max().y.to_bits());
        assert!(!body.overlaps(&floor));
        let hit = sweep_aabb(&body, Vec2::new(0.0, -3.0), &floor).expect("touching is a contact");
        assert_eq!(hit.t.to_bits(), 0.0f32.to_bits());
        assert_eq!(hit.normal, Vec2::Y);
    }

    fn aabb(cx: f32, cy: f32, hx: f32, hy: f32) -> Aabb {
        Aabb::new(Vec2::new(cx, cy), Vec2::new(hx, hy))
    }

    /// Corner-pair construction must recover the geometric center and half
    /// extents of an asymmetric box, and touching boxes must not count as
    /// overlapping — a body resting on a floor would otherwise read as
    /// embedded in it.
    #[wasm_bindgen_test(unsupported = test)]
    fn from_min_max_recovers_geometry_and_touching_is_not_overlap() {
        let a = Aabb::from_min_max(Vec2::new(1.0, 2.0), Vec2::new(4.0, 6.0));
        assert_eq!(a.center, Vec2::new(2.5, 4.0));
        assert_eq!(a.half, Vec2::new(1.5, 2.0));
        assert_eq!(a.min(), Vec2::new(1.0, 2.0));
        assert_eq!(a.max(), Vec2::new(4.0, 6.0));

        let touching = Aabb::from_min_max(Vec2::new(4.0, 2.0), Vec2::new(5.0, 6.0));
        assert!(!a.overlaps(&touching), "shared edge is not an overlap");
        assert!(a.overlaps(&touching.translated(Vec2::new(-0.5, 0.0))));
    }

    /// The entry time and normal must come from the geometry: a body whose
    /// right edge is 4 units from the wall's left edge, moving 10 units,
    /// contacts at t = 0.4 with the normal pointing back along the motion.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_wall_reports_geometric_entry_time_and_facing_normal() {
        let body = aabb(-5.0, 0.0, 0.5, 0.5);
        let wall = aabb(0.0, 0.0, 0.5, 0.5);
        let hit = sweep_aabb(&body, Vec2::new(10.0, 0.0), &wall).expect("hit");
        assert_eq!(hit.normal, Vec2::new(-1.0, 0.0));
        assert!((hit.t - 0.4).abs() < 1e-6, "t was {}", hit.t);
    }

    /// Vertical faces must be signed against the motion: landing yields `+y`
    /// and a head-bump `-y`. A sign slip here inverts floor and ceiling.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_vertical_normals_are_signed_against_the_motion() {
        let surface = aabb(0.0, 0.0, 5.0, 0.5);
        let above = aabb(0.0, 5.0, 0.5, 0.5);
        let below = aabb(0.0, -5.0, 0.5, 0.5);
        let landing = sweep_aabb(&above, Vec2::new(0.0, -10.0), &surface).expect("hit");
        let bump = sweep_aabb(&below, Vec2::new(0.0, 10.0), &surface).expect("hit");
        assert_eq!(landing.normal, Vec2::new(0.0, 1.0));
        assert_eq!(bump.normal, Vec2::new(0.0, -1.0));
    }

    /// Motion that passes beside the target, or stops short of it, is a miss.
    /// Contact reached exactly at the end of the motion is a hit at t = 1.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_miss_and_end_of_motion_boundaries() {
        let body = aabb(-5.0, 0.0, 0.5, 0.5);
        let wall = aabb(0.0, 0.0, 0.5, 0.5);
        let beside = aabb(-5.0, 5.0, 0.5, 0.5);

        assert!(sweep_aabb(&beside, Vec2::new(10.0, 0.0), &wall).is_none());
        // Contact needs 4 units of travel; 1 unit stops short.
        assert!(sweep_aabb(&body, Vec2::new(1.0, 0.0), &wall).is_none());
        let hit = sweep_aabb(&body, Vec2::new(4.0, 0.0), &wall).expect("hit at t=1");
        assert!((hit.t - 1.0).abs() < 1e-6, "t was {}", hit.t);
    }

    /// An exact corner arrival (equal entry times on both axes) must resolve to
    /// the horizontal face, so a body falling into the corner of a ledge lands
    /// on it instead of being pushed sideways.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_corner_tie_resolves_to_the_horizontal_face() {
        // Both axes have 1 unit of gap and 4 units of travel: entry ties at 0.25.
        let body = aabb(-2.0, -2.0, 0.5, 0.5);
        let block = aabb(0.0, 0.0, 0.5, 0.5);
        let hit = sweep_aabb(&body, Vec2::new(4.0, 4.0), &block).expect("hit");
        assert!((hit.t - 0.25).abs() < 1e-6, "t was {}", hit.t);
        assert_eq!(hit.normal, Vec2::new(0.0, -1.0));
    }

    /// A body that starts inside a collider reports no hit at all, in either
    /// direction. Reporting a contact here would hand back a normal from a face
    /// the body is already past and trap it inside the box forever.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_embedded_body_reports_no_hit_in_any_direction() {
        let block = aabb(0.0, 0.0, 1.0, 1.0);
        let body = aabb(0.2, 0.0, 0.5, 0.5);
        for delta in [
            Vec2::new(3.0, 0.0),
            Vec2::new(-3.0, 0.0),
            Vec2::new(0.0, 3.0),
            Vec2::new(0.0, -3.0),
        ] {
            assert!(
                sweep_aabb(&body, delta, &block).is_none(),
                "embedded body must be free to move by {delta}"
            );
        }
    }

    /// Degenerate motion must not produce a contact: zero delta has no entry
    /// time, and NaN or infinite delta must fall out instead of yielding a
    /// bogus t (NaN comparisons are all false, so a naive range check passes).
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_zero_and_non_finite_delta_report_no_hit() {
        let body = aabb(-5.0, 0.0, 0.5, 0.5);
        let wall = aabb(0.0, 0.0, 0.5, 0.5);
        assert!(sweep_aabb(&body, Vec2::ZERO, &wall).is_none());
        assert!(sweep_aabb(&body, Vec2::new(f32::NAN, 0.0), &wall).is_none());
        assert!(sweep_aabb(&body, Vec2::new(f32::INFINITY, 0.0), &wall).is_none());
        assert!(sweep_aabb(&body, Vec2::new(0.0, f32::NEG_INFINITY), &wall).is_none());
    }

    /// A delta far too small to cross the 4-unit gap must miss. Dividing by a
    /// near-zero component yields an enormous entry time, so this fails the
    /// moment the range gate is loosened or the times are clamped.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_sub_epsilon_delta_cannot_reach_a_distant_target() {
        let body = aabb(-5.0, 0.0, 0.5, 0.5);
        let wall = aabb(0.0, 0.0, 0.5, 0.5);
        assert!(sweep_aabb(&body, Vec2::new(1e-9, 0.0), &wall).is_none());
        assert!(sweep_aabb(&body, Vec2::new(1e-3, 0.0), &wall).is_none());
    }

    /// A zero-extent body sweeps as a ray against the target's real faces, so
    /// callers can probe points without a special code path.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_zero_extent_body_behaves_like_a_ray() {
        let point = aabb(-4.0, 0.0, 0.0, 0.0);
        let block = aabb(0.0, 0.0, 2.0, 2.0);
        // The point starts 2 units from the block's left face and travels 8.
        let hit = sweep_aabb(&point, Vec2::new(8.0, 0.0), &block).expect("hit");
        assert!((hit.t - 0.25).abs() < 1e-6, "t was {}", hit.t);
        assert_eq!(hit.normal, Vec2::new(-1.0, 0.0));
    }
}
