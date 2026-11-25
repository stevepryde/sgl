//! A world of triangles the game gives once, from a model or authored
//! geometry: what a camera ray, a ball cast or [`ground_under`] meets.
//!
//! The tree follows `Box3D` v0.1.0 `src/mesh.c` (MIT, Erin Catto; portions by
//! Dirk Gregorius; `PROVENANCE.md`): `b3BuildRecursive` with
//! `b3SplitMedian` and `b3SplitHalf`, the depth-first layout in which a
//! node's left child follows it and its leaves name contiguous runs of
//! triangles (`b3SortMeshTriangles`), and the stack walks of `b3QueryMesh`
//! (a region) and `b3ShapeCastMesh` (a swept box).
//!
//! [`ground_under`]: crate::ground_under

use core::fmt;

use sgl_core::math::Vec3;

use crate::shape::Collider;
use crate::space;
use crate::surface::{SurfaceError, Triangle, box_meets_along, boxes_meet};
use crate::world::{Unanswered, World};

/// Most triangles one [`TriangleSet`] holds: about a million, more than a
/// game's collision geometry needs and few enough that the set's memory
/// (about 64 bytes a triangle) stays modest.
pub const MAX_TRIANGLES: usize = 1 << 20;

/// Most nodes on a path from a [`TriangleSet`]'s root to a leaf. The build
/// never exceeds it (it halves its runs once a path grows long), so a walk
/// keeps its pending nodes in a fixed stack of this many.
pub const MAX_TREE_DEPTH: usize = 64;

/// Most triangles a [`TriangleSet`] offers one query. A query whose region
/// or path meets more is [`Unanswered`], like a [`CellWorld`](crate::CellWorld)
/// past [`MAX_QUERY_CELLS`](crate::MAX_QUERY_CELLS): the caller moves
/// nothing and is told. Only triangles whose own box meets the region or
/// path count, so the count depends on the geometry, never on the order the
/// triangles were given in or the shape of the tree.
pub const MAX_QUERY_TRIANGLES: u64 = 4096;

/// A leaf holds at most this many triangles (`B3_DESIRED_TRIANGLES_PER_LEAF`).
const TRIANGLES_PER_LEAF: usize = 4;

/// Below this depth a run is split at the middle of its triangles' centres
/// (`b3SplitMedian`); from it on, in half by count (`b3SplitHalf`), which
/// reaches a leaf from any run of at most [`MAX_TRIANGLES`] within the
/// depth left.
const MEDIAN_DEPTH: usize = MAX_TREE_DEPTH - 20;
const _: () = assert!(MAX_TRIANGLES <= TRIANGLES_PER_LEAF << (MAX_TREE_DEPTH - MEDIAN_DEPTH));

/// Triangles the game gives once, built into a tree: a world a ray, a cast,
/// an overlap or [`ground_under`](crate::ground_under) can ask.
///
/// Each triangle is offered to a query as a solid convex collider of its
/// three corners on every layer ([`Shape::Convex`](crate::Shape::Convex)),
/// carrying the game's id, so it is met as any convex collider is: exactly
/// by a [`ray`](crate::ray) or [`ground_under`](crate::ground_under), at its
/// own height and with its own normal, and through the convex kernel by a
/// shape with extent, to within the kernel's slop, a hundredth of the
/// smaller shape's extent (`specs/physics.md`). Triangles are two-sided: a
/// ray or a cast meets one from either side, whichever way it is wound.
/// Nothing walks on them: the mover's floors are still authored as simple
/// shapes.
///
/// The set is immutable. Results never depend on the order the triangles
/// were given in or on how each was wound: the set keeps every triangle's
/// corners and the triangles themselves in a fixed order, and builds its
/// tree from that.
#[derive(Clone)]
pub struct TriangleSet {
    /// The triangles in the tree's depth-first order.
    triangles: Vec<Triangle>,
    /// Each triangle's box ([`Triangle::reach`]), in the same order.
    reaches: Vec<(Vec3, Vec3)>,
    /// The tree, root first, each node's left child just after it.
    nodes: Vec<Node>,
    degenerate: usize,
}

