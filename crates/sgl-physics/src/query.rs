//! Questions a game asks a world: what a shape overlaps, what a moving shape
//! hits first, and what a ray hits first. The mover asks the same question
//! through [`first_contact`].

use core::cmp::Ordering;

use sgl_core::{CanonicalWrite, StateHasher};

use crate::config::MoverConfig;
use crate::shape::{Collider, ColliderKind, Cut, Shape, order_cuts};
use crate::slide::{back_off, lands};
use crate::space::{self, Vector};
use crate::sweep::{self, Contact, TIE_ORDER, Tolerance};
use crate::world::{Unanswered, World};

/// The first blocking contact along a motion.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Hit<V> {
    /// Fraction of the motion travelled before contact, in `[0, 1]`: `0`
    /// means touching at the start, `1` contact exactly at the end.
    pub t: f32,
    /// Unit normal of the face met, pointing from it toward the moving body.
    /// Axis-aligned between boxes; a ramp's plane gives its own normal; a
    /// round or convex shape's contact gives the normal between the shapes,
    /// which leans at an edge or a corner. A [`ray`] reports the surface's
    /// own normal where it crosses it: a triangle's plane, a polygon's
    /// edge, a ball's or a capsule's radius.
    pub normal: V,
    /// The id of the collider met.
    pub id: u64,
}

impl<V: Vector> Hit<V> {
    /// The largest postcard encoding: an `f32`, the normal and a `u64`
    /// varint (26 bytes in 3D, 22 in 2D).
    pub const MAX_POSTCARD_BYTES: usize = 4 + space::postcard_bytes::<V>() + 10;
}

impl<V: Vector> CanonicalWrite for Hit<V> {
    /// `t`'s bits, the normal's bits in axis order, then `id`.
    fn canonical_write(&self, out: &mut StateHasher) {
        out.u32(self.t.to_bits());
        space::write(out, self.normal);
        out.u64(self.id);
    }
}

/// The first collider `body` meets moving by `motion` from `position`, among
/// the solid colliders on a layer in `mask`.
///
/// Sensors are passed through. A one-way platform is met like any solid
/// box: a cast knows no up axis. A game whose casts should pass its
/// platforms puts them on their own layer and leaves it out of `mask`. A
/// soft collider is met as any solid, and every collider where it stands:
/// its velocity is not read.
///
/// A body touching a face, or inside it by no more than the rounding of the
/// collider's own coordinates (`4ε(|position| + half)` on that axis), meets
/// it at `t = 0` when it moves into it, so a projectile stepped to a hit and
/// cast again stays at the face. A collider the body overlaps more deeply at
/// the start is passed, so a cast from inside a box finds what lies beyond
/// it. Contacts at the same instant resolve to the lowest id, then the
/// lowest position; an exact tie between axes resolves y, then x, then z.
///
/// A ball, a capsule or a convex shape, as the body or the collider, is met
/// through the convex kernel to within a slop (a body with no extent, which
/// [`ray`] casts, meets most of them exactly instead): a hundredth of the
/// smaller shape's extent, at most a quarter of the smaller radius. `t` is where
/// the shapes come within one to one and a quarter slops of each other,
/// before they touch, and a body within the slop (overlap included) moving
/// in meets the collider at `t = 0` (`specs/physics.md`, 19).
///
/// `Ok(None)` when nothing is met, or `motion` is zero or not finite;
/// [`Unanswered`] when the world will not walk the motion's region.
pub fn cast<V: Vector, W: World<V> + ?Sized>(
    body: &Shape<V>,
    position: V,
    motion: V,
    world: &W,
    mask: u32,
) -> Result<Option<Hit<V>>, Unanswered> {
    if motion == V::ZERO || !space::is_finite(motion) {
        return Ok(None);
    }
    first_contact(body, position, V::ZERO, motion, world, mask, Rule::Cast)
}

