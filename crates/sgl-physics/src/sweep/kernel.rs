//! The pairs the closed-form box sweep does not cover: every pair a ball, a
//! capsule or a convex shape makes, a cut box met by one of them included.
//! Each shape becomes a convex kernel proxy ([`crate::convex`]) in the body's
//! frame, the body at the origin, and the kernel measures and casts it.
//!
//! **Contact a slop apart.** The kernel cannot tell shapes that touch from
//! shapes that overlap a little (two radius-less cores that touch read as
//! overlapping), so a pair here makes contact when the shapes come within a
//! small length, the *slop*, of each other: the kernel's soft contact. A
//! cast stops the body between `slop` and `1.25 * slop` short of the target
//! and never past it. A body already within that band meets the target at
//! once when it moves into it.
//!
//! **The slop** scales with the shapes, never a fixed length ([`slop`]).
//!
//! **Never a clear path.** A cast the kernel gives up on stops the body
//! where it gave up, and an overlap with no known way out blocks all
//! motion: neither is read as free space.

use sgl_core::math::{Vec2, Vec3};

use super::{Contact, Tolerance};
use crate::convex::{self, Cast, CastState, ClosestPoints, DistanceState, d2, d3};
use crate::shape::{Cut, Shape};
use crate::space::{self, Vector};

/// Room for a proxy's points in either dimension: the 3D kernel's
/// capacity, which is the larger.
const STORAGE: usize = d3::MAX_POINTS;

/// A pair's slop as a share of the smaller shape's extent (its largest
/// half-extent): one hundredth, a contact error no game sees at play scale.
const EXTENT_SLOP: f32 = 0.01;

/// The most a pair's slop may be, as a share of the smaller radius: a
/// quarter keeps a contact on a round surface within a third of its radius
/// of where the surfaces would touch.
const RADIUS_SLOP: f32 = 0.25;

/// The mover's slop as a share of its skin: a sixteenth, so the band the
/// kernel stops in (a quarter of the slop) is a sixty-fourth of the skin.
const SKIN_SLOP: f32 = 1.0 / 16.0;

/// The least slop, in units of the pair's largest coordinate: 32 units in
/// the last place. The distance between the cores carries rounding of a few
/// units in the last place of the largest coordinate the kernel adds and
/// multiplies, and a cast must land within a quarter of the slop of its
/// target, so the slop is at least eight times a four-unit error.
const ROUNDING_SLOP: f32 = 32.0 * f32::EPSILON;

/// How many slops from the target a pair is in contact: the cast's target
/// is one slop, reached within a quarter more.
const CONTACT_BAND: f32 = 1.25;

/// A shape as the kernel takes it: the hull of a few points, grown by a
/// radius, in the body's frame.
#[derive(Clone, Copy, Debug)]
struct Hull<V> {
    points: [V; STORAGE],
    count: usize,
    radius: f32,
}

impl<V: Vector> Hull<V> {
    fn empty(radius: f32) -> Self {
        Self {
            points: [V::ZERO; STORAGE],
            count: 0,
            radius,
        }
    }

    fn push(&mut self, point: V) {
        debug_assert!(self.count < STORAGE, "a proxy holds at most {STORAGE}");
        if self.count < STORAGE {
            self.points[self.count] = point;
            self.count += 1;
        }
    }

    fn as_slice(&self) -> &[V] {
        &self.points[..self.count]
    }

    /// `shape` centred at `at`, cut by `cut` when it is a box.
    fn of(shape: &Shape<V>, cut: Option<&Cut<V>>, at: V) -> Self {
        match (*shape, cut) {
            (Shape::Box { half }, None) => corners(half, at),
            (Shape::Box { half }, Some(cut)) => cut_corners(half, cut, at),
            (Shape::Ball { radius }, _) => {
                let mut out = Self::empty(radius);
                out.push(at);
                out
            }
            (Shape::Capsule { half_axis, radius }, _) => {
                let mut out = Self::empty(radius);
                out.push(at - half_axis);
                out.push(at + half_axis);
                out
            }
            (Shape::Convex(convex), _) => {
                let mut out = Self::empty(0.0);
                for &point in convex.points() {
                    out.push(at + point);
                }
                out
            }
        }
    }

