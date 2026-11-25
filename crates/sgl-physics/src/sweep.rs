//! The pair tests: how one shape meets another. Each takes the target's
//! position relative to the body, so the body sits at the origin and the
//! arithmetic's accuracy depends on the shapes' sizes and the distance
//! between them, never on where they are in the world.
//!
//! Every pair goes through one dispatch on the two [`Shape`]s and the
//! target's [`Cut`]; a new shape adds its pairs here. A box against a box
//! and a box against a cut box are both closed-form slab tests: the cut box
//! adds one axis, its plane's normal.

use crate::shape::{Cut, Shape};
use crate::space::{self, Vector};

/// Axes in the order a tie between them resolves for a query that knows no
/// up axis ([`cast`](crate::cast), [`ray`](crate::ray)): y, then x, then z.
/// The mover puts the axis most along its up first
/// ([`MoverConfig`](crate::MoverConfig)), so a body arriving exactly at a
/// corner while falling lands rather than being shoved sideways.
pub(crate) const TIE_ORDER: [usize; 3] = [1, 0, 2];

/// Where along a motion a body meets a target, and the face it meets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Contact<V> {
    /// Fraction of the motion travelled before contact, in `[0, 1]`.
    pub t: f32,
    /// Unit normal of the face met, pointing from the target toward the body.
    pub normal: V,
}

/// The first contact of `body` moving by `delta` with `target`, cut by
/// `cut`, at `rel`, or `None` when the motion misses it, stops short of it,
/// starts inside it, moves away from it, or is zero or not finite. An exact
/// tie between entering axes resolves to the first of them in `order`, and
/// a cut's axis enters after every cardinal one.
pub(crate) fn sweep<V: Vector>(
    body: &Shape<V>,
    delta: V,
    target: &Shape<V>,
    cut: Option<&Cut<V>>,
    rel: V,
    order: [usize; 3],
) -> Option<Contact<V>> {
    match (body, target, cut) {
        (Shape::Box { half: body }, Shape::Box { half: target }, None) => {
            box_sweep(*body, delta, *target, rel, order)
        }
        (Shape::Box { half: body }, Shape::Box { half: target }, Some(cut)) => {
            cut_box_sweep(*body, delta, &CutBox::new(*target, cut, rel), order)
        }
    }
}

/// Whether `body` and `target`, cut by `cut`, at `rel` share interior.
/// Touching is not overlap, so a body resting exactly on a surface is not
/// inside it.
pub(crate) fn overlap<V: Vector>(
    body: &Shape<V>,
    target: &Shape<V>,
    cut: Option<&Cut<V>>,
    rel: V,
) -> bool {
    match (body, target, cut) {
        (Shape::Box { half: body }, Shape::Box { half: target }, None) => {
            (0..V::AXES).all(|axis| {
                let (lo, hi) = (rel[axis] - target[axis], rel[axis] + target[axis]);
                body[axis] > lo && -body[axis] < hi
            })
        }
        (Shape::Box { half: body }, Shape::Box { half: target }, Some(cut)) => {
            CutBox::new(*target, cut, rel).overlaps(*body)
        }
    }
}

/// The contact for a body that already overlaps `target` (a spawn, a moved
/// collider, or rounding at a contact). It may move out of `target` or along
/// it but not deeper: motion toward `target` along its axis of least
/// penetration is a contact at `t = 0` on that axis's face, so a slide keeps
/// only the motion along the face. Ties resolve in `order`.
pub(crate) fn embedded<V: Vector>(
    body: &Shape<V>,
    delta: V,
    target: &Shape<V>,
    cut: Option<&Cut<V>>,
    rel: V,
    order: [usize; 3],
) -> Option<Contact<V>> {
    match (body, target, cut) {
        (Shape::Box { half: body }, Shape::Box { half: target }, Some(cut)) => {
            CutBox::new(*target, cut, rel).embedded(*body, delta, order)
        }
        (Shape::Box { half: body }, Shape::Box { half: target }, None) => {
            let depth = box_depths(*body, *target, rel);
            let axis = order[..V::AXES]
                .iter()
                .copied()
                .fold(order[0], |best, axis| {
                    if depth[axis] < depth[best] {
                        axis
                    } else {
                        best
                    }
                });
            let (along, motion) = (rel[axis], delta[axis]);
            // Compare signs directly: multiplying a tiny offset by a tiny
            // motion can underflow to a signed zero and let a body deeper.
            let inward = (along > 0.0 && motion > 0.0) || (along < 0.0 && motion < 0.0);
            inward.then(|| Contact {
                t: 0.0,
                normal: space::unit(axis, along < 0.0),
            })
        }
    }
}

