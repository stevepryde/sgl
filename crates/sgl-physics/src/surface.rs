//! What the two surface worlds share: a triangle offered as a convex
//! collider, the box a world finds it by, the box tests a world walks
//! with, and why a surface was refused.
//!
//! The swept-box test follows `Box3D` v0.1.0 (MIT, `PROVENANCE.md`):
//! `b3TestBoundsOverlap` and `b3TestBoundsRayOverlap` of `src/simd.h`, as
//! `b3ShapeCastMesh` of `src/mesh.c` uses them.

use core::cmp::Ordering;
use core::fmt;

use sgl_core::math::Vec3;

use crate::shape::{Collider, Convex, Shape};
use crate::space;

/// Why a [`TriangleSet`](crate::TriangleSet) or a
/// [`HeightField`](crate::HeightField) was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceError {
    /// A position, height, cell size or origin is not finite.
    NotFinite,
    /// The index count is not a multiple of three, or an index names no
    /// position.
    Indices,
    /// The ids are neither one for all nor one per triangle (one per cell
    /// for a height field).
    Ids,
    /// More triangles than [`MAX_TRIANGLES`](crate::MAX_TRIANGLES), or more
    /// samples than [`MAX_HEIGHT_SAMPLES`](crate::MAX_HEIGHT_SAMPLES).
    TooMany,
    /// A height field with fewer than two samples along an axis, a sample
    /// count that is not `columns * rows`, a cell size that is not positive,
    /// or cells too small to tell apart at its origin.
    Grid,
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotFinite => {
                "surface: every position, height, cell size and origin must be finite"
            }
            Self::Indices => "surface: three indices a triangle, each naming a position",
            Self::Ids => "surface: give one id for all, or one per triangle (per cell)",
            Self::TooMany => "surface: more triangles or samples than one surface holds",
            Self::Grid => {
                "surface: a height field needs at least 2 x 2 samples, columns * rows of them, \
                 and a positive cell size its origin can tell apart"
            }
        })
    }
}

impl std::error::Error for SurfaceError {}

/// One triangle of a surface and the game's id for it.
///
/// Its corners are kept in a fixed order (by their coordinates, x first),
/// whatever order the game wound them in, so a triangle and the same
/// triangle wound the other way, or started from another corner, are the
/// same collider to the bit. Triangles are two-sided.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Triangle {
    corners: [Vec3; 3],
    pub id: u64,
}

impl Triangle {
    /// The triangle `a`, `b`, `c`, or `None` when it is degenerate: no area
    /// to `f32`'s precision (its corners in a line or coincident), or so
    /// large or so far out that its centre or its corners about it are not
    /// finite.
    pub fn new(a: Vec3, b: Vec3, c: Vec3, id: u64) -> Option<Self> {
        let mut corners = [a, b, c];
        corners.sort_by(|p, q| space::compare(*p, *q));
        let out = Self { corners, id };
        let collider = out.collider();
        let Shape::Convex(hull) = collider.shape else {
            return None;
        };
        let sound = space::is_finite(collider.position)
            && hull.points().iter().all(|&point| space::is_finite(point));
        (sound && has_area(corners)).then_some(out)
    }

    /// A fixed total order over the corners, x first: with the id, the
    /// order a [`TriangleSet`](crate::TriangleSet) keeps its triangles in.
    pub fn corners_order(&self, other: &Self) -> Ordering {
        self.corners
            .iter()
            .zip(&other.corners)
            .map(|(a, b)| space::compare(*a, *b))
            .find(|order| order.is_ne())
            .unwrap_or(Ordering::Equal)
    }

    /// The triangle as a solid convex collider on every layer: its corners
    /// about the centre of the box that encloses them, so the collider's
    /// box is exactly the triangle's and its position moves with it, to
    /// the bit, under a translation that is exact.
    pub fn collider(&self) -> Collider<Vec3> {
        let [a, b, c] = self.corners;
        let lo = space::min_each(space::min_each(a, b), c);
        let hi = space::max_each(space::max_each(a, b), c);
        let position = (lo + hi) * 0.5;
        let hull = Convex::triangle([a - position, b - position, c - position]);
        Collider::new(self.id, Shape::Convex(hull), position)
    }