    /// The same shape moved by `by`.
    fn moved(&self, by: V) -> Self {
        let mut out = *self;
        for point in &mut out.points[..self.count] {
            *point = *point + by;
        }
        out
    }

    /// The same shape grown by `by` beyond its radius.
    fn grown(&self, by: f32) -> Self {
        Self {
            radius: self.radius + by,
            ..*self
        }
    }

    /// The largest absolute coordinate of its points.
    fn largest(&self) -> f32 {
        self.as_slice().iter().fold(0.0, |largest, &point| {
            space::max(largest, space::largest_abs(point))
        })
    }

    fn d2(&self) -> d2::Proxy {
        let mut points = [Vec2::ZERO; d2::MAX_POINTS];
        for (slot, &point) in points.iter_mut().zip(self.as_slice()) {
            *slot = Vec2::new(point[0], point[1]);
        }
        d2::Proxy::new(&points[..self.count.min(d2::MAX_POINTS)], self.radius)
    }

    fn d3(&self) -> d3::Proxy {
        let mut points = [Vec3::ZERO; d3::MAX_POINTS];
        for (slot, &point) in points.iter_mut().zip(self.as_slice()) {
            *slot = Vec3::new(point[0], point[1], point[2]);
        }
        d3::Proxy::new(&points[..self.count], self.radius)
    }
}

/// The corners of a box of half-extents `half` at `at`, one per corner
/// along the axes it has extent on: a box with no extent is one point.
fn corners<V: Vector>(half: V, at: V) -> Hull<V> {
    let mut axes = [0usize; 3];
    let mut spread = 0;
    for axis in 0..V::AXES {
        if half[axis] != 0.0 {
            axes[spread] = axis;
            spread += 1;
        }
    }
    let mut out = Hull::empty(0.0);
    for corner in 0..1usize << spread {
        let mut point = at;
        for (bit, &axis) in axes[..spread].iter().enumerate() {
            point[axis] = if corner & (1 << bit) == 0 {
                at[axis] - half[axis]
            } else {
                at[axis] + half[axis]
            };
        }
        out.push(point);
    }
    out
}

/// The solid part of a box of half-extents `half` at `at` cut by `cut`: its
/// corners on the solid side of the plane, then the points where the plane
/// crosses its edges. Their hull is the solid part: at most five points in
/// 2D and, the plane turning about one axis, ten in 3D.
fn cut_corners<V: Vector>(half: V, cut: &Cut<V>, at: V) -> Hull<V> {
    let corner = |index: usize| {
        let mut local = V::ZERO;
        for axis in 0..V::AXES {
            local[axis] = if index & (1 << axis) == 0 {
                -half[axis]
            } else {
                half[axis]
            };
        }
        local
    };
    let side = |local: V| space::dot(local, cut.normal()) - cut.offset();
    let mut out = Hull::empty(0.0);
    for index in 0..1usize << V::AXES {
        let local = corner(index);
        if side(local) <= 0.0 {
            out.push(at + local);
        }
    }
    for index in 0..1usize << V::AXES {
        for axis in 0..V::AXES {
            if index & (1 << axis) != 0 {
                continue;
            }
            let (a, b) = (corner(index), corner(index | 1 << axis));
            let (side_a, side_b) = (side(a), side(b));
            if (side_a < 0.0 && side_b > 0.0) || (side_a > 0.0 && side_b < 0.0) {
                let mut crossing = a;
                crossing[axis] = a[axis] + (b[axis] - a[axis]) * (side_a / (side_a - side_b));
                out.push(at + crossing);
            }
        }
    }
    out
}

/// The closest points between the cores of `a` and `b`, through the kernel
/// of the space.
fn distance<V: Vector>(a: &Hull<V>, b: &Hull<V>) -> ClosestPoints<V> {
    if V::AXES == 2 {
        let found = d2::distance(&a.d2(), &b.d2(), false);
        ClosestPoints {
            state: found.state,
            point_a: from2(found.point_a),
            point_b: from2(found.point_b),
            normal: from2(found.normal),
            distance: found.distance,
            iterations: found.iterations,
        }
    } else {
        let found = d3::distance(&a.d3(), &b.d3(), false);
        ClosestPoints {
            state: found.state,
            point_a: from3(found.point_a),
            point_b: from3(found.point_b),
            normal: from3(found.normal),
            distance: found.distance,
            iterations: found.iterations,
        }
    }
}