/// The contact for a body overlapping `target` by no more than `allowance`
/// on some axis, on that axis's face, when `blocks` accepts the face's
/// normal: a body resting on a one-way platform within its skin, or one a
/// rounding step inside a face it touches in world coordinates. Axes are
/// tried in `order`.
pub(crate) fn resting<V: Vector>(
    body: &Shape<V>,
    target: &Shape<V>,
    cut: Option<&Cut<V>>,
    rel: V,
    allowance: V,
    order: [usize; 3],
    blocks: impl Fn(V) -> bool,
) -> Option<Contact<V>> {
    match (body, target, cut) {
        (Shape::Box { half: body }, Shape::Box { half: target }, Some(cut)) => {
            CutBox::new(*target, cut, rel).resting(*body, allowance, order, blocks)
        }
        (Shape::Box { half: body }, Shape::Box { half: target }, None) => {
            let depth = box_depths(*body, *target, rel);
            order[..V::AXES]
                .iter()
                .map(|&axis| (axis, space::unit(axis, rel[axis] <= 0.0)))
                .find(|&(axis, normal)| depth[axis] <= allowance[axis] && blocks(normal))
                .map(|(_, normal)| Contact { t: 0.0, normal })
        }
    }
}

/// How far two overlapping boxes reach into each other along each axis.
fn box_depths<V: Vector>(body: V, target: V, rel: V) -> V {
    let mut depth = V::ZERO;
    for axis in 0..V::AXES {
        depth[axis] = (body[axis] + target[axis]) - rel[axis].abs();
    }
    depth
}

/// The closed-form swept box test, the body at the origin.
///
/// On each axis, entry and exit times come from the gaps between the boxes'
/// own edges (`±body` and `rel ± target`, as [`overlap`] reads them), so
/// boxes that touch enter at exactly `t = 0`. The normal comes from the last
/// axis to enter, signed against the motion on that axis; ties resolve in
/// `order`.
///
/// Already overlapping, moving away, and zero or non-finite motion all
/// return `None` rather than an invented contact: an embedded body must
/// never receive a normal from a face it is already past.
fn box_sweep<V: Vector>(
    body: V,
    delta: V,
    target: V,
    rel: V,
    order: [usize; 3],
) -> Option<Contact<V>> {
    if delta == V::ZERO || !space::is_finite(delta) {
        return None;
    }
    let mut entries = [f32::NEG_INFINITY; 3];
    let mut exit = f32::INFINITY;
    for axis in 0..V::AXES {
        let (entry, leave) = slab(
            -body[axis],
            body[axis],
            delta[axis],
            rel[axis] - target[axis],
            rel[axis] + target[axis],
        )?;
        entries[axis] = entry;
        exit = space::min(exit, leave);
    }
    let axis = order[..V::AXES]
        .iter()
        .copied()
        .fold(order[0], |best, axis| {
            if entries[axis] > entries[best] {
                axis
            } else {
                best
            }
        });
    let entry = entries[axis];
    // `entry > exit`: the slabs are never crossed at the same time, the sweep
    // passes beside the target. Outside `[0, 1]`: contact is behind the start
    // (overlapping or moving away) or beyond the end of the motion. A NaN
    // entry fails the range test too.
    if entry > exit || !(0.0..=1.0).contains(&entry) {
        return None;
    }
    // `+ 0.0` turns the `-0.0` a touching body moving toward `-axis` gets
    // into `0.0`, so mirrored sweeps report the same `t`.
    Some(Contact {
        t: entry + 0.0,
        // The entering axis has motion: parallel axes never enter last.
        normal: space::unit(axis, delta[axis] < 0.0),
    })
}