/// Moves `body` from `position` by `delta` until the first collider on a
/// layer in `mask` blocks it, and stops a `skin` short of it along the path:
/// Godot's `move_and_collide`. Returns the translation to apply to
/// `position` and the contact, or the whole `delta` and `None` when nothing
/// is in the way. It does not slide: what the body does next (a bounce, a
/// break, a slide of its own) is the game's. For projectiles, pushed crates
/// and anything whose response is not a character's.
///
/// Colliders block as they do for [`slide`](crate::slide): sensors and soft
/// colliders ([`Push::Soft`](crate::Push::Soft)) never, a one-way platform
/// only motion into its floor face, and a collider the body starts inside
/// only motion deeper into it. Every collider is taken where it stands; its
/// velocity is not read. A body stopped against a
/// face and moved again along it is not blocked by that face. A non-finite
/// `delta`, position or body size moves nothing, and so does a move whose
/// end `f32` cannot hold (a finite position and motion whose sum is not
/// finite).
///
/// [`Unanswered`] when the world will not walk the motion's region: the
/// body then moves nothing.
pub fn move_and_collide<V: Vector, W: World<V> + ?Sized>(
    body: &Shape<V>,
    position: V,
    delta: V,
    world: &W,
    mask: u32,
    config: &MoverConfig<V>,
) -> Result<(V, Option<Hit<V>>), Unanswered> {
    if delta == V::ZERO || !lands(body, position, V::ZERO, delta) {
        return Ok((V::ZERO, None));
    }
    let rule = Rule::Move(config);
    Ok(
        match first_contact(body, position, V::ZERO, delta, world, mask, rule)? {
            Some(hit) => (back_off(delta, &hit, config.skin()), Some(hit)),
            None => (delta, None),
        },
    )
}

/// The first collider a point travelling by `motion` from `origin` meets: a
/// [`cast`] of a box with no extent. A ray starting on a face, or a rounding
/// step inside it, and moving into the collider meets it at `t = 0`; a ray
/// from deeper inside one passes it.
///
/// A ray is exact against a box, a cut box, a ball, a capsule, a 2D polygon
/// and a 3D triangle (a convex shape of three points, as a
/// [`TriangleSet`](crate::TriangleSet) or a
/// [`HeightField`](crate::HeightField) offers): `t` is where it crosses the
/// surface, to a few units in the last place of the coordinates involved,
/// and the normal is the surface's there (a triangle's own, turned against
/// the ray). A triangle is two-sided and has no inside: a ray starting on
/// it meets it at once. A ray touching a surface without approaching it (a
/// tangent, a slide along a face) meets nothing. A 3D convex hull of four or
/// more points is met through the kernel, a slop short, as [`cast`] meets
/// it (`specs/physics.md`, 15).
pub fn ray<V: Vector, W: World<V> + ?Sized>(
    origin: V,
    motion: V,
    world: &W,
    mask: u32,
) -> Result<Option<Hit<V>>, Unanswered> {
    cast(&Shape::Box { half: V::ZERO }, origin, motion, world, mask)
}

/// Every collider on a layer in `mask` that `body` at `position` overlaps,
/// sensors included: how a game finds what is in a trigger or a hit box.
/// Touching is not overlap, except that two radius-less shapes met through
/// the kernel (a box and a convex shape, say) that touch exactly may read
/// either way.
///
/// The list is in a fixed order (id, then position, then shape, kind,
/// layers, cut, velocity and push), never the order the world visited them
/// in, and lists each collider once however many times the world visited
/// it. Every collider is taken where it stands, soft ones included.
pub fn overlaps<V: Vector, W: World<V> + ?Sized>(
    body: &Shape<V>,
    position: V,
    world: &W,
    mask: u32,
) -> Result<Vec<Collider<V>>, Unanswered> {
    let half = body.half_extents();
    let (min, max) = (position - half, position + half);
    let pad = pad(min, max);
    let near = (min - pad * 0.5, max + pad * 0.5);
    let mut found = Vec::new();
    world.colliders_near(min - pad, max + pad, &mut |collider| {
        if collider.layers & mask != 0
            && collider.occupies()
            && (sweep::closed_form(body, &collider.shape)
                || reaches(near.0, near.1, V::ZERO, collider))
            && sweep::overlap(
                body,
                &collider.shape,
                collider.cut.as_ref(),
                collider.position - position,
            )
        {
            found.push(*collider);
        }
    })?;
    found.sort_by(Collider::order);
    found.dedup_by(|a, b| a.order(b).is_eq());
    Ok(found)
}

/// How far a shape is from a collider: what [`distance`] measures.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Separation<V> {
    /// The gap between their surfaces, in the game's units: zero when they
    /// touch or overlap.
    pub distance: f32,
    /// Unit normal from the collider toward the shape across the gap, or
    /// zero when they touch or overlap.
    pub normal: V,
}