/// Where `b`, moved by `translation`, first comes within the kernel's
/// target of the fixed `a`, through the kernel of the space.
fn shape_cast<V: Vector>(a: &Hull<V>, b: &Hull<V>, translation: V, slop: f32) -> Cast<V> {
    if V::AXES == 2 {
        let motion = Vec2::new(translation[0], translation[1]);
        let cast = d2::shape_cast(&a.d2(), &b.d2(), motion, 1.0, false, slop);
        Cast {
            state: cast.state,
            fraction: cast.fraction,
            normal: from2(cast.normal),
            point: from2(cast.point),
            iterations: cast.iterations,
        }
    } else {
        let motion = Vec3::new(translation[0], translation[1], translation[2]);
        let cast = d3::shape_cast(&a.d3(), &b.d3(), motion, 1.0, false, slop);
        Cast {
            state: cast.state,
            fraction: cast.fraction,
            normal: from3(cast.normal),
            point: from3(cast.point),
            iterations: cast.iterations,
        }
    }
}

fn from2<V: Vector>(v: Vec2) -> V {
    let mut out = V::ZERO;
    out[0] = v.x;
    out[1] = v.y;
    out
}

fn from3<V: Vector>(v: Vec3) -> V {
    let mut out = V::ZERO;
    out[0] = v.x;
    out[1] = v.y;
    out[2] = v.z;
    out
}

/// Where a pair's cores stand.
enum Start<V> {
    /// The cores overlap, or touch closer than `f32` tells: no normal.
    Deep,
    /// The cores are apart. `gap` is between the surfaces, negative when
    /// they overlap; the unit normal runs from the target toward the body,
    /// and `point` is the nearest point of the target's core.
    Apart { gap: f32, normal: V, point: V },
}

/// A body and a target as the kernel meets them, in the body's frame.
struct Pair<V> {
    target: Hull<V>,
    body: Hull<V>,
}

impl<V: Vector> Pair<V> {
    /// `body` at the origin and `target`, cut by `cut`, at `rel`, or `None`
    /// when the body's own numbers describe nothing.
    fn new(body: &Shape<V>, target: &Shape<V>, cut: Option<&Cut<V>>, rel: V) -> Option<Self> {
        body.is_sound().then(|| Self {
            target: Hull::of(target, cut, rel),
            body: Hull::of(body, None, V::ZERO),
        })
    }

    fn start(&self) -> Start<V> {
        let found = distance(&self.target, &self.body);
        if found.state == DistanceState::Overlapped || !convex::is_unit(found.normal) {
            return Start::Deep;
        }
        Start::Apart {
            gap: found.distance - (self.target.radius + self.body.radius),
            normal: found.normal,
            point: found.point_a,
        }
    }

    /// The pair with the body moved back along `delta` by `back`, or `None`
    /// when `delta` has no direction.
    fn backed(&self, delta: V, back: f32) -> Option<Self> {
        let (along, _) = space::direction(delta)?;
        Some(Self {
            target: self.target,
            body: self.body.moved(along * -back),
        })
    }
}

/// Whether a pair can make contact at all: two shapes with no extent (a
/// point against a point) never do.
pub(crate) fn meets<V: Vector>(body: &Shape<V>, target: &Shape<V>) -> bool {
    extent(body) > 0.0 || extent(target) > 0.0
}

/// A shape's largest half-extent.
fn extent<V: Vector>(shape: &Shape<V>) -> f32 {
    space::largest_abs(shape.half_extents())
}

/// The slop a pair makes contact within, a positive length.
///
/// A query's ([`Tolerance::Query`]) is a hundredth of the smaller shape's
/// extent ([`EXTENT_SLOP`]), at most a quarter of the smaller radius
/// ([`RADIUS_SLOP`]); a shape with no extent (a ray) or no radius (a box, a
/// convex shape) does not count. The mover's ([`Tolerance::Skin`]) is a
/// sixteenth of its skin ([`SKIN_SLOP`]), or the query's when the skin is
/// zero. Either is at least [`ROUNDING_SLOP`] of the largest coordinate the
/// cast meets, so `f32` can reach it.
fn slop<V: Vector>(
    body: &Shape<V>,
    target: &Shape<V>,
    pair: &Pair<V>,
    delta: V,
    tolerance: Tolerance,
) -> f32 {
    let fitted = match tolerance {
        Tolerance::Skin(skin) if skin > 0.0 => skin * SKIN_SLOP,
        _ => shape_slop(body, target),
    };
    let largest = space::max(
        pair.target.largest(),
        pair.body.largest() + space::largest_abs(delta),
    );
    space::max(fitted, rounding_slop(largest))
}