#[derive(Clone, Copy, Debug)]
struct Node {
    /// The box holding every triangle's box beneath it.
    lo: Vec3,
    hi: Vec3,
    kind: Kind,
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    /// Triangles `first..first + count` of the set.
    Leaf { first: u32, count: u32 },
    /// The right child's index; the left child follows this node.
    Inner { right: u32 },
}

/// A triangle as the build sorts it (`b3Primitive`).
#[derive(Clone, Copy)]
struct Primitive {
    lo: Vec3,
    hi: Vec3,
    centre: Vec3,
    index: u32,
}

impl TriangleSet {
    /// The triangles `indices` makes of `positions`, three indices each,
    /// with `ids` the game's id for each triangle, or one id for them all.
    ///
    /// A triangle with no area to `f32`'s precision (its corners in a line
    /// or coincident), or so large its edges or their products overflow,
    /// is dropped and counted ([`Self::degenerate`]), not refused: a
    /// model's slivers are not an authoring error.
    ///
    /// # Errors
    /// [`SurfaceError::Indices`] when the index count is not a multiple of
    /// three or an index names no position; [`SurfaceError::TooMany`] for
    /// more than [`MAX_TRIANGLES`]; [`SurfaceError::Ids`] when `ids` is
    /// neither one id nor one per triangle; [`SurfaceError::NotFinite`] when
    /// a position is not finite.
    pub fn new(positions: &[Vec3], indices: &[u32], ids: &[u64]) -> Result<Self, SurfaceError> {
        if !indices.len().is_multiple_of(3) {
            return Err(SurfaceError::Indices);
        }
        let count = indices.len() / 3;
        if count > MAX_TRIANGLES {
            return Err(SurfaceError::TooMany);
        }
        if !(ids.len() == 1 || ids.len() == count) {
            return Err(SurfaceError::Ids);
        }
        if !positions.iter().all(|&p| space::is_finite(p)) {
            return Err(SurfaceError::NotFinite);
        }
        let corner = |index: u32| {
            usize::try_from(index)
                .ok()
                .and_then(|index| positions.get(index).copied())
                .ok_or(SurfaceError::Indices)
        };
        let mut triangles = Vec::with_capacity(count);
        let mut degenerate = 0;
        for (n, three) in indices.chunks_exact(3).enumerate() {
            let (a, b, c) = (corner(three[0])?, corner(three[1])?, corner(three[2])?);
            let id = if ids.len() == 1 { ids[0] } else { ids[n] };
            match Triangle::new(a, b, c, id) {
                Some(triangle) => triangles.push(triangle),
                None => degenerate += 1,
            }
        }
        // A fixed order, so the tree is the same whatever order the game
        // gave: the id, then the corners. Equal triangles are the same
        // collider, so their order among themselves cannot show.
        triangles.sort_unstable_by(|a, b| a.id.cmp(&b.id).then_with(|| a.corners_order(b)));
        Ok(Self::built(&triangles, degenerate))
    }

    /// The set of `triangles`, in their fixed order, with its tree.
    fn built(triangles: &[Triangle], degenerate: usize) -> Self {
        let mut primitives: Vec<Primitive> = triangles
            .iter()
            .zip(0u32..)
            .map(|(triangle, index)| {
                let (lo, hi) = triangle.reach();
                Primitive {
                    lo,
                    hi,
                    centre: triangle.collider().position,
                    index,
                }
            })
            .collect();
        let mut nodes = Vec::with_capacity(2 * primitives.len());
        if !primitives.is_empty() {
            build(&mut nodes, &mut primitives, 0, 1);
        }
        // The build partitioned the primitives in place, so each leaf's run
        // is already contiguous in depth-first order: lay the triangles out
        // the same way (`b3SortMeshTriangles`).
        let ordered: Vec<Triangle> = primitives
            .iter()
            .map(|p| triangles[index(p.index)])
            .collect();
        let reaches = primitives.iter().map(|p| (p.lo, p.hi)).collect();
        Self {
            triangles: ordered,
            reaches,
            nodes,
            degenerate,
        }
    }

    /// How many triangles the set holds, degenerate ones left out.
    #[must_use]
    pub fn len(&self) -> usize {
        self.triangles.len()
    }