/// A cut box in the body's frame: the edges of its solid part along each
/// axis, and the top of its solid part along the cut's normal, all relative
/// to the body's centre.
///
/// The edges along each axis are those of the solid part, not of the whole
/// box: a cut that planes a face away entirely leaves only an edge or a
/// corner of it standing, and a body's face meets that edge or corner
/// there, not where the face was. Along the normal only the top bounds the
/// solid: the bottom is a corner of the box, never a face of the separation
/// between a box and the cut box, so it never decides a contact.
///
/// Every axis a box and a cut box can be separated on is here: the cardinal
/// axes and the normal. In 3D the normal has a zero component, so the cross
/// product of any edge of one shape with any edge of the other is a
/// cardinal axis or the normal, and the slab test on these axes is exact.
#[derive(Clone, Copy)]
struct CutBox<V> {
    lo: V,
    hi: V,
    normal: V,
    top: f32,
}

impl<V: Vector> CutBox<V> {
    /// The box of half-extents `half` at `rel`, cut by `cut`.
    fn new(half: V, cut: &Cut<V>, rel: V) -> Self {
        let (normal, offset) = (cut.normal(), cut.offset());
        let (mut lo, mut hi) = (V::ZERO, V::ZERO);
        for axis in 0..V::AXES {
            let (min, max) = if normal[axis] == 0.0 {
                (-half[axis], half[axis])
            } else {
                // The farthest the solid part reaches along this axis toward
                // the normal: the plane at the box's corner that lies lowest
                // along the normal on every other axis, held to the box.
                let rest = reach_except(half, normal, axis);
                let along = (offset + rest) / normal[axis].abs();
                let limit = space::min(half[axis], space::max(-half[axis], along));
                if normal[axis] > 0.0 {
                    (-half[axis], limit)
                } else {
                    (-limit, half[axis])
                }
            };
            lo[axis] = rel[axis] + min;
            hi[axis] = rel[axis] + max;
        }
        let top = space::dot(rel, normal) + space::min(space::reach(half, normal), offset);
        Self {
            lo,
            hi,
            normal,
            top,
        }
    }

    /// Whether a box body of half-extents `body` at the origin shares
    /// interior with the solid part, by the same edges the sweep reads.
    fn overlaps(&self, body: V) -> bool {
        (0..V::AXES).all(|axis| body[axis] > self.lo[axis] && -body[axis] < self.hi[axis])
            && -space::reach(body, self.normal) < self.top
    }

    /// How deep a body of half-extents `body` reaches into the solid part
    /// along `axis`, and the face it is nearer to leaving by: `1` the upper,
    /// `-1` the lower, `0` neither (equally deep both ways).
    fn depth(&self, body: V, axis: usize) -> (f32, i8) {
        let up = self.hi[axis] + body[axis];
        let down = body[axis] - self.lo[axis];
        if up < down {
            (up, 1)
        } else if down < up {
            (down, -1)
        } else {
            (up, 0)
        }
    }

    /// [`embedded`] for a cut box: the axis of least penetration, the
    /// cardinal axes in `order` and then the cut's, stops motion into its
    /// face.
    fn embedded(&self, body: V, delta: V, order: [usize; 3]) -> Option<Contact<V>> {
        let axis = order[..V::AXES]
            .iter()
            .copied()
            .fold(order[0], |best, axis| {
                if self.depth(body, axis).0 < self.depth(body, best).0 {
                    axis
                } else {
                    best
                }
            });
        let (depth, side) = self.depth(body, axis);
        if self.top + space::reach(body, self.normal) < depth {
            return (space::dot(delta, self.normal) < 0.0).then_some(Contact {
                t: 0.0,
                normal: self.normal,
            });
        }
        // Compare signs directly, as the box does: a product of tiny values
        // can underflow to a signed zero.
        let motion = delta[axis];
        let inward = (side > 0 && motion < 0.0) || (side < 0 && motion > 0.0);
        inward.then(|| Contact {
            t: 0.0,
            normal: space::unit(axis, side > 0),
        })
    }