    /// The closed box a world finds the triangle by: its collider's box
    /// grown on each axis by twice the collider's rounding allowance, so it
    /// holds the box grown once ([`World`](crate::World)) however computing
    /// it rounds.
    pub fn reach(&self) -> (Vec3, Vec3) {
        let collider = self.collider();
        let grow = collider.shape.half_extents() + collider.rounding() * 2.0;
        (collider.position - grow, collider.position + grow)
    }
}

/// Whether the corners span an area `f32` can tell from none: the cross
/// product of two edges is longer than four units of rounding of the
/// product of their lengths, and the edges' products fit `f32`. The edges
/// are scaled by their largest component first, so neither overflows nor
/// underflows the test.
fn has_area(corners: [Vec3; 3]) -> bool {
    let (e1, e2) = (corners[1] - corners[0], corners[2] - corners[0]);
    let scale = space::max(
        space::max(space::largest_abs(e1), space::largest_abs(e2)),
        space::largest_abs(corners[2] - corners[1]),
    );
    // A ray crosses a triangle by its edges' cross product in `f32`; a
    // triangle whose edges' products overflow could never be met, so it
    // is dropped as one with no area to `f32`.
    if !(scale.is_finite() && scale > 0.0 && (2.0 * scale * scale).is_finite()) {
        return false;
    }
    let (e1, e2) = (e1 * (1.0 / scale), e2 * (1.0 / scale));
    let n = cross(e1, e2);
    let tolerance = 4.0 * f32::EPSILON;
    dot(n, n) > tolerance * tolerance * (dot(e1, e1) * dot(e2, e2))
}

/// `a × b`, each component in a fixed order.
fn cross(a: Vec3, b: Vec3) -> Vec3 {
    Vec3::new(
        a.y * b.z - a.z * b.y,
        a.z * b.x - a.x * b.z,
        a.x * b.y - a.y * b.x,
    )
}

/// `a · b`, x then y then z.
fn dot(a: Vec3, b: Vec3) -> f32 {
    a.x * b.x + a.y * b.y + a.z * b.z
}

/// Whether the closed boxes `lo..=hi` and `min..=max` meet.
pub(crate) fn boxes_meet(lo: Vec3, hi: Vec3, min: Vec3, max: Vec3) -> bool {
    (0..3).all(|axis| lo[axis] <= max[axis] && min[axis] <= hi[axis])
}