/// A query's slop for a pair of shapes before its rounding floor: a
/// hundredth of the smaller shape's extent ([`EXTENT_SLOP`]), at most a
/// quarter of the smaller radius ([`RADIUS_SLOP`]); a shape with no extent
/// or no radius does not count. Infinite when neither shape has an extent,
/// a pair that never [`meets`]. The planar bodies take theirs from here.
pub(crate) fn shape_slop<V: Vector>(a: &Shape<V>, b: &Shape<V>) -> f32 {
    let mut slop = f32::INFINITY;
    for shape in [a, b] {
        let extent = extent(shape);
        if extent > 0.0 {
            slop = space::min(slop, extent * EXTENT_SLOP);
        }
    }
    for shape in [a, b] {
        let radius = shape.radius();
        if radius > 0.0 {
            slop = space::min(slop, radius * RADIUS_SLOP);
        }
    }
    slop
}

/// The least slop among coordinates up to `largest` ([`ROUNDING_SLOP`] of
/// it), so `f32` can land a contact within a quarter of the slop.
pub(crate) fn rounding_slop(largest: f32) -> f32 {
    ROUNDING_SLOP * largest
}

/// A contact at the start against the motion itself: what a pair with no
/// known way out answers, so the body does not move. `None` only for a
/// motion with no direction, which moves nothing anyway.
fn blocked<V: Vector>(delta: V) -> Option<Contact<V>> {
    space::direction(delta).map(|(along, _)| Contact {
        t: 0.0,
        normal: -along,
        faces: 0,
    })
}

/// A contact at `t` with `normal`, at `point` on the target: for a whole
/// box, the faces the point lies on ([`Contact::faces`]).
fn contact<V: Vector>(
    t: f32,
    normal: V,
    target: &Shape<V>,
    cut: Option<&Cut<V>>,
    rel: V,
    point: V,
) -> Contact<V> {
    let mut faces = 0;
    if let (Shape::Box { half }, None) = (target, cut) {
        for axis in 0..V::AXES {
            // The nearest point of a box's face is a weighted sum of its
            // corners there, which share the face's coordinate: it lies on
            // the face to within a few units in the last place.
            let tolerance = 16.0 * f32::EPSILON * (rel[axis].abs() + half[axis]);
            if point[axis] >= rel[axis] + half[axis] - tolerance {
                faces |= 1 << (2 * axis + 1);
            }
            if point[axis] <= rel[axis] - half[axis] + tolerance {
                faces |= 1 << (2 * axis);
            }
        }
    }
    Contact { t, normal, faces }
}

/// [`super::sweep`] for a kernel pair the body does not overlap.
///
/// A body already within the contact band ([`CONTACT_BAND`] slops) meets
/// the target at `t = 0` when it moves into the normal between them, and
/// not otherwise. Further away, the kernel casts the body grown by twice
/// the slop, so its target between the cores, `max(slop, radii - slop)`,
/// is the radii plus one slop: contact a slop apart, whatever the radii. A
/// query reports that fraction; the mover reports where the body would
/// touch, the fraction plus the slop (no more than the skin) over the
/// approach speed, so its skin back-off leaves the skin. That is the
/// distance along the normal at contact, so on a flat face it is exact and
/// on a corner it is before touching: the distance between convex shapes
/// along a straight motion is convex, so it never falls faster than it
/// does at contact.
///
/// A cast the kernel gives up on stops the body where it gave up, against
/// the last normal it found when that one is approached, or against the
/// motion. A graze, a hit whose normal is not approached, is no contact.
pub(super) fn sweep<V: Vector>(
    body: &Shape<V>,
    delta: V,
    target: &Shape<V>,
    cut: Option<&Cut<V>>,
    rel: V,
    tolerance: Tolerance,
) -> Option<Contact<V>> {
    if delta == V::ZERO || !space::is_finite(delta) || !meets(body, target) {
        return None;
    }
    let pair = Pair::new(body, target, cut, rel)?;
    let slop = slop(body, target, &pair, delta, tolerance);
    let (gap, normal, point) = match pair.start() {
        // The caller found no overlap; read the same answer as one.
        Start::Deep => return blocked(delta),
        Start::Apart { gap, normal, point } => (gap, normal, point),
    };
    if gap < CONTACT_BAND * slop {
        return (space::dot(delta, normal) < 0.0)
            .then(|| contact(0.0, normal, target, cut, rel, point));
    }
    let found = shape_cast(&pair.target, &pair.body.grown(2.0 * slop), delta, slop);
    respond(&found, delta, slop, tolerance, |t, normal, point| {
        contact(t, normal, target, cut, rel, point)
    })
}