    /// Whether the set holds no triangle.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    /// How many triangles were dropped as degenerate.
    #[must_use]
    pub fn degenerate(&self) -> usize {
        self.degenerate
    }

    /// The most nodes on a path from the root to a leaf.
    #[cfg(test)]
    fn depth(&self) -> usize {
        fn below(nodes: &[Node], at: usize) -> usize {
            match nodes[at].kind {
                Kind::Leaf { .. } => 1,
                Kind::Inner { right } => 1 + below(nodes, at + 1).max(below(nodes, index(right))),
            }
        }
        if self.nodes.is_empty() {
            0
        } else {
            below(&self.nodes, 0)
        }
    }

    /// Walks the tree from the root, entering every node whose box `enters`
    /// accepts, and offers every triangle in a leaf entered whose own box
    /// it accepts: at most [`MAX_QUERY_TRIANGLES`] of them, or
    /// [`Unanswered`].
    fn walk(
        &self,
        enters: impl Fn(Vec3, Vec3) -> bool,
        visit: &mut dyn FnMut(&Collider<Vec3>),
    ) -> Result<(), Unanswered> {
        if self.nodes.is_empty() {
            return Ok(());
        }
        // A node is pushed only on the way down a path, so the stack never
        // holds more than the tree is deep.
        let mut stack = [0u32; MAX_TREE_DEPTH];
        let mut pending = 0;
        let mut at = 0;
        let mut offered = 0u64;
        loop {
            let node = self.nodes[at];
            if enters(node.lo, node.hi) {
                match node.kind {
                    Kind::Inner { right } => {
                        stack[pending] = right;
                        pending += 1;
                        at += 1;
                        continue;
                    }
                    Kind::Leaf { first, count } => {
                        for i in index(first)..index(first) + index(count) {
                            let (lo, hi) = self.reaches[i];
                            if enters(lo, hi) {
                                offered += 1;
                                if offered > MAX_QUERY_TRIANGLES {
                                    return Err(Unanswered);
                                }
                                visit(&self.triangles[i].collider());
                            }
                        }
                    }
                }
            }
            if pending == 0 {
                return Ok(());
            }
            pending -= 1;
            at = index(stack[pending]);
        }
    }
}

impl fmt::Debug for TriangleSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TriangleSet")
            .field("len", &self.len())
            .field("degenerate", &self.degenerate)
            .finish_non_exhaustive()
    }
}

impl World<Vec3> for TriangleSet {
    /// Offers every triangle whose box, grown by its rounding allowance,
    /// meets the closed box `min..=max`: the nodes whose boxes meet it are
    /// walked (`b3QueryMesh`). [`Unanswered`] when more than
    /// [`MAX_QUERY_TRIANGLES`] do, or the region is not finite.
    fn colliders_near(
        &self,
        min: Vec3,
        max: Vec3,
        visit: &mut dyn FnMut(&Collider<Vec3>),
    ) -> Result<(), Unanswered> {
        if !(space::is_finite(min) && space::is_finite(max)) {
            return Err(Unanswered);
        }
        self.walk(|lo, hi| boxes_meet(lo, hi, min, max), visit)
    }

    /// Offers every triangle whose box meets the closed box `min..=max`
    /// swept by `motion`: the nodes the sweep meets are walked
    /// (`b3ShapeCastMesh`), and so is each triangle's own box, so a long ray
    /// across a large mesh offers only what lies along it. [`Unanswered`]
    /// when more than [`MAX_QUERY_TRIANGLES`] do, or the sweep is not finite.
    fn colliders_along(
        &self,
        min: Vec3,
        max: Vec3,
        motion: Vec3,
        visit: &mut dyn FnMut(&Collider<Vec3>),
    ) -> Result<(), Unanswered> {
        let finite = space::is_finite(min)
            && space::is_finite(max)
            && space::is_finite(min + motion)
            && space::is_finite(max + motion);
        if !finite {
            return Err(Unanswered);
        }
        self.walk(|lo, hi| box_meets_along(lo, hi, min, max, motion), visit)
    }
}