impl<V: Vector> Separation<V> {
    /// The largest postcard encoding: an `f32` and the normal (16 bytes in
    /// 3D, 12 in 2D).
    pub const MAX_POSTCARD_BYTES: usize = 4 + space::postcard_bytes::<V>();
}

impl<V: Vector> CanonicalWrite for Separation<V> {
    /// `distance`'s bits, then the normal's bits in axis order.
    fn canonical_write(&self, out: &mut StateHasher) {
        out.u32(self.distance.to_bits());
        space::write(out, self.normal);
    }
}

/// How far `body` at `position` is from `collider`, whatever its kind or
/// layers: the gap between their surfaces and the direction across it. How
/// a game measures a hit box's reach, an explosion's falloff or a sensor's
/// nearness.
///
/// Two whole boxes are measured in closed form. Every other pair is
/// measured by the convex kernel (GJK) to within the rounding of `f32` at
/// the pair's size; a cut box is the solid part its plane leaves. Shapes that touch or
/// overlap are zero apart, with no normal.
///
/// `None` when the collider occupies nothing, or the body's shape or
/// position is not sound (a negative or non-finite size, a non-finite
/// position). The collider is taken where it stands: its velocity and push
/// are not read.
#[must_use]
pub fn distance<V: Vector>(
    body: &Shape<V>,
    position: V,
    collider: &Collider<V>,
) -> Option<Separation<V>> {
    if !collider.occupies() || !body.is_sound() || !space::is_finite(position) {
        return None;
    }
    let rel = collider.position - position;
    let (distance, normal) = sweep::separation(body, &collider.shape, collider.cut.as_ref(), rel);
    Some(Separation { distance, normal })
}

/// What a sweep treats as blocking.
#[derive(Clone, Copy)]
pub(crate) enum Rule<'a, V> {
    /// [`cast`]: every collider but sensors, one-way platforms whole; those
    /// the body starts inside by more than a rounding step are passed.
    Cast,
    /// [`slide`](crate::slide), [`snap_to_ground`](crate::snap_to_ground)
    /// and [`move_and_collide`]: an embedded body may not go deeper, and a
    /// one-way platform blocks only motion into its floor face.
    Move(&'a MoverConfig<V>),
}

/// The first contact of `body`, now at `origin + offset`, moving by `delta`,
/// under `rule`. Positions are taken relative to `origin` before anything
/// else, so the result's accuracy does not depend on how far `origin` is
/// from the world's origin.
pub(crate) fn first_contact<V: Vector, W: World<V> + ?Sized>(
    body: &Shape<V>,
    origin: V,
    offset: V,
    delta: V,
    world: &W,
    mask: u32,
    rule: Rule<'_, V>,
) -> Result<Option<Hit<V>>, Unanswered> {
    let half = body.half_extents();
    let at = origin + offset;
    let (start_min, start_max) = (at - half, at + half);
    let pad = pad(
        space::min_each(start_min, start_min + delta),
        space::max_each(start_max, start_max + delta),
    );
    let near = (start_min - pad * 0.5, start_max + pad * 0.5);
    let mut best: Option<Candidate<V>> = None;
    // The mover's sweeps pass a soft collider: it nudges a body after its
    // move instead of stopping it.
    let passes_soft = matches!(rule, Rule::Move(_));
    let mut consider = |collider: &Collider<V>| {
        if collider.layers & mask == 0 || !collider.kind.blocks() || !collider.occupies() {
            return;
        }
        if passes_soft && collider.soft().is_some() {
            return;
        }
        if !sweep::closed_form(body, &collider.shape) && !reaches(near.0, near.1, delta, collider) {
            return;
        }
        let rel = (collider.position - origin) - offset;
        let Some(contact) = contact(body, delta, collider, rel, rule) else {
            return;
        };
        let candidate = Candidate {
            contact,
            id: collider.id,
            position: collider.position,
            shape: collider.shape,
            cut: collider.cut,
        };
        if best.is_none_or(|best| candidate.order(&best).is_lt()) {
            best = Some(candidate);
        }
    };
    world.colliders_along(start_min - pad, start_max + pad, delta, &mut consider)?;
    Ok(best.map(|best| Hit {
        t: best.contact.t,
        normal: best.contact.normal,
        id: best.id,
    }))
}