    /// [`resting`] for a cut box: the cardinal axes in `order`, then the
    /// cut's, whose allowance is the box allowance's reach along it.
    fn resting(
        &self,
        body: V,
        allowance: V,
        order: [usize; 3],
        blocks: impl Fn(V) -> bool,
    ) -> Option<Contact<V>> {
        order[..V::AXES]
            .iter()
            .map(|&axis| {
                let (depth, side) = self.depth(body, axis);
                (depth <= allowance[axis], space::unit(axis, side >= 0))
            })
            .chain(core::iter::once((
                self.top + space::reach(body, self.normal) <= space::reach(allowance, self.normal),
                self.normal,
            )))
            .find(|&(within, normal)| within && blocks(normal))
            .map(|(_, normal)| Contact { t: 0.0, normal })
    }
}

/// `Σ |d_i| half_i` over the axes `d` uses other than `skip`, in axis order.
fn reach_except<V: Vector>(half: V, d: V, skip: usize) -> f32 {
    let mut sum = 0.0;
    for axis in 0..V::AXES {
        if axis != skip && d[axis] != 0.0 {
            sum += d[axis].abs() * half[axis];
        }
    }
    sum
}

/// The closed-form swept box test against a cut box, the body at the
/// origin: [`box_sweep`]'s slab test on the cardinal axes, by the solid
/// part's edges, and on the cut's normal, by its top. The normal comes from
/// the last axis to enter; the cut's enters last only when strictly later
/// than every cardinal axis, and reports the plane's normal.
fn cut_box_sweep<V: Vector>(
    body: V,
    delta: V,
    target: &CutBox<V>,
    order: [usize; 3],
) -> Option<Contact<V>> {
    if delta == V::ZERO || !space::is_finite(delta) {
        return None;
    }
    let mut entries = [f32::NEG_INFINITY; 3];
    let mut exit = f32::INFINITY;
    for axis in 0..V::AXES {
        let (entry, leave) = slab(
            -body[axis],
            body[axis],
            delta[axis],
            target.lo[axis],
            target.hi[axis],
        )?;
        entries[axis] = entry;
        exit = space::min(exit, leave);
    }
    let axis = order[..V::AXES]
        .iter()
        .copied()
        .fold(order[0], |best, axis| {
            if entries[axis] > entries[best] {
                axis
            } else {
                best
            }
        });
    let (cut_entry, cut_exit) = below(
        -space::reach(body, target.normal),
        space::dot(delta, target.normal),
        target.top,
    )?;
    exit = space::min(exit, cut_exit);
    let on_cut = cut_entry > entries[axis];
    let entry = if on_cut { cut_entry } else { entries[axis] };
    // As the box: never crossed together, or outside the motion.
    if entry > exit || !(0.0..=1.0).contains(&entry) {
        return None;
    }
    let normal = if on_cut {
        target.normal
    } else {
        space::unit(axis, delta[axis] < 0.0)
    };
    Some(Contact {
        t: entry + 0.0,
        normal,
    })
}

/// Entry and exit times of an interval whose lower edge is `b_lo`, moving
/// by `d`, across the half-line below `t_hi`: [`slab`] with no lower edge.
fn below(b_lo: f32, d: f32, t_hi: f32) -> Option<(f32, f32)> {
    if d == 0.0 {
        (b_lo < t_hi).then_some((f32::NEG_INFINITY, f32::INFINITY))
    } else if d > 0.0 {
        Some((f32::NEG_INFINITY, (t_hi - b_lo) / d))
    } else {
        Some(((t_hi - b_lo) / d, f32::INFINITY))
    }
}