/// The contact a kernel cast means, made by `contact` from a fraction, a
/// normal and the point on the target. Never a clear path when the kernel
/// gave up: see [`sweep`].
fn respond<V: Vector>(
    found: &Cast<V>,
    delta: V,
    slop: f32,
    tolerance: Tolerance,
    contact: impl Fn(f32, V, V) -> Contact<V>,
) -> Option<Contact<V>> {
    match found.state {
        CastState::Miss => None,
        CastState::Hit => {
            let approach = -space::dot(delta, found.normal);
            if approach.is_nan() || approach <= 0.0 {
                return None;
            }
            let t = match tolerance {
                Tolerance::Query => found.fraction,
                Tolerance::Skin(skin) => {
                    space::min(1.0, found.fraction + space::min(slop, skin) / approach)
                }
            };
            Some(contact(t, found.normal, found.point))
        }
        CastState::Failed => {
            let usable = convex::is_unit(found.normal) && space::dot(delta, found.normal) < 0.0;
            if usable {
                Some(Contact {
                    t: found.fraction,
                    normal: found.normal,
                    faces: 0,
                })
            } else {
                blocked(delta).map(|stop| Contact {
                    t: found.fraction,
                    ..stop
                })
            }
        }
        // The start was outside the band, so this is rounding at its edge:
        // as within it.
        CastState::InitialOverlap => {
            if convex::is_unit(found.normal) {
                (space::dot(delta, found.normal) < 0.0).then_some(Contact {
                    t: 0.0,
                    normal: found.normal,
                    faces: 0,
                })
            } else {
                blocked(delta)
            }
        }
    }
}

/// [`super::overlap`] for a kernel pair: the cores overlap, or the shapes
/// do by any amount. Touching is not overlap where the kernel can tell
/// (shapes with a radius); two radius-less shapes that touch exactly may
/// read either way.
pub(super) fn overlap<V: Vector>(
    body: &Shape<V>,
    target: &Shape<V>,
    cut: Option<&Cut<V>>,
    rel: V,
) -> bool {
    if !meets(body, target) {
        return false;
    }
    Pair::new(body, target, cut, rel).is_some_and(|pair| match pair.start() {
        Start::Deep => true,
        Start::Apart { gap, .. } => gap < 0.0,
    })
}

/// [`super::embedded`] for a kernel pair: a way out is the normal between
/// the cores when they are apart, or else the one found with the body
/// stepped back along its motion by the slop. Motion into it is a contact
/// at `t = 0`; with no way out known (a ball whose centre is inside the
/// target), every motion is, so the body is not moved deeper.
pub(super) fn embedded<V: Vector>(
    body: &Shape<V>,
    delta: V,
    target: &Shape<V>,
    cut: Option<&Cut<V>>,
    rel: V,
    tolerance: Tolerance,
) -> Option<Contact<V>> {
    if delta == V::ZERO || !space::is_finite(delta) || !meets(body, target) {
        return None;
    }
    let pair = Pair::new(body, target, cut, rel)?;
    let slop = slop(body, target, &pair, delta, tolerance);
    let start = match pair.start() {
        Start::Deep => pair.backed(delta, slop).map(|back| back.start()),
        apart @ Start::Apart { .. } => Some(apart),
    };
    match start {
        Some(Start::Apart { normal, point, .. }) => {
            (space::dot(delta, normal) < 0.0).then(|| contact(0.0, normal, target, cut, rel, point))
        }
        _ => blocked(delta),
    }
}