/// The contact `body` moving by `delta` makes with `collider` at `rel`, as
/// `rule` reads it.
fn contact<V: Vector>(
    body: &Shape<V>,
    delta: V,
    collider: &Collider<V>,
    rel: V,
    rule: Rule<'_, V>,
) -> Option<Contact<V>> {
    let (target, cut) = (&collider.shape, collider.cut.as_ref());
    let config = match rule {
        Rule::Cast if !sweep::overlap(body, target, cut, rel) => {
            return sweep::sweep(body, delta, target, cut, rel, TIE_ORDER, Tolerance::Query);
        }
        // A body whose edge equals a face in world coordinates can lie a
        // rounding step inside it once taken relative to the body: that is
        // touching, and motion into the face meets it at once. Deeper, the
        // body started inside and the cast passes the collider. A kernel
        // pair allows its slop.
        Rule::Cast => {
            let into = |normal: V| space::dot(delta, normal) < 0.0;
            let allowance = collider.rounding();
            let tolerance = Tolerance::Query;
            return sweep::resting(
                body, delta, target, cut, rel, allowance, TIE_ORDER, tolerance, into,
            );
        }
        Rule::Move(config) => config,
    };
    let order = config.tie_order();
    let tolerance = Tolerance::Skin(config.skin());
    let one_way = collider.kind == ColliderKind::OneWay;
    // A one-way platform blocks motion into its floor face: the face a
    // contact's normal names, or, for a round or convex body meeting the
    // platform's edge, the top face whose edge it meets.
    let blocks_one_way = |normal: V| config.is_floor(normal) && space::dot(delta, normal) < 0.0;
    let contact = if !sweep::overlap(body, target, cut, rel) {
        sweep::sweep(body, delta, target, cut, rel, order, tolerance)
    } else if one_way {
        // Within `skin` (plus the rounding of the platform's own coordinates)
        // below the floor face, a body rests on the platform, as Godot's
        // one-way margin has it; deeper, it is passing through.
        let allowance = collider.rounding() + space::splat(config.skin());
        sweep::resting(
            body,
            delta,
            target,
            cut,
            rel,
            allowance,
            order,
            tolerance,
            blocks_one_way,
        )
    } else {
        sweep::embedded(body, delta, target, cut, rel, order, tolerance)
    }?;
    if one_way && !contact.meets(blocks_one_way) {
        return None;
    }
    Some(contact)
}

/// Whether `collider`'s box, grown by its rounding allowance, meets the box
/// `min..=max` swept by `delta`, closed: the colliders a moving query meets
/// through the kernel. A kernel pair makes contact a slop apart, so the
/// kernel could find one with a collider just beyond the region a world
/// offers; this keeps to colliders every world offers (its region is padded
/// by twice what the caller pads `min..=max` by), so the result never
/// depends on the world. A collider it leaves out the body never reaches.
pub(crate) fn reaches<V: Vector>(min: V, max: V, delta: V, collider: &Collider<V>) -> bool {
    let reach = collider.shape.half_extents() + collider.rounding();
    let (lo, hi) = (collider.position - reach, collider.position + reach);
    let (mut enter, mut leave) = (0.0f32, 1.0f32);
    for axis in 0..V::AXES {
        let d = delta[axis];
        if d == 0.0 {
            if max[axis] < lo[axis] || min[axis] > hi[axis] {
                return false;
            }
            continue;
        }
        let (a, b) = ((lo[axis] - max[axis]) / d, (hi[axis] - min[axis]) / d);
        let (entry, exit) = if d > 0.0 { (a, b) } else { (b, a) };
        enter = space::max(enter, entry);
        leave = space::min(leave, exit);
    }
    enter <= leave
}

/// A contact and the collider it was made with, ordered so the earliest
/// wins and ties never depend on the order a world visited colliders in.
#[derive(Clone, Copy)]
struct Candidate<V> {
    contact: Contact<V>,
    id: u64,
    position: V,
    shape: Shape<V>,
    cut: Option<Cut<V>>,
}

impl<V: Vector> Candidate<V> {
    /// Earliest first; at the same instant, the lowest id, then the lowest
    /// position component by component, then the normal, the shape and the
    /// cut, so the order is total.
    fn order(&self, other: &Self) -> Ordering {
        self.contact
            .t
            .total_cmp(&other.contact.t)
            .then(self.id.cmp(&other.id))
            .then_with(|| space::compare(self.position, other.position))
            .then_with(|| space::compare(self.contact.normal, other.contact.normal))
            .then_with(|| self.shape.order(&other.shape))
            .then_with(|| order_cuts(self.cut.as_ref(), other.cut.as_ref()))
    }
}