/// A `u32` the set stored as an index.
fn index(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// A count of at most [`MAX_TRIANGLES`] as a `u32`.
fn small(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Builds the subtree of `primitives`, which start at `offset` in the set,
/// at `depth` (the root is 1), and returns its index (`b3BuildRecursive`).
fn build(nodes: &mut Vec<Node>, primitives: &mut [Primitive], offset: usize, depth: usize) -> u32 {
    let (lo, hi) = bounds(primitives);
    let at = nodes.len();
    nodes.push(Node {
        lo,
        hi,
        kind: Kind::Leaf {
            first: small(offset),
            count: small(primitives.len()),
        },
    });
    if primitives.len() > TRIANGLES_PER_LEAF {
        let split = if depth < MEDIAN_DEPTH {
            split_median(primitives)
        } else {
            primitives.len() / 2
        };
        let (left, right) = primitives.split_at_mut(split);
        build(nodes, left, offset, depth + 1);
        let right = build(nodes, right, offset + split, depth + 1);
        nodes[at].kind = Kind::Inner { right };
    }
    small(at)
}

/// The box holding every primitive's box.
fn bounds(primitives: &[Primitive]) -> (Vec3, Vec3) {
    primitives
        .iter()
        .fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(lo, hi), p| {
            (space::min_each(lo, p.lo), space::max_each(hi, p.hi))
        })
}

/// Partitions more than two primitives at the middle of their centres'
/// box along its longest axis, x before y before z on equal lengths, and
/// returns how many fall below it; half of them when every one falls on one
/// side, or all but one do (`b3SplitMedian`, its Hoare partition included).
fn split_median(primitives: &mut [Primitive]) -> usize {
    let count = primitives.len();
    let (mut lower, mut upper) = (primitives[0].centre, primitives[0].centre);
    for p in &primitives[1..] {
        lower = space::min_each(lower, p.centre);
        upper = space::max_each(upper, p.centre);
    }
    let d = upper - lower;
    let c = (lower + upper) * 0.5;
    let axis = if d.x >= d.y && d.x >= d.z {
        0
    } else if d.y >= d.z {
        1
    } else {
        2
    };
    let pivot = c[axis];
    let (mut i1, mut i2) = (0, count);
    while i1 < i2 {
        while i1 < i2 && primitives[i1].centre[axis] < pivot {
            i1 += 1;
        }
        while i1 < i2 && primitives[i2 - 1].centre[axis] >= pivot {
            i2 -= 1;
        }
        if i1 < i2 {
            primitives.swap(i1, i2 - 1);
            i1 += 1;
            i2 -= 1;
        }
    }
    if i1 == 0 || i1 == count - 1 {
        count / 2
    } else {
        i1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Defect: a mesh whose triangles bunch so the median split peels off
    /// only a few at each level builds a tree deeper than a walk's stack.
    /// Oracle: the cap ([`MAX_TREE_DEPTH`]). Pairs of small triangles at
    /// x = 2^k, k from -120 to 120, make every split at the middle of the
    /// centres' box leave one pair beyond it: 241 levels unguarded. The
    /// tree stays within the cap, and every triangle is still reached.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_tree_never_grows_deeper_than_its_cap() {
        let mut positions = Vec::new();
        let mut indices = Vec::new();
        for k in -120..=120 {
            let x = 2f32.powi(k);
            for dz in [0.0, 2.0] {
                let base = u32::try_from(positions.len()).unwrap();
                positions.push(Vec3::new(x, 0.0, dz));
                positions.push(Vec3::new(x, 1.0, dz));
                positions.push(Vec3::new(x, 0.0, dz + 1.0));
                indices.extend([base, base + 1, base + 2]);
            }
        }
        let set = TriangleSet::new(&positions, &indices, &[3]).unwrap();
        assert_eq!(set.len(), 482);
        assert!(set.depth() <= MAX_TREE_DEPTH, "{}", set.depth());
        let mut met = 0;
        let everywhere = Vec3::splat(f32::MAX);
        set.colliders_near(-everywhere, everywhere, &mut |_| met += 1)
            .unwrap();
        assert_eq!(met, 482);
    }
}