/// [`super::resting`] for a kernel pair: the body overlaps the target by no
/// more than `allowance` along the normal between the cores, or the slop
/// if that is more, and `blocks` accepts the normal or a face of a box the
/// contact lies on. When the cores overlap, the body is stepped back along
/// its motion by that allowance; if the cores are clear of each other
/// there, it was within it.
#[allow(clippy::too_many_arguments)]
pub(super) fn resting<V: Vector>(
    body: &Shape<V>,
    delta: V,
    target: &Shape<V>,
    cut: Option<&Cut<V>>,
    rel: V,
    allowance: V,
    tolerance: Tolerance,
    blocks: impl Fn(V) -> bool,
) -> Option<Contact<V>> {
    if !meets(body, target) {
        return None;
    }
    let pair = Pair::new(body, target, cut, rel)?;
    let slop = slop(body, target, &pair, delta, tolerance);
    let within = |normal: V| space::max(space::reach(allowance, normal), slop);
    let (gap, normal, point) = match pair.start() {
        Start::Apart { gap, normal, point } => (gap, normal, point),
        Start::Deep => {
            let (along, _) = space::direction(delta)?;
            let back = pair.backed(delta, within(along))?;
            match back.start() {
                Start::Apart { gap, normal, point } if gap >= 0.0 => (0.0, normal, point),
                _ => return None,
            }
        }
    };
    let found = contact(0.0, normal, target, cut, rel, point);
    (-gap <= within(normal) && found.meets(&blocks)).then_some(found)
}

/// [`super::way_out`] for a kernel pair: the normal between the cores and
/// the depth along it, when the shapes overlap and the cores do not.
pub(super) fn way_out<V: Vector>(
    body: &Shape<V>,
    target: &Shape<V>,
    cut: Option<&Cut<V>>,
    rel: V,
) -> Option<(V, f32)> {
    if !meets(body, target) {
        return None;
    }
    match Pair::new(body, target, cut, rel)?.start() {
        Start::Apart { gap, normal, .. } if gap < 0.0 => Some((normal, -gap)),
        _ => None,
    }
}

/// [`super::separation`] for a kernel pair: the gap between the surfaces
/// and the normal, or `(0, zero)` when they touch or overlap.
pub(super) fn separation<V: Vector>(
    body: &Shape<V>,
    target: &Shape<V>,
    cut: Option<&Cut<V>>,
    rel: V,
) -> (f32, V) {
    match Pair::new(body, target, cut, rel).map(|pair| pair.start()) {
        Some(Start::Apart { gap, normal, .. }) if gap > 0.0 => (gap, normal),
        _ => (0.0, V::ZERO),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn give_up(state: CastState, normal: Vec2) -> Cast<Vec2> {
        Cast {
            state,
            fraction: 0.4,
            normal,
            point: Vec2::ZERO,
            iterations: 20,
        }
    }

    fn made(t: f32, normal: Vec2, _: Vec2) -> Contact<Vec2> {
        Contact {
            t,
            normal,
            faces: 0,
        }
    }

    /// Defect: a cast the kernel gave up on read as a clear path, so a body
    /// tunnels. Oracle: the requirement (never a clear path). Moving by
    /// (3, -4), whose direction is (0.6, -0.8): a cast that failed at 0.4
    /// stops the body there, against the normal it found when the motion
    /// approaches it, and against the motion itself when that normal is
    /// zero or moving away from it; an initial overlap with no normal blocks
    /// all motion at once.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_cast_the_kernel_gave_up_on_is_never_a_clear_path() {
        let delta = Vec2::new(3.0, -4.0);
        let against = Vec2::new(-0.6, 0.8);
        let answer = |found: Cast<Vec2>| {
            let contact = respond(&found, delta, 0.01, Tolerance::Query, made);
            contact.map(|c| (c.t, c.normal))
        };
        assert_eq!(
            answer(give_up(CastState::Failed, Vec2::Y)),
            Some((0.4, Vec2::Y))
        );
        assert_eq!(
            answer(give_up(CastState::Failed, Vec2::ZERO)),
            Some((0.4, against))
        );
        assert_eq!(
            answer(give_up(CastState::Failed, Vec2::NEG_Y)),
            Some((0.4, against))
        );
        let overlap = give_up(CastState::InitialOverlap, Vec2::ZERO);
        assert_eq!(answer(overlap), Some((0.0, against)));
    }
}