/// Entry and exit times of the interval `[b_lo, b_hi]` moving by `d` across
/// the interval `[t_lo, t_hi]`, from the gaps between their edges.
///
/// Exactly zero motion is parallel. A parallel body strictly overlapping the
/// slab is unconstrained on this axis (`-inf..inf`) and lets the others
/// decide; one outside or only touching can never enter. Overflowed entry
/// times fall outside `[0, 1]` and the sweep rejects them.
fn slab(b_lo: f32, b_hi: f32, d: f32, t_lo: f32, t_hi: f32) -> Option<(f32, f32)> {
    if d == 0.0 {
        (b_hi > t_lo && b_lo < t_hi).then_some((f32::NEG_INFINITY, f32::INFINITY))
    } else if d > 0.0 {
        Some(((t_lo - b_hi) / d, (t_hi - b_lo) / d))
    } else {
        Some(((t_hi - b_lo) / d, (t_lo - b_hi) / d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sgl_core::math::{Vec2, Vec3};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn boxed(hx: f32, hy: f32) -> Shape<Vec2> {
        Shape::Box {
            half: Vec2::new(hx, hy),
        }
    }

    /// A body centred at `(bx, by)` swept by `delta` into a box centred at
    /// `(cx, cy)`. Every case below is exact in `f32`, so the relative
    /// position is the difference of the centres with no rounding.
    fn sweep_at(
        (bx, by, bhx, bhy): (f32, f32, f32, f32),
        delta: Vec2,
        (cx, cy, hx, hy): (f32, f32, f32, f32),
    ) -> Option<Contact<Vec2>> {
        sweep(
            &boxed(bhx, bhy),
            delta,
            &boxed(hx, hy),
            None,
            Vec2::new(cx - bx, cy - by),
            TIE_ORDER,
        )
    }

    /// #249: boxes that share an edge or a corner do not overlap on any side,
    /// and one nudged inward on any side does: the strict comparison is the
    /// contract that lets a body rest on a surface.
    #[wasm_bindgen_test(unsupported = test)]
    fn touching_on_every_side_is_not_overlap_but_a_nudge_is() {
        let unit = boxed(1.0, 1.0);
        for (dx, dy) in [(2.0, 0.0), (-2.0, 0.0), (0.0, 2.0), (0.0, -2.0), (2.0, 2.0)] {
            assert!(
                !overlap(&unit, &unit, None, Vec2::new(dx, dy)),
                "touching at {dx},{dy}"
            );
            let nudged = Vec2::new(dx * 0.999, dy * 0.999);
            assert!(overlap(&unit, &unit, None, nudged), "nudged at {dx},{dy}");
        }
    }

    /// #249: a sweep whose path grazes the target's corner exactly (entry
    /// and exit coincide) is a contact at that instant, on the face the
    /// motion entered last; a body sliding along a touching edge with no
    /// motion toward it is not a contact at all.
    #[wasm_bindgen_test(unsupported = test)]
    fn corner_grazes_hit_and_edge_slides_do_not() {
        let target = (4.0, 4.0, 1.0, 1.0);
        let graze = sweep_at((0.0, 4.0, 1.0, 1.0), Vec2::new(4.0, 4.0), target)
            .expect("grazing the corner is a contact");
        assert_eq!(graze.t.to_bits(), 0.5f32.to_bits());
        assert_eq!(graze.normal, Vec2::NEG_X);

        // Bottom edge of the body exactly on the target's top edge, moving
        // sideways across it: parallel on y and only touching, so no hit.
        let slide = sweep_at((0.0, 6.0, 1.0, 1.0), Vec2::new(8.0, 0.0), target);
        assert_eq!(slide, None);
        // The same slide one hair lower is an immediate hit.
        let scrape = sweep_at((0.0, 5.999, 1.0, 1.0), Vec2::new(8.0, 0.0), target);
        assert!(scrape.is_some());
    }

    /// #427, in the body's frame: a body whose bottom edge equals the
    /// floor's top edge (so it does not overlap) hits the floor at once when
    /// it moves down, rather than missing it. The edges are built to be
    /// equal at awkward values and the test asserts that they are.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_body_exactly_touching_at_rounding_coordinates_hits_at_once() {
        let (body, floor) = (boxed(0.4, 0.249_400_82), boxed(50.0, 0.125));
        let rel = Vec2::new(0.0, -0.374_400_82);
        assert_eq!((rel.y + 0.125).to_bits(), (-0.249_400_82f32).to_bits());
        assert!(!overlap(&body, &floor, None, rel));
        let hit = sweep(&body, Vec2::new(0.0, -3.0), &floor, None, rel, TIE_ORDER)
            .expect("touching is a contact");
        assert_eq!(hit.t.to_bits(), 0.0f32.to_bits());
        assert_eq!(hit.normal, Vec2::Y);
    }

    /// The entry time and normal must come from the geometry: a body whose
    /// right edge is 4 units from the wall's left edge, moving 10 units,
    /// contacts at t = 0.4 with the normal pointing back along the motion.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_wall_reports_geometric_entry_time_and_facing_normal() {
        let hit = sweep_at(
            (-5.0, 0.0, 0.5, 0.5),
            Vec2::new(10.0, 0.0),
            (0.0, 0.0, 0.5, 0.5),
        )
        .expect("hit");
        assert_eq!(hit.normal, Vec2::new(-1.0, 0.0));
        assert!((hit.t - 0.4).abs() < 1e-6, "t was {}", hit.t);
    }

    /// Vertical faces must be signed against the motion: landing yields `+y`
    /// and a head-bump `-y`. A sign slip here inverts floor and ceiling.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_vertical_normals_are_signed_against_the_motion() {
        let surface = (0.0, 0.0, 5.0, 0.5);
        let landing = sweep_at((0.0, 5.0, 0.5, 0.5), Vec2::new(0.0, -10.0), surface).expect("hit");
        let bump = sweep_at((0.0, -5.0, 0.5, 0.5), Vec2::new(0.0, 10.0), surface).expect("hit");
        assert_eq!(landing.normal, Vec2::new(0.0, 1.0));
        assert_eq!(bump.normal, Vec2::new(0.0, -1.0));
    }

    /// Motion that passes beside the target, or stops short of it, is a miss.
    /// Contact reached exactly at the end of the motion is a hit at t = 1.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_miss_and_end_of_motion_boundaries() {
        let body = (-5.0, 0.0, 0.5, 0.5);
        let wall = (0.0, 0.0, 0.5, 0.5);
        let beside = (-5.0, 5.0, 0.5, 0.5);

        assert!(sweep_at(beside, Vec2::new(10.0, 0.0), wall).is_none());
        // Contact needs 4 units of travel; 1 unit stops short.
        assert!(sweep_at(body, Vec2::new(1.0, 0.0), wall).is_none());
        let hit = sweep_at(body, Vec2::new(4.0, 0.0), wall).expect("hit at t=1");
        assert!((hit.t - 1.0).abs() < 1e-6, "t was {}", hit.t);
    }

    /// An exact corner arrival (equal entry times on both axes) must resolve
    /// to the horizontal face, so a body falling into the corner of a ledge
    /// lands on it instead of being pushed sideways.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_corner_tie_resolves_to_the_horizontal_face() {
        // Both axes have 1 unit of gap and 4 units of travel: entry ties at 0.25.
        let hit = sweep_at(
            (-2.0, -2.0, 0.5, 0.5),
            Vec2::new(4.0, 4.0),
            (0.0, 0.0, 0.5, 0.5),
        )
        .expect("hit");
        assert!((hit.t - 0.25).abs() < 1e-6, "t was {}", hit.t);
        assert_eq!(hit.normal, Vec2::new(0.0, -1.0));
    }

    /// In 3D a tie resolves y, then x, then z: a cube arriving at the corner
    /// of another along all three axes at once lands on its top face, and
    /// with y out of the race the x face wins over the z face. Each axis has
    /// one unit of gap and four of travel, so every entry is 0.25.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_three_way_tie_resolves_y_then_x_then_z() {
        let cube = Shape::Box {
            half: Vec3::splat(0.5),
        };
        let rel = Vec3::splat(2.0);
        let all = sweep(&cube, Vec3::splat(4.0), &cube, None, rel, TIE_ORDER).expect("hit");
        assert_eq!((all.t, all.normal), (0.25, Vec3::NEG_Y));
        let level = Vec3::new(2.0, 0.0, 2.0);
        let xz = sweep(
            &cube,
            Vec3::new(4.0, 0.0, 4.0),
            &cube,
            None,
            level,
            TIE_ORDER,
        )
        .expect("hit");
        assert_eq!((xz.t, xz.normal), (0.25, Vec3::NEG_X));
    }

    /// A body that starts inside a collider reports no swept hit at all, in
    /// either direction. Reporting one would hand back a normal from a face
    /// the body is already past and trap it inside the box.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_embedded_body_reports_no_hit_in_any_direction() {
        for delta in [
            Vec2::new(3.0, 0.0),
            Vec2::new(-3.0, 0.0),
            Vec2::new(0.0, 3.0),
            Vec2::new(0.0, -3.0),
        ] {
            assert!(
                sweep_at((0.2, 0.0, 0.5, 0.5), delta, (0.0, 0.0, 1.0, 1.0)).is_none(),
                "embedded body must be free to move by {delta}"
            );
        }
    }

    /// Degenerate motion must not produce a contact: zero delta has no entry
    /// time, and NaN or infinite delta must fall out instead of yielding a
    /// bogus t (NaN comparisons are all false, so a naive range check passes).
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_zero_and_non_finite_delta_report_no_hit() {
        let (body, wall) = ((-5.0, 0.0, 0.5, 0.5), (0.0, 0.0, 0.5, 0.5));
        assert!(sweep_at(body, Vec2::ZERO, wall).is_none());
        assert!(sweep_at(body, Vec2::new(f32::NAN, 0.0), wall).is_none());
        assert!(sweep_at(body, Vec2::new(f32::INFINITY, 0.0), wall).is_none());
        assert!(sweep_at(body, Vec2::new(0.0, f32::NEG_INFINITY), wall).is_none());
    }

    /// A delta far too small to cross the 4-unit gap must miss. Dividing by a
    /// near-zero component yields an enormous entry time, so this fails the
    /// moment the range gate is loosened or the times are clamped.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_sub_epsilon_delta_cannot_reach_a_distant_target() {
        let (body, wall) = ((-5.0, 0.0, 0.5, 0.5), (0.0, 0.0, 0.5, 0.5));
        assert!(sweep_at(body, Vec2::new(1e-9, 0.0), wall).is_none());
        assert!(sweep_at(body, Vec2::new(1e-3, 0.0), wall).is_none());
    }

    /// A zero-extent body sweeps as a ray against the target's real faces, so
    /// callers can probe points without a special code path.
    #[wasm_bindgen_test(unsupported = test)]
    fn sweep_zero_extent_body_behaves_like_a_ray() {
        // The point starts 2 units from the block's left face and travels 8.
        let hit = sweep_at(
            (-4.0, 0.0, 0.0, 0.0),
            Vec2::new(8.0, 0.0),
            (0.0, 0.0, 2.0, 2.0),
        )
        .expect("hit");
        assert!((hit.t - 0.25).abs() < 1e-6, "t was {}", hit.t);
        assert_eq!(hit.normal, Vec2::new(-1.0, 0.0));
    }

    /// An embedded body may leave or slide, never go deeper: moving toward
    /// the collider along the axis of least penetration (here x, 0.3 deep
    /// against 1.5 on y) is a contact at t = 0 on that face; moving out or
    /// along it is not.
    #[wasm_bindgen_test(unsupported = test)]
    fn an_embedded_body_is_stopped_only_going_deeper() {
        let (body, block) = (boxed(0.5, 0.5), boxed(1.0, 1.0));
        let rel = Vec2::new(1.2, 0.0);
        let deeper =
            embedded(&body, Vec2::new(1.0, 0.0), &block, None, rel, TIE_ORDER).expect("blocked");
        assert_eq!((deeper.t, deeper.normal), (0.0, Vec2::NEG_X));
        assert_eq!(
            embedded(&body, Vec2::new(-1.0, 0.0), &block, None, rel, TIE_ORDER),
            None
        );
        assert_eq!(
            embedded(&body, Vec2::new(0.0, 1.0), &block, None, rel, TIE_ORDER),
            None
        );
    }
}