/// Whether the closed box `lo..=hi` meets the closed box `min..=max` swept
/// by `motion`, erring toward meeting by a few units of rounding of the
/// largest coordinate either holds, so a box the sweep reaches exactly is
/// never left out.
///
/// The sweep is the centre of `min..=max` moving by `motion` against
/// `lo..=hi` grown by its half-extents, as `b3ShapeCastMesh` sets it up:
/// separated when the two boxes enclosing them are (`b3TestBoundsOverlap`),
/// or when an axis across the motion and a box edge separates them
/// (`b3TestBoundsRayOverlap`, the segment-box separating-axis test of Gino
/// van den Bergen's *Collision Detection in Interactive 3D Environments*,
/// p. 80). Those six axes decide exactly whether a segment meets a box.
pub(crate) fn box_meets_along(lo: Vec3, hi: Vec3, min: Vec3, max: Vec3, motion: Vec3) -> bool {
    let (end_min, end_max) = (min + motion, max + motion);
    let largest = [lo, hi, min, max, end_min, end_max]
        .into_iter()
        .fold(0.0, |largest, v| space::max(largest, space::largest_abs(v)));
    let slack = Vec3::splat(16.0 * f32::EPSILON * largest);
    let (lo, hi) = (lo - slack, hi + slack);
    let swept_min = space::min_each(min, end_min);
    let swept_max = space::max_each(max, end_max);
    if !boxes_meet(lo, hi, swept_min, swept_max) {
        return false;
    }
    let centre = (min + max) * 0.5;
    let extent = (max - min) * 0.5;
    let (node_min, node_max) = (lo - extent, hi + extent);
    let node_centre = (node_min + node_max) * 0.5;
    let node_extent = node_max - node_centre;
    let start = centre - node_centre;
    let reach = cross(motion, start).abs();
    let along = motion.abs();
    let room = Vec3::new(
        along.y * node_extent.z + along.z * node_extent.y,
        along.z * node_extent.x + along.x * node_extent.z,
        along.x * node_extent.y + along.y * node_extent.x,
    );
    reach.x <= room.x && reach.y <= room.y && reach.z <= room.z
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Defect: a triangle wound the other way, or started from another
    /// corner, offered as a different collider, so a result shows the
    /// order the game gave its corners in. Oracle: the requirement (order
    /// never shows): all six orders of one triangle's corners make the same
    /// collider, to the bit.
    #[wasm_bindgen_test(unsupported = test)]
    fn every_order_of_a_triangles_corners_is_the_same_collider() {
        let (a, b, c) = (
            Vec3::new(0.5, -1.25, 3.0),
            Vec3::new(-2.0, 0.75, 1.5),
            Vec3::new(1.0, 2.0, -0.5),
        );
        let first = Triangle::new(a, b, c, 7).expect("has area").collider();
        for (p, q, r) in [(a, c, b), (b, a, c), (b, c, a), (c, a, b), (c, b, a)] {
            let other = Triangle::new(p, q, r, 7).expect("has area").collider();
            assert_eq!(other, first);
        }
    }

    /// Defect: a triangle with no area kept (the kernel then meets a
    /// segment where the game meant nothing), or a thin one with area
    /// dropped. Oracle: the geometry: corners in a line and two coincident
    /// are
    /// degenerate; so is one whose edges overflow `f32`; a sliver one part
    /// in a thousand thick is not.
    #[wasm_bindgen_test(unsupported = test)]
    fn degenerate_triangles_are_refused_and_slivers_kept() {
        let id = 1;
        let line = Triangle::new(Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0), Vec3::splat(3.0), id);
        assert!(line.is_none());
        let point = Triangle::new(Vec3::X, Vec3::X, Vec3::Y, id);
        assert!(point.is_none());
        let huge = Triangle::new(
            Vec3::new(-3e38, 0.0, 0.0),
            Vec3::new(3e38, 0.0, 0.0),
            Vec3::new(0.0, 3e38, 0.0),
            id,
        );
        assert!(huge.is_none());
        let sliver = Triangle::new(
            Vec3::ZERO,
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.5, 1e-3, 0.0),
            id,
        );
        assert!(sliver.is_some());
    }

    /// Defect: the swept test leaves out a box the sweep reaches, so a
    /// world misses a collider on a long diagonal motion, or takes in one
    /// the sweep passes. Oracle: hand-worked geometry: a unit box at the
    /// origin, a point moving from (-2, 0, -2) toward (2, 0, 2) passes
    /// through it; moving from (-3, 0, -1) by (4, 0, 4) passes 1/√2 beside
    /// its corner (the line z = x + 2 never comes within the box, though
    /// the box enclosing the motion holds it); a point ending exactly on
    /// its face, and a point sliding exactly along its edge, reach it.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_swept_test_meets_what_a_sweep_reaches_and_nothing_beside_it() {
        let (lo, hi) = (Vec3::splat(-0.5), Vec3::splat(0.5));
        let point = |p: Vec3| (p, p);
        let (a, b) = point(Vec3::new(-2.0, 0.0, -2.0));
        assert!(box_meets_along(lo, hi, a, b, Vec3::new(4.0, 0.0, 4.0)));
        let (a, b) = point(Vec3::new(-3.0, 0.0, -1.0));
        assert!(!box_meets_along(lo, hi, a, b, Vec3::new(4.0, 0.0, 4.0)));
        assert!(boxes_meet(
            lo,
            hi,
            Vec3::new(-3.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, 3.0)
        ));
        let (a, b) = point(Vec3::new(-2.0, 0.0, 0.0));
        assert!(box_meets_along(lo, hi, a, b, Vec3::new(1.5, 0.0, 0.0)));
        let (a, b) = point(Vec3::new(-2.0, 0.5, 0.5));
        assert!(box_meets_along(lo, hi, a, b, Vec3::new(4.0, 0.0, 0.0)));
        let (a, b) = point(Vec3::new(-2.0, 0.0, 0.0));
        assert!(!box_meets_along(lo, hi, a, b, Vec3::new(1.25, 0.0, 0.0)));
    }
}
