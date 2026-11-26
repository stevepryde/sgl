//! What a ray is over: every collider whose closed solid a ray touches, and
//! the stretch of the ray inside each. Where [`ray`](crate::ray) asks what
//! stops a point, this asks what a pointer or a crosshair selects.

use sgl_core::{CanonicalWrite, StateHasher};

use crate::query::pad;
use crate::shape::Collider;
use crate::space::{self, Vector};
use crate::sweep;
use crate::world::{Unanswered, World};

/// The stretch of a ray that lies in one collider's closed solid: its
/// surface is part of it.
///
/// `enter` and `exit` are fractions of the ray's motion, as [`Hit::t`] is:
/// the ray is in the solid from `origin + motion * enter` to
/// `origin + motion * exit`. A ray that only touches the solid (a tangent
/// to a ball, a corner of a box) has `enter == exit`.
///
/// [`Hit::t`]: crate::Hit::t
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RaySpan<V> {
    /// Where the ray is first in the solid, in `[0, 1]`: `0` when it starts
    /// in it.
    pub enter: f32,
    /// Where the ray is last in the solid, in `[enter, 1]`: `1` when it
    /// ends in it.
    pub exit: f32,
    /// Unit normal of the surface the ray crosses at `enter`, pointing out
    /// of the solid. `None` when the ray starts in the solid without
    /// entering it there: inside it, or on its surface and leaving.
    pub enter_normal: Option<V>,
    /// Unit normal of the surface the ray crosses at `exit`, pointing out
    /// of the solid. `None` when the ray ends in the solid.
    pub exit_normal: Option<V>,
    /// The id of the collider.
    pub id: u64,
}

impl<V: Vector> RaySpan<V> {
    /// The largest postcard encoding: two `f32`s, two optional normals and
    /// a `u64` varint (44 bytes in 3D, 36 in 2D).
    pub const MAX_POSTCARD_BYTES: usize = 8 + 2 * (1 + space::postcard_bytes::<V>()) + 10;
}

impl<V: Vector> CanonicalWrite for RaySpan<V> {
    /// `enter`'s and `exit`'s bits, then for each normal a bool tag
    /// followed, when present, by its bits in axis order, then `id`.
    fn canonical_write(&self, out: &mut StateHasher) {
        out.u32(self.enter.to_bits());
        out.u32(self.exit.to_bits());
        for normal in [self.enter_normal, self.exit_normal] {
            out.bool(normal.is_some());
            if let Some(normal) = normal {
                space::write(out, normal);
            }
        }
        out.u64(self.id);
    }
}