/// How far to grow the region `min..=max` on every axis, so a collider
/// whose face the body reaches exactly, computed relative to the body, is
/// never left out by a world reading the region in world coordinates. The
/// pad covers the rounding of the body's own coordinates (several units in
/// the last place of the region's largest); a world covers the collider's
/// ([`World`]). It adds candidates only, and the exact tests decide.
pub(crate) fn pad<V: Vector>(min: V, max: V) -> V {
    let largest = space::max(space::largest_abs(min), space::largest_abs(max));
    space::splat(largest * (8.0 * f32::EPSILON))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sgl_core::math::{Vec2, Vec3};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn block(id: u64, min: Vec3, max: Vec3) -> Collider<Vec3> {
        Collider::new(
            id,
            Shape::Box {
                half: (max - min) * 0.5,
            },
            (min + max) * 0.5,
        )
    }

    /// A ray along x from the origin meets the face of a box spanning x 4..6
    /// after 4 of its 10 units: t = 0.4, normal -x.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_meets_the_near_face_at_the_fraction_of_its_length() {
        let walls = [block(
            7,
            Vec3::new(4.0, -1.0, -1.0),
            Vec3::new(6.0, 1.0, 1.0),
        )];
        let hit = ray(Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), &walls[..], u32::MAX)
            .unwrap()
            .expect("hit");
        assert_eq!(
            hit,
            Hit {
                t: 0.4,
                normal: Vec3::NEG_X,
                id: 7
            }
        );
    }

    /// A diagonal ray from the origin by (8, 4, 2) reaches the plane x = 2 at
    /// t = 0.25, where it is at (2, 1, 0.5): inside the face of a slab
    /// spanning x 2..3, y and z -10..10. It meets that face, not the slab's
    /// top or side.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_diagonal_ray_meets_the_face_it_crosses() {
        let slab = [block(
            1,
            Vec3::new(2.0, -10.0, -10.0),
            Vec3::new(3.0, 10.0, 10.0),
        )];
        let hit = ray(Vec3::ZERO, Vec3::new(8.0, 4.0, 2.0), &slab[..], u32::MAX)
            .unwrap()
            .expect("hit");
        assert_eq!(
            hit,
            Hit {
                t: 0.25,
                normal: Vec3::NEG_X,
                id: 1
            }
        );
        // From y = 9, climbing a unit per unit across, the ray is at y = 11
        // when it reaches x = 2: over the slab's top at y = 10, a miss.
        let over = ray(
            Vec3::new(0.0, 9.0, 0.0),
            Vec3::new(8.0, 8.0, 0.0),
            &slab[..],
            u32::MAX,
        );
        assert_eq!(over, Ok(None));
    }

    /// A ray that starts inside a box passes it and meets the next one: the
    /// box from x 0..2 holds the origin, the next starts at x = 5, 5 of 10
    /// units along.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_from_inside_a_box_meets_what_lies_beyond() {
        let walls = [
            block(1, Vec3::new(-1.0, -1.0, -1.0), Vec3::new(2.0, 1.0, 1.0)),
            block(2, Vec3::new(5.0, -1.0, -1.0), Vec3::new(6.0, 1.0, 1.0)),
        ];
        let hit = ray(Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), &walls[..], u32::MAX)
            .unwrap()
            .expect("hit");
        assert_eq!((hit.t, hit.id), (0.5, 2));
    }

    /// A box body cast down onto a floor: its bottom starts at y = -0.5 and
    /// the floor's top is at y = -2, so it touches after 1.5 of 10 units,
    /// t = 0.15, on the floor's up face.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_cast_box_lands_where_its_bottom_meets_the_floor() {
        let floor = [block(
            3,
            Vec3::new(-5.0, -3.0, -5.0),
            Vec3::new(5.0, -2.0, 5.0),
        )];
        let body = Shape::Box {
            half: Vec3::splat(0.5),
        };
        let hit = cast(
            &body,
            Vec3::ZERO,
            Vec3::new(0.0, -10.0, 0.0),
            &floor[..],
            u32::MAX,
        )
        .unwrap()
        .expect("hit");
        assert_eq!(hit.id, 3);
        assert_eq!(hit.normal, Vec3::Y);
        assert!((hit.t - 0.15).abs() < 1e-6, "t was {}", hit.t);
    }

    /// Two floor tiles meet at x = 0 and a body lands across the seam: both
    /// are met at the same instant, and the lower id is reported whichever
    /// order the world lists them in.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_tie_at_one_instant_goes_to_the_lowest_id_in_any_order() {
        let left = block(9, Vec3::new(-2.0, -1.0, -1.0), Vec3::new(0.0, 0.0, 1.0));
        let right = block(4, Vec3::new(0.0, -1.0, -1.0), Vec3::new(2.0, 0.0, 1.0));
        let body = Shape::Box {
            half: Vec3::splat(0.5),
        };
        let down = Vec3::new(0.0, -2.0, 0.0);
        let start = Vec3::new(0.0, 1.0, 0.0);
        for tiles in [[left, right], [right, left]] {
            let hit = cast(&body, start, down, &tiles[..], u32::MAX)
                .unwrap()
                .expect("hit");
            assert_eq!((hit.t, hit.id), (0.25, 4));
        }
    }

    /// A layer mask leaves out what it should: a wall on layer 2 is invisible
    /// to a ray masking layer 1 and met by one masking layer 2 or every
    /// layer. Sensors are never met.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_layer_mask_and_the_kind_decide_what_a_ray_meets() {
        let wall = block(5, Vec3::new(4.0, -1.0, -1.0), Vec3::new(6.0, 1.0, 1.0)).with_layers(0b10);
        let motion = Vec3::new(10.0, 0.0, 0.0);
        let world = [wall];
        assert_eq!(ray(Vec3::ZERO, motion, &world[..], 0b01), Ok(None));
        assert_eq!(
            ray(Vec3::ZERO, motion, &world[..], 0b10)
                .unwrap()
                .map(|h| h.id),
            Some(5)
        );
        assert_eq!(
            ray(Vec3::ZERO, motion, &world[..], u32::MAX)
                .unwrap()
                .map(|h| h.id),
            Some(5)
        );
        let sensor = [wall.with_kind(ColliderKind::Sensor)];
        assert_eq!(ray(Vec3::ZERO, motion, &sensor[..], u32::MAX), Ok(None));
    }

    /// `overlaps` finds sensors and solids the body overlaps, on the masked
    /// layers, and not one it only touches, listed by id whatever the
    /// world's order.
    #[wasm_bindgen_test(unsupported = test)]
    fn overlaps_lists_what_the_body_is_inside_by_id() {
        let unit = Shape::Box {
            half: Vec2::splat(0.5),
        };
        let at = |id: u64, x: f32| Collider::new(id, unit, Vec2::new(x, 0.0));
        let world = [
            at(8, 0.5).with_kind(ColliderKind::Sensor),
            at(3, -0.5),
            at(1, 1.0),
            at(2, 0.25).with_layers(0b100),
        ];
        let found = overlaps(&unit, Vec2::ZERO, &world[..], 0b011).unwrap();
        let ids: Vec<u64> = found.iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            vec![3, 8],
            "touching (id 1) and masked (id 2) are left out"
        );
    }

    /// #427 in world coordinates: a body whose bottom edge equals the
    /// floor's top edge, bit for bit, is touching it, so a cast down meets
    /// the floor at once. Taken relative to the body the floor's top lies a
    /// rounding step above the body's bottom; without reading that as
    /// touching the cast passes the floor.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_cast_from_exactly_on_a_floor_at_rounding_coordinates_meets_it_at_once() {
        let floor = [Collider::new(
            1,
            Shape::Box {
                half: Vec2::new(50.0, 0.125),
            },
            Vec2::new(1.090_340_4, 4.337_375_6),
        )];
        let half = Vec2::new(0.4, 0.249_400_84);
        let position = Vec2::new(1.090_340_4, 4.711_776_3);
        assert_eq!(
            (position.y - half.y).to_bits(),
            (floor[0].position.y + 0.125f32).to_bits()
        );
        let hit = cast(
            &Shape::Box { half },
            position,
            Vec2::new(0.0, -3.0),
            &floor[..],
            u32::MAX,
        )
        .unwrap()
        .expect("touching a floor is a contact");
        assert_eq!((hit.t.to_bits(), hit.normal), (0.0f32.to_bits(), Vec2::Y));
    }

    /// Two colliders share an id and are met at the same instant: a body
    /// moving by (1, -1) reaches a floor's top (y = -1) and a wall's face
    /// (x = 1) both after half a unit. The one at the lower position wins,
    /// whatever the world's order, and its normal is reported. The floor at
    /// x = 0 wins over the wall at x = 1.5 (normal +y); mirrored, the wall at
    /// x = -1.5 wins over the floor (normal +x). Ordering by the normal
    /// instead would pick -x in the first case and +y in the second.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_tie_between_equal_ids_goes_to_the_lowest_position() {
        let at = |center: Vec2, half: Vec2| Collider::new(7, Shape::Box { half }, center);
        let floor = at(Vec2::new(0.0, -1.5), Vec2::new(5.0, 0.5));
        let body = Shape::Box {
            half: Vec2::splat(0.5),
        };
        for (side, normal) in [(1.0f32, Vec2::Y), (-1.0, Vec2::X)] {
            let wall = at(Vec2::new(1.5 * side, 2.0), Vec2::new(0.5, 2.5));
            let motion = Vec2::new(side, -1.0);
            for world in [[floor, wall], [wall, floor]] {
                let hit = cast(&body, Vec2::ZERO, motion, &world[..], u32::MAX)
                    .unwrap()
                    .expect("hit");
                assert_eq!((hit.t, hit.normal, hit.id), (0.5, normal, 7));
            }
        }
    }

    /// A world may visit a collider more than once (the same crate in both
    /// halves of a pair): `overlaps` still lists it once, and a cast meets
    /// it as it would once.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_collider_visited_twice_is_listed_once() {
        let unit = Shape::Box {
            half: Vec2::splat(0.5),
        };
        let crates = [Collider::new(3, unit, Vec2::new(0.5, 0.0))];
        let twice = (&crates[..], &crates[..]);
        let found = overlaps(&unit, Vec2::ZERO, &twice, u32::MAX).unwrap();
        assert_eq!(found, crates.to_vec());
    }

    /// A box with a negative half-extent, or a position or half-extent that
    /// is not finite, occupies nothing: a slice, a set and a cell world all
    /// let a cast pass it and leave it out of `overlaps`, while a sound box
    /// beside it (id 9) is still met.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_collider_that_occupies_nothing_is_met_by_no_world() {
        use crate::{CellColliders, CellWorld, ColliderSet};
        use sgl_core::math::IVec2;

        let body = Shape::Box {
            half: Vec2::splat(0.25),
        };
        let sound = Collider::new(
            9,
            Shape::Box {
                half: Vec2::splat(0.5),
            },
            Vec2::new(6.5, 0.5),
        );
        for broken in [
            Collider::new(
                1,
                Shape::Box {
                    half: Vec2::new(-0.5, 0.5),
                },
                Vec2::new(2.5, 0.5),
            ),
            Collider::new(
                1,
                Shape::Box {
                    half: Vec2::new(0.5, f32::NAN),
                },
                Vec2::new(2.5, 0.5),
            ),
            Collider::new(
                1,
                Shape::Box {
                    half: Vec2::new(0.5, f32::INFINITY),
                },
                Vec2::new(2.5, 0.5),
            ),
            Collider::new(
                1,
                Shape::Box {
                    half: Vec2::splat(0.5),
                },
                Vec2::new(f32::NAN, 0.5),
            ),
        ] {
            let slice = [broken, sound];
            let mut set = ColliderSet::new(1.0);
            set.insert(broken);
            set.insert(sound);
            // The cell world holds both in cell-local units: (2, 0) the
            // broken box, (6, 0) the sound one.
            let local = |c: Collider<Vec2>, x: f32| Collider {
                position: Vec2::new(c.position.x - x, c.position.y),
                ..c
            };
            let cells = CellWorld::new(1.0, |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
                if cell == IVec2::new(2, 0) {
                    out.push(local(broken, 2.0));
                } else if cell == IVec2::new(6, 0) {
                    out.push(local(sound, 6.0));
                }
            });
            let slice = &slice[..];
            let worlds: [&dyn World<Vec2>; 3] = [&slice, &set, &cells];
            for world in worlds {
                let start = Vec2::new(0.5, 0.5);
                let hit = cast(&body, start, Vec2::new(8.0, 0.0), world, u32::MAX)
                    .unwrap()
                    .expect("the sound box is met");
                assert_eq!(hit.id, 9, "{broken:?}");
                let inside = overlaps(&body, Vec2::new(2.5, 0.5), world, u32::MAX).unwrap();
                assert!(inside.is_empty(), "{broken:?}: {inside:?}");
            }
        }
    }
}