/// Every collider on a layer in `mask` whose closed solid a point travelling
/// by `motion` from `origin` touches, each with the stretch of the ray in
/// it, the nearest `enter` first: what a pointer, a crosshair or an aimed
/// tool is over.
///
/// A solid's surface is part of it, and nothing is grown to make that so:
/// each decision is one comparison of the values [`ray`](crate::ray)
/// computes. A ray lying in a box's face or along its edge, one touching a
/// corner, a tangent to a ball and a ray that starts inside a collider are
/// all listed, where `ray`, which asks for a contact, meets none of them. A
/// ray beside a solid is not listed, however near. A start or a tangent
/// within the rounding of the coordinates of the surface may read either
/// way.
///
/// A ray that starts in a solid has `enter == 0.0` and no `enter_normal`;
/// its `exit` is where it leaves. Whether that collider is the pick, or the
/// next one is, is the game's rule, as is which of two colliders sharing a
/// face owns a ray lying in it: both are listed.
///
/// Colliders of every kind are listed, sensors included, as
/// [`overlaps`](crate::overlaps) lists them: a game picks among them by
/// `mask`. Spans at the same `enter` are in a fixed order (id, then
/// position, then the collider's other fields), never the order the world
/// visited them in, and a collider the world visited twice is listed once.
/// A box's normals are axis-aligned, a tie between axes resolving y, then
/// x, then z; a ramp's plane gives its own normal, when the ray crosses it
/// strictly after every axis coming in, or strictly before going out.
///
/// It reads a box, whole or cut, a ball, a capsule and a 2D convex polygon,
/// each in closed form. A 3D convex shape (a triangle of a
/// [`TriangleSet`](crate::TriangleSet) or a
/// [`HeightField`](crate::HeightField), a hull) and a ball or a capsule of
/// no radius are never listed (`specs/physics.md`, 15).
///
/// An empty list when `motion` is zero or not finite, or `origin` is not
/// finite; [`Unanswered`] when the world will not walk the ray's region.
///
/// ```
/// use sgl_core::math::Vec3;
/// use sgl_physics::{ALL_LAYERS, Collider, Shape, ray, ray_spans};
///
/// // A voxel filling the cell from (0, 0, 0) to (1, 1, 1).
/// let cell = [Collider::new(7, Shape::Box { half: Vec3::splat(0.5) }, Vec3::splat(0.5))];
/// // A ray along x in the plane of its z = 0 face is over it from x = 0,
/// // two units along, to x = 1.
/// let (eye, reach) = (Vec3::new(-2.0, 0.5, 0.0), Vec3::X * 4.0);
/// let over = ray_spans(eye, reach, &cell[..], ALL_LAYERS).unwrap();
/// assert_eq!((over[0].id, over[0].enter, over[0].exit), (7, 0.5, 0.75));
/// assert_eq!(over[0].enter_normal, Some(Vec3::NEG_X));
/// // It slides along that face, so it makes no contact.
/// assert_eq!(ray(eye, reach, &cell[..], ALL_LAYERS), Ok(None));
/// ```
pub fn ray_spans<V: Vector, W: World<V> + ?Sized>(
    origin: V,
    motion: V,
    world: &W,
    mask: u32,
) -> Result<Vec<RaySpan<V>>, Unanswered> {
    if motion == V::ZERO || !space::is_finite(motion) || !space::is_finite(origin) {
        return Ok(Vec::new());
    }
    let end = origin + motion;
    let pad = pad(space::min_each(origin, end), space::max_each(origin, end));
    let mut found: Vec<(RaySpan<V>, Collider<V>)> = Vec::new();
    world.colliders_along(origin - pad, origin + pad, motion, &mut |collider| {
        if collider.layers & mask == 0 || !collider.occupies() {
            return;
        }
        let rel = collider.position - origin;
        if let Some(span) = sweep::span(&collider.shape, collider.cut.as_ref(), motion, rel) {
            let span = RaySpan {
                enter: span.enter,
                exit: span.exit,
                enter_normal: span.enter_normal,
                exit_normal: span.exit_normal,
                id: collider.id,
            };
            found.push((span, *collider));
        }
    })?;
    found.sort_by(|a, b| {
        a.0.enter
            .total_cmp(&b.0.enter)
            .then_with(|| a.1.order(&b.1))
    });
    found.dedup_by(|a, b| a.1.order(&b.1).is_eq());
    Ok(found.into_iter().map(|(span, _)| span).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::{ALL_LAYERS, ColliderKind, Convex, Cut, Shape};
    use crate::{CellColliders, CellWorld, ColliderSet};
    use sgl_core::math::{IVec3, Vec2, Vec3};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// How far inside a solid a point is: positive inside, negative outside.
    type Depth<'a> = &'a dyn Fn(Vec3) -> f32;

    /// A span as `(enter, exit, enter_normal, exit_normal)`.
    type Stretch<V> = (f32, f32, Option<V>, Option<V>);

    /// The spans of a ray among `world`, without their ids.
    fn spans<V: Vector>(from: V, by: V, world: &[Collider<V>]) -> Vec<Stretch<V>> {
        ray_spans(from, by, world, ALL_LAYERS)
            .unwrap()
            .into_iter()
            .map(|span| (span.enter, span.exit, span.enter_normal, span.exit_normal))
            .collect()
    }

    /// The one span of a ray among `world`.
    fn span<V: Vector>(from: V, by: V, world: &[Collider<V>]) -> Stretch<V> {
        let found = spans(from, by, world);
        assert_eq!(found.len(), 1, "from {from:?} by {by:?}: {found:?}");
        found[0]
    }

    /// Whether `got` is `want` to four units in the last place: for values
    /// the test's own arithmetic rounds (a normal of 1 / sqrt(2)).
    fn close<V: Vector>(got: Stretch<V>, want: Stretch<V>) -> bool {
        let near = |a: f32, b: f32| (a - b).abs() <= 4.0 * f32::EPSILON;
        let same = |a: Option<V>, b: Option<V>| match (a, b) {
            (Some(a), Some(b)) => (0..V::AXES).all(|axis| near(a[axis], b[axis])),
            (None, None) => true,
            _ => false,
        };
        near(got.0, want.0) && near(got.1, want.1) && same(got.2, want.2) && same(got.3, want.3)
    }

    /// Asserts that 33 points along a ray agree with its spans among
    /// `world`: one more than a thousandth inside the solid, by `depth`
    /// (positive inside, negative outside), is in a span, and one more than
    /// a thousandth outside is in none.
    fn sampled<V: Vector>(from: V, motion: V, world: &[Collider<V>], depth: &dyn Fn(V) -> f32) {
        const MARGIN: f32 = 1e-3;
        let found = spans(from, motion, world);
        for step in 0u8..=32 {
            let t = f32::from(step) / 32.0;
            let inside = depth(from + motion * t);
            let listed = found.iter().any(|span| span.0 <= t && t <= span.1);
            let agrees = if listed {
                inside >= -MARGIN
            } else {
                inside <= MARGIN
            };
            assert!(
                agrees,
                "{world:?} from {from:?} by {motion:?} at {t}: {inside} deep, {found:?}"
            );
        }
    }

    /// The voxel filling the cell from (0, 0, 0) to (1, 1, 1).
    fn cell() -> [Collider<Vec3>; 1] {
        let half = Vec3::splat(0.5);
        [Collider::new(1, Shape::Box { half }, half)]
    }

    /// Defect (#930): a ray lying in a box's face, along its edge or
    /// through its corner is not over the box, or one beside it is. Oracle:
    /// the closed cube from 0 to 1. A ray from (-2, 0.5, 0) along x by 4
    /// lies in its face z = 0: over it from x = 0, 2 of 4 along, to x = 1,
    /// 3 of 4, coming in by the -x face and out by the +x one. The same
    /// from (-2, 1, 0), along the edge the faces y = 1 and z = 0 share. One
    /// a millionth outside the face, at z = -1e-6, is over nothing. From
    /// (-1, 0, 0.5) by (2, 2, 0) a ray meets only the cube's edge at
    /// (0, 1): x lets it in half way as y lets it out.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_lying_in_a_face_is_over_the_box() {
        let world = cell();
        let along = Vec3::X * 4.0;
        let through = (0.5, 0.75, Some(Vec3::NEG_X), Some(Vec3::X));
        assert_eq!(span(Vec3::new(-2.0, 0.5, 0.0), along, &world), through);
        assert_eq!(span(Vec3::new(-2.0, 1.0, 0.0), along, &world), through);
        assert_eq!(spans(Vec3::new(-2.0, 0.5, -1e-6), along, &world), vec![]);
        assert_eq!(
            span(Vec3::new(-1.0, 0.0, 0.5), Vec3::new(2.0, 2.0, 0.0), &world),
            (0.5, 0.5, Some(Vec3::NEG_X), Some(Vec3::Y))
        );
    }

    /// Defect: a ray that starts or ends inside a box is not listed, or is
    /// given a surface it never crossed; one that stops short of the box or
    /// starts past it is listed. Oracle: the cube from 0 to 1. From its
    /// centre along x by 2 a ray leaves by x = 1 after 0.5 of 2, entering
    /// nothing. From (-1, 0.5, 0.5) by 1.5 it comes in at x = 0, 1 of 1.5
    /// along, and ends inside. From (1, 0.5, 0.5), on a face, moving out it
    /// is in the cube at its start alone; moving in, from the start to x =
    /// 0. By 0.5 from x = -1 it stops short; from x = 1.5 moving away it
    /// starts past it.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_inside_a_box_has_no_surface_where_it_starts_or_ends() {
        let world = cell();
        let at = |x: f32| Vec3::new(x, 0.5, 0.5);
        assert_eq!(
            span(at(0.5), Vec3::X * 2.0, &world),
            (0.0, 0.25, None, Some(Vec3::X))
        );
        assert_eq!(
            span(at(-1.0), Vec3::X * 1.5, &world),
            (1.0 / 1.5, 1.0, Some(Vec3::NEG_X), None)
        );
        assert_eq!(
            span(at(1.0), Vec3::X * 2.0, &world),
            (0.0, 0.0, None, Some(Vec3::X))
        );
        assert_eq!(
            span(at(1.0), Vec3::NEG_X * 2.0, &world),
            (0.0, 0.5, Some(Vec3::X), Some(Vec3::NEG_X))
        );
        assert_eq!(spans(at(-1.0), Vec3::X * 0.5, &world), vec![]);
        assert_eq!(spans(at(1.5), Vec3::X * 2.0, &world), vec![]);
    }

    /// Defect: a ray is over the part of a ramp its cut removed, enters a
    /// ramp by its slope with a face's normal, or a ray lying in the slope
    /// is not over the ramp. Oracle: the cube from 0 to 1 cut to the wedge
    /// y <= x, its slope facing (-1, 1, 0) / sqrt(2). Down the line x =
    /// 0.25 from y = 2 by 4 a ray comes in by the slope at y = 0.25, 1.75
    /// of 4 along, and out by the floor at y = 0, half way. From (-0.5,
    /// -0.5, 0.5) by (2, 2, 0) it lies in the slope's plane: over the ramp
    /// from (0, 0) to (1, 1), a quarter to three quarters, by the cube's
    /// faces. A thousandth above the slope it is over nothing, and so is
    /// one through the removed half, along y = 0.75 at x = 0.25.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_is_over_a_ramp_by_its_slope() {
        let half = Vec3::splat(0.5);
        let wedge = Collider::new(1, Shape::Box { half }, half)
            .with_cut(Cut::new(Vec3::new(-1.0, 1.0, 0.0), 0.0));
        let world = [wedge];
        let slope = Vec3::new(-1.0, 1.0, 0.0) * core::f32::consts::FRAC_1_SQRT_2;
        let down = span(Vec3::new(0.25, 2.0, 0.5), Vec3::NEG_Y * 4.0, &world);
        assert!(
            close(down, (0.4375, 0.5, Some(slope), Some(Vec3::NEG_Y))),
            "{down:?}"
        );
        let diagonal = Vec3::new(2.0, 2.0, 0.0);
        assert_eq!(
            span(Vec3::new(-0.5, -0.5, 0.5), diagonal, &world),
            (0.25, 0.75, Some(Vec3::NEG_Y), Some(Vec3::Y))
        );
        assert_eq!(
            spans(Vec3::new(-0.5, -0.499, 0.5), diagonal, &world),
            vec![]
        );
        assert_eq!(
            spans(Vec3::new(0.25, 0.75, -1.0), Vec3::Z * 3.0, &world),
            vec![]
        );
    }

    /// Defect (#930): a ray tangent to a ball is over nothing, a near miss
    /// is over it, or a ray from inside it has no exit. Oracle: the 3-4-5
    /// triangle. A ball of radius 5 at (100, 50, -20); a ray along x from x
    /// = 90 by 20. At y = 55 it is tangent at (100, 55), half way, the
    /// normal up, in and out at once. At y = 55.001 it misses. At y = 53 it
    /// crosses where x = 100 ± 4: 6 and 14 of 20, normals (∓4, 3, 0) / 5.
    /// From (99, 50, -20), inside, it leaves at x = 105 after 6 of 20, by
    /// the +x pole. From (105, 50, -20), on the surface, moving out it is
    /// in the ball at its start alone. From x = 106 moving away, nothing.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_is_over_a_ball_at_a_tangent_and_from_inside() {
        let world = [Collider::new(
            1,
            Shape::Ball { radius: 5.0 },
            Vec3::new(100.0, 50.0, -20.0),
        )];
        let at = |x: f32, y: f32| Vec3::new(x, y, -20.0);
        let along = Vec3::X * 20.0;
        assert_eq!(
            span(at(90.0, 55.0), along, &world),
            (0.5, 0.5, Some(Vec3::Y), Some(Vec3::Y))
        );
        assert_eq!(spans(at(90.0, 55.001), along, &world), vec![]);
        let v = Vec3::new;
        assert_eq!(
            span(at(90.0, 53.0), along, &world),
            (0.3, 0.7, Some(v(-0.8, 0.6, 0.0)), Some(v(0.8, 0.6, 0.0)))
        );
        assert_eq!(
            span(at(99.0, 50.0), along, &world),
            (0.0, 0.3, None, Some(Vec3::X))
        );
        assert_eq!(
            span(at(105.0, 50.0), along, &world),
            (0.0, 0.0, None, Some(Vec3::X))
        );
        assert_eq!(spans(at(106.0, 50.0), along, &world), vec![]);
    }

    /// Defect: a ray is over a capsule's cylinder beyond its ends, misses
    /// its caps, or one running along its side or tangent to it is over
    /// nothing. Oracle: hand-worked geometry. A capsule of half-axis (0, 2,
    /// 0) and radius 1.25 at (10, 0, 0): its side is x = 10 ± 1.25 and its
    /// caps are about (10, ±2, 0). Along x at y = 0.5 from x = 5 by 10 a ray
    /// crosses the side at x = 8.75 and 11.25: 0.375 and 0.625. At y =
    /// 2.75, 0.75 above the top cap's centre, it crosses the cap where x =
    /// 10 ± 1: 0.4 and 0.6, normals (∓1, 0.75, 0) / 1.25. Up the line x =
    /// 8.75 from y = -10 by 20 it runs along the side from y = -2 to y = 2,
    /// tangent to both caps: 0.4 to 0.6, the side's normal at both ends.
    /// Along z at x = 8.75 it is tangent to the side, half way. From (9.5,
    /// 0.3, 0), inside, it leaves by the side at x = 11.25, 1.75 of 10.
    /// Along x at y = 3.5, above the cap's top at 3.25, nothing. The same
    /// cap in 2D.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_is_over_a_capsule_by_its_side_and_its_caps() {
        let capsule = Shape::Capsule {
            half_axis: Vec3::new(0.0, 2.0, 0.0),
            radius: 1.25,
        };
        let world = [Collider::new(1, capsule, Vec3::new(10.0, 0.0, 0.0))];
        let v = Vec3::new;
        let along = Vec3::X * 10.0;
        assert_eq!(
            span(v(5.0, 0.5, 0.0), along, &world),
            (0.375, 0.625, Some(Vec3::NEG_X), Some(Vec3::X))
        );
        assert_eq!(
            span(v(5.0, 2.75, 0.0), along, &world),
            (0.4, 0.6, Some(v(-0.8, 0.6, 0.0)), Some(v(0.8, 0.6, 0.0)))
        );
        assert_eq!(
            span(v(8.75, -10.0, 0.0), Vec3::Y * 20.0, &world),
            (0.4, 0.6, Some(Vec3::NEG_X), Some(Vec3::NEG_X))
        );
        assert_eq!(
            span(v(8.75, 0.5, -5.0), Vec3::Z * 10.0, &world),
            (0.5, 0.5, Some(Vec3::NEG_X), Some(Vec3::NEG_X))
        );
        assert_eq!(
            span(v(9.5, 0.3, 0.0), along, &world),
            (0.0, 0.175, None, Some(Vec3::X))
        );
        assert_eq!(spans(v(5.0, 3.5, 0.0), along, &world), vec![]);

        let capsule = Shape::Capsule {
            half_axis: Vec2::new(0.0, 2.0),
            radius: 1.25,
        };
        let world = [Collider::new(1, capsule, Vec2::new(10.0, 0.0))];
        assert_eq!(
            span(Vec2::new(5.0, 2.75), Vec2::X * 10.0, &world),
            (
                0.4,
                0.6,
                Some(Vec2::new(-0.8, 0.6)),
                Some(Vec2::new(0.8, 0.6))
            )
        );
    }

    /// Defect: a ray running along a polygon's edge is over nothing, one
    /// beside the edge is over it, or a ray from inside has no exit.
    /// Oracle: hand-worked geometry. The triangle (-2, -2), (2, -2), (-2,
    /// 2) about (20, 10): its long edge is x + y = 0 about the centre,
    /// facing (1, 1) / sqrt(2). Up the left edge's line x = -2 from y = -5
    /// by 10 a ray runs along it from the corner at y = -2 to the corner at
    /// y = 2: 0.3 to 0.7, in by the bottom and out by the long edge. A
    /// thousandth left of it, nothing. From (-0.5, -0.5), inside, up by 4
    /// it leaves by the long edge at y = 0.5, a quarter along.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_is_over_a_polygon_along_its_edge() {
        let points = [
            Vec2::new(-2.0, -2.0),
            Vec2::new(2.0, -2.0),
            Vec2::new(-2.0, 2.0),
        ];
        let centre = Vec2::new(20.0, 10.0);
        let world = [Collider::new(
            1,
            Shape::Convex(Convex::polygon(&points, 1.0, 0.0)),
            centre,
        )];
        let at = |x: f32, y: f32| centre + Vec2::new(x, y);
        let diagonal = Vec2::splat(core::f32::consts::FRAC_1_SQRT_2);
        let edge = span(at(-2.0, -5.0), Vec2::Y * 10.0, &world);
        assert!(
            close(edge, (0.3, 0.7, Some(Vec2::NEG_Y), Some(diagonal))),
            "{edge:?}"
        );
        assert_eq!(spans(at(-2.001, -5.0), Vec2::Y * 10.0, &world), vec![]);
        let inside = span(at(-0.5, -0.5), Vec2::Y * 4.0, &world);
        assert!(
            close(inside, (0.0, 0.25, None, Some(diagonal))),
            "{inside:?}"
        );
    }

    /// Defect: a span's ends are not where the ray crosses the surface: a
    /// wrong root, a clip on the wrong side of a plane, a capsule's cap
    /// taken for its side. Oracle: each solid's own definition, written
    /// here apart from the kernels (the points of a box under a plane,
    /// within a radius of a centre or of a segment, inside every edge of a
    /// polygon), asked of 33 points along each of 500 seeded rays, 100 at
    /// each of five solids: a point a thousandth inside the solid is in the
    /// ray's span, and one a thousandth outside it is not.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_span_holds_the_points_of_the_ray_inside_the_solid() {
        let mut seed = 0x0930_5ba5_u64;
        let mut draw = |low: f32, high: f32| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            #[allow(clippy::cast_precision_loss)]
            let unit = (seed >> 40) as f32 / 16_777_216.0;
            low + (high - low) * unit
        };
        // How far inside each solid a point is: positive inside, negative
        // outside, never more than the true distance to the surface.
        let centre = Vec3::new(3.1, -2.7, 4.3);
        let half = Vec3::new(1.3, 0.7, 1.1);
        let slope = Vec3::new(0.6, 0.8, 0.0);
        let boxed = move |p: Vec3| {
            let q = (p - centre).abs();
            (half - q).min_element()
        };
        let ramp = move |p: Vec3| boxed(p).min(0.2 - slope.dot(p - centre));
        let ball = |p: Vec3| 1.7 - (p - centre).length();
        let half_axis = Vec3::new(0.3, 1.1, -0.2);
        let pill = move |p: Vec3| {
            let along = (p - centre).dot(half_axis) / half_axis.length_squared();
            0.9 - (p - centre - half_axis * along.clamp(-1.0, 1.0)).length()
        };
        let block = Shape::Box { half };
        let solids: [(Collider<Vec3>, Depth<'_>); 4] = [
            (Collider::new(1, block, centre), &boxed),
            (
                Collider::new(1, block, centre).with_cut(Cut::new(slope, 0.2)),
                &ramp,
            ),
            (Collider::new(1, Shape::Ball { radius: 1.7 }, centre), &ball),
            (
                Collider::new(
                    1,
                    Shape::Capsule {
                        half_axis,
                        radius: 0.9,
                    },
                    centre,
                ),
                &pill,
            ),
        ];
        let corners = [
            Vec2::new(-1.3, -0.9),
            Vec2::new(1.7, -1.1),
            Vec2::new(0.9, 1.3),
            Vec2::new(-1.1, 0.7),
        ];
        let flat = Vec2::new(-7.3, 3.9);
        let polygon = [Collider::new(
            1,
            Shape::Convex(Convex::polygon(&corners, 1.0, 0.0)),
            flat,
        )];
        let within = |p: Vec2| {
            (0..4).fold(f32::INFINITY, |least, i| {
                let (a, b) = (corners[i], corners[(i + 1) % 4]);
                let outward = Vec2::new(b.y - a.y, a.x - b.x).normalize();
                least.min(outward.dot(a - (p - flat)))
            })
        };
        for _ in 0..100 {
            let offset = Vec3::new(draw(-3.0, 3.0), draw(-3.0, 3.0), draw(-3.0, 3.0));
            let motion = Vec3::new(draw(-6.0, 6.0), draw(-6.0, 6.0), draw(-6.0, 6.0));
            let from = centre + offset;
            for (collider, depth) in &solids {
                sampled(from, motion, &[*collider], depth);
            }
            sampled(
                flat + offset.truncate(),
                motion.truncate(),
                &polygon,
                &within,
            );
        }
    }

    /// Defect: spans come in the order a world visited their colliders in,
    /// a collider visited twice is listed twice, or a mask or a kind is
    /// read as `ray` reads it. Oracle: three unit boxes centred on the x
    /// axis at 2 (id 9), 5 (id 4) and 5 again (id 3, a sensor on layer 2),
    /// crossed by a ray from the origin along x by 10: the box at 2 is
    /// entered at 1.5 of 10, the two at 5 at 4.5, the lower id first,
    /// whichever order the slice has them in and though a pair of the same
    /// slice visits each twice. Masking layer 2 out leaves the sensor out;
    /// masking only layer 2 leaves it alone.
    #[wasm_bindgen_test(unsupported = test)]
    fn spans_are_listed_once_each_nearest_first_then_by_id() {
        let unit = Shape::Box {
            half: Vec3::splat(0.5),
        };
        let at = |id: u64, x: f32| Collider::new(id, unit, Vec3::new(x, 0.0, 0.0));
        let sensor = at(3, 5.0).with_kind(ColliderKind::Sensor).with_layers(0b10);
        let (near, far) = (at(9, 2.0).with_layers(0b01), at(4, 5.0).with_layers(0b01));
        let ids = |world: &dyn World<Vec3>, mask: u32| -> Vec<(u64, f32)> {
            ray_spans(Vec3::ZERO, Vec3::X * 10.0, world, mask)
                .unwrap()
                .into_iter()
                .map(|span| (span.id, span.enter))
                .collect()
        };
        for world in [[near, far, sensor], [sensor, far, near]] {
            let all = vec![(9, 0.15), (3, 0.45), (4, 0.45)];
            assert_eq!(ids(&&world[..], ALL_LAYERS), all);
            assert_eq!(ids(&(&world[..], &world[..]), ALL_LAYERS), all);
            assert_eq!(ids(&&world[..], 0b01), vec![(9, 0.15), (4, 0.45)]);
            assert_eq!(ids(&&world[..], 0b10), vec![(3, 0.45)]);
        }
    }

    /// Defect: a world that finds colliders by region leaves out one the
    /// ray only touches, so a pick depends on the world. Oracle: a slab of
    /// voxels two cells along x and two along y (y = -1 and y = 0), each
    /// the cell (x, y, 0) with id `10 x + y + 1`, and a ray from (-1, 0,
    /// 0.5) along x by 4, lying in the plane y = 0 the two layers share. It
    /// is over all four: the cells at x = 0 from a quarter to a half, those
    /// at x = 1 from a half to three quarters, each pair in id order. A
    /// slice, a set and a cell world answer the same.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_between_two_layers_of_voxels_is_over_both_in_every_world() {
        let half = Vec3::splat(0.5);
        let solid =
            |cell: IVec3| (0..2).contains(&cell.x) && (-1..1).contains(&cell.y) && cell.z == 0;
        let id = |cell: IVec3| u64::try_from(10 * cell.x + cell.y + 1).unwrap();
        let mut slice = Vec::new();
        let mut set = ColliderSet::new(1.0);
        for cell in [(0, -1), (0, 0), (1, -1), (1, 0)].map(|(x, y)| IVec3::new(x, y, 0)) {
            let voxel = Collider::new(id(cell), Shape::Box { half }, cell.as_vec3() + half);
            slice.push(voxel);
            set.insert(voxel);
        }
        let cells = CellWorld::new(1.0, |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
            if solid(cell) {
                out.push(Collider::new(id(cell), Shape::Box { half }, half));
            }
        });
        let slice = &slice[..];
        let worlds: [&dyn World<Vec3>; 3] = [&slice, &set, &cells];
        for world in worlds {
            let found: Vec<(u64, f32, f32)> =
                ray_spans(Vec3::new(-1.0, 0.0, 0.5), Vec3::X * 4.0, world, ALL_LAYERS)
                    .unwrap()
                    .into_iter()
                    .map(|span| (span.id, span.enter, span.exit))
                    .collect();
            assert_eq!(
                found,
                vec![
                    (0, 0.25, 0.5),
                    (1, 0.25, 0.5),
                    (10, 0.5, 0.75),
                    (11, 0.5, 0.75)
                ]
            );
        }
    }

    /// Defect: a 3D convex shape, which has no closed form here, is listed
    /// with an invented span, or a motion with no direction or a start
    /// that is nowhere lists the world. Oracle: the documented contract. A
    /// triangle a ray crosses, and a ball of no radius it passes through,
    /// are not listed; a zero motion from inside a box, a motion that is
    /// not finite and a start that is not finite list nothing.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_shape_with_no_closed_solid_and_a_ray_with_no_path_list_nothing() {
        let v = Vec3::new;
        let corners = [v(-10.0, 0.0, -10.0), v(10.0, 0.0, -10.0), v(0.0, 0.0, 10.0)];
        let level = Convex::hull(&corners, sgl_core::math::Quat::IDENTITY);
        let world = [
            Collider::new(1, Shape::Convex(level), v(0.0, 0.5, 0.0)),
            Collider::new(2, Shape::Ball { radius: 0.0 }, v(0.0, 2.0, 0.0)),
        ];
        assert_eq!(spans(v(0.0, 5.0, 0.0), Vec3::NEG_Y * 10.0, &world), vec![]);
        let world = cell();
        assert_eq!(spans(Vec3::splat(0.5), Vec3::ZERO, &world), vec![]);
        assert_eq!(
            spans(Vec3::splat(0.5), Vec3::X * f32::INFINITY, &world),
            vec![]
        );
        assert_eq!(spans(Vec3::splat(f32::NAN), Vec3::X, &world), vec![]);
    }
}
