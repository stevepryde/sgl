//! A world the crate stores: colliders bucketed into a uniform grid.

use std::collections::HashMap;

use crate::shape::Collider;
use crate::space::{CellRange, Vector};
use crate::world::{Unanswered, World};

/// Most grid cells one collider is bucketed into. A collider spanning more
/// (a long floor at a small cell size) is kept apart and offered to every
/// query whose cells it meets, so an insert never walks an unbounded cell
/// range.
const MAX_COLLIDER_CELLS: u64 = 1024;

/// Colliders bucketed into a uniform grid, for worlds of many colliders.
///
/// The grid's cell size is the game's choice: it is a spatial hash bucket,
/// not a tile size. Cells near the typical collider size keep queries small;
/// larger cells offer more candidates, smaller ones bucket each collider in
/// more cells. Either way a query only narrows the candidates: the crate
/// tests each one exactly.
///
/// A set is derived data. A game may keep one, or rebuild it from its own
/// state at any step, in any order, and get the same results: no result
/// depends on the order colliders were inserted in.
///
/// Ids are unique within a set: inserting a collider whose id is already
/// present replaces the one there.
///
/// Work is bounded whatever the boxes' size: a collider spanning more than
/// 1024 cells is not bucketed but checked by every query whose cells it
/// meets, and a query region spanning more cells than the set has colliders
/// scans the colliders instead of the cells. A query visits at most about as
/// many cells as the set holds colliders, plus the oversized colliders. An
/// inverted or non-finite box addresses no cells and is never a candidate.
///
/// Each collider is bucketed by its box grown by its own rounding allowance
/// (see [`World`]), so one whose face a body reaches exactly is offered even
/// when the face, computed relative to the body, lies a rounding step from
/// the cell boundary it sits on.
#[derive(Debug, Clone)]
pub struct ColliderSet<V: Vector> {
    cell_size: f32,
    colliders: Vec<Collider<V>>,
    /// The cells each collider meets, kept so a scan need not recompute them.
    ranges: Vec<Option<CellRange<V>>>,
    /// Grid cell -> positions in `colliders` of those meeting it.
    grid: HashMap<V::Cell, Vec<usize>>,
    /// Colliders spanning more than [`MAX_COLLIDER_CELLS`] cells.
    oversized: Vec<usize>,
    /// Id -> position in `colliders`. Only looked up, never iterated, so it
    /// never reaches a result.
    slots: HashMap<u64, usize>,
}

impl<V: Vector> ColliderSet<V> {
    /// An empty set bucketed at `cell_size` world units per grid cell.
    ///
    /// # Panics
    /// If `cell_size` is not positive and finite: an authoring error. A zero,
    /// negative or non-finite size cannot address a grid.
    #[must_use]
    pub fn new(cell_size: f32) -> Self {
        assert!(
            cell_size > 0.0 && cell_size.is_finite(),
            "ColliderSet: cell size must be positive and finite, got {cell_size}"
        );
        Self {
            cell_size,
            colliders: Vec::new(),
            ranges: Vec::new(),
            grid: HashMap::new(),
            oversized: Vec::new(),
            slots: HashMap::new(),
        }
    }

    /// World units per broadphase grid cell.
    #[must_use]
    pub fn cell_size(&self) -> f32 {
        self.cell_size
    }

    /// Adds a collider, bucketing it into every grid cell its enclosing box,
    /// grown by its rounding allowance, meets, or keeping it apart when that
    /// is more than 1024 cells. A collider whose id is already in the set
    /// replaces that one, in its place.
    pub fn insert(&mut self, collider: Collider<V>) {
        let index = if let Some(&index) = self.slots.get(&collider.id) {
            self.unbucket(index);
            self.colliders[index] = collider;
            index
        } else {
            let index = self.colliders.len();
            self.slots.insert(collider.id, index);
            self.colliders.push(collider);
            self.ranges.push(None);
            index
        };
        let half = collider.shape.half_extents();
        let grown = half + collider.rounding();
        let range = CellRange::<V>::of_corners(
            collider.position - grown,
            collider.position + grown,
            self.cell_size,
        );
        if let Some(cells) = range {
            if cells.count() > MAX_COLLIDER_CELLS {
                self.oversized.push(index);
            } else {
                cells.for_each(|cell| self.grid.entry(cell).or_default().push(index));
            }
        }
        self.ranges[index] = range;
    }

    /// Takes the collider at `index` out of the grid, leaving its slot.
    fn unbucket(&mut self, index: usize) {
        let Some(cells) = self.ranges[index].take() else {
            return;
        };
        if cells.count() > MAX_COLLIDER_CELLS {
            self.oversized.retain(|&other| other != index);
            return;
        }
        cells.for_each(|cell| {
            if let Some(bucket) = self.grid.get_mut(&cell) {
                bucket.retain(|&other| other != index);
                if bucket.is_empty() {
                    self.grid.remove(&cell);
                }
            }
        });
    }

    /// Number of colliders in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.colliders.len()
    }

    /// Whether the set holds no colliders.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.colliders.is_empty()
    }

    /// Removes every collider and its grid buckets, keeping the cell size.
    pub fn clear(&mut self) {
        self.colliders.clear();
        self.ranges.clear();
        self.grid.clear();
        self.oversized.clear();
        self.slots.clear();
    }

    /// The colliders, in the order they were first inserted: a replacement
    /// takes the place of the collider it replaced.
    pub fn iter(&self) -> std::slice::Iter<'_, Collider<V>> {
        self.colliders.iter()
    }

    /// Positions in the set of the candidates for the closed box
    /// `min..=max`, each once, ascending.
    pub(crate) fn candidates(&self, min: V, max: V) -> Vec<usize> {
        let Some(cells) = CellRange::<V>::of_corners(min, max, self.cell_size) else {
            return Vec::new();
        };
        let meets = |index: &usize| self.ranges[*index].is_some_and(|other| other.meets(&cells));
        let mut found = Vec::new();
        if cells.count() > self.colliders.len() as u64 {
            found.extend((0..self.colliders.len()).filter(meets));
            return found;
        }
        cells.for_each(|cell| {
            if let Some(bucket) = self.grid.get(&cell) {
                found.extend_from_slice(bucket);
            }
        });
        found.extend(self.oversized.iter().copied().filter(meets));
        found.sort_unstable();
        found.dedup();
        found
    }
}

impl<V: Vector> World<V> for ColliderSet<V> {
    fn colliders_near(
        &self,
        min: V,
        max: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered> {
        for index in self.candidates(min, max) {
            visit(&self.colliders[index]);
        }
        Ok(())
    }
}

impl<'a, V: Vector> IntoIterator for &'a ColliderSet<V> {
    type Item = &'a Collider<V>;
    type IntoIter = std::slice::Iter<'a, Collider<V>>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::{ColliderKind, Shape};
    use sgl_core::math::{Vec2, Vec3};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn boxed(id: u64, cx: f32, cy: f32, hx: f32, hy: f32) -> Collider<Vec2> {
        Collider::new(
            id,
            Shape::Box {
                half: Vec2::new(hx, hy),
            },
            Vec2::new(cx, cy),
        )
    }

    fn region(cx: f32, cy: f32, hx: f32, hy: f32) -> (Vec2, Vec2) {
        let (c, h) = (Vec2::new(cx, cy), Vec2::new(hx, hy));
        (c - h, c + h)
    }

    fn query(set: &ColliderSet<Vec2>, (min, max): (Vec2, Vec2)) -> Vec<usize> {
        set.candidates(min, max)
    }

    /// #249: colliders at negative, fractional coordinates with a cell size
    /// that does not divide them are found by a query in their cells, and a
    /// non-finite region finds nothing even with a collider at the origin.
    #[wasm_bindgen_test(unsupported = test)]
    fn negative_fractional_boxes_are_found_and_non_finite_regions_find_nothing() {
        let mut set = ColliderSet::new(1.5);
        set.insert(boxed(0, 0.0, 0.0, 0.5, 0.5));
        set.insert(boxed(1, -2.2, -3.7, 0.4, 0.4));
        assert_eq!(query(&set, region(-2.5, -4.0, 0.2, 0.2)), vec![1]);
        assert_eq!(
            query(&set, region(-5.0, -5.0, 0.2, 0.2)),
            Vec::<usize>::new()
        );
        assert_eq!(query(&set, region(0.2, 0.2, 0.1, 0.1)), vec![0]);
        assert!(query(&set, region(f32::NAN, 0.0, 1.0, 1.0)).is_empty());
        assert!(query(&set, region(0.0, 0.0, f32::INFINITY, 1.0)).is_empty());
    }

    /// The configured cell size must drive bucketing. With cells far larger
    /// than the colliders, two boxes 50 units apart share a cell and both come
    /// back as candidates; a hard-coded cell size would return only the near
    /// one.
    #[wasm_bindgen_test(unsupported = test)]
    fn configured_cell_size_decides_bucketing() {
        let mut coarse = ColliderSet::new(100.0);
        coarse.insert(boxed(0, 0.0, 0.0, 0.5, 0.5));
        coarse.insert(boxed(1, 50.0, 0.0, 0.5, 0.5));
        assert_eq!(query(&coarse, region(0.0, 0.0, 1.0, 1.0)), vec![0, 1]);

        let mut fine = ColliderSet::new(1.0);
        fine.insert(boxed(0, 0.0, 0.0, 0.5, 0.5));
        fine.insert(boxed(1, 50.0, 0.0, 0.5, 0.5));
        assert_eq!(query(&fine, region(0.0, 0.0, 1.0, 1.0)), vec![0]);
    }

    /// A collider wider than one cell must be reachable from every cell it
    /// covers and still be a candidate exactly once. At a 0.25 cell size the
    /// wide floor occupies dozens of buckets, so a missing dedupe repeats it
    /// and centre-only bucketing loses it.
    #[wasm_bindgen_test(unsupported = test)]
    fn multi_cell_colliders_are_candidates_once() {
        let mut set = ColliderSet::new(0.25);
        set.insert(boxed(0, 0.0, 0.0, 1.0, 0.25));
        set.insert(boxed(1, 10.0, 10.0, 0.5, 0.5));
        set.insert(boxed(2, 0.5, 0.5, 0.3, 0.3));

        assert_eq!(query(&set, region(0.0, 0.0, 1.0, 1.0)), vec![0, 2]);
        // Reachable from a region touching only its right end.
        assert_eq!(query(&set, region(0.9, -0.2, 0.05, 0.02)), vec![0]);
        assert_eq!(query(&set, region(10.0, 10.0, 0.1, 0.1)), vec![1]);
    }

    /// Clearing must drop the grid too. Colliders alone would leave buckets
    /// holding positions that no longer exist, and the next query would read
    /// past the end of the set.
    #[wasm_bindgen_test(unsupported = test)]
    fn clear_drops_the_grid_with_the_colliders() {
        let mut set = ColliderSet::new(1.0);
        set.insert(boxed(0, 0.0, 0.0, 0.5, 0.5));
        set.insert(boxed(1, 1.0, 0.0, 0.5, 0.5));
        set.clear();

        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
        assert!(query(&set, region(0.0, 0.0, 2.0, 2.0)).is_empty());

        set.insert(boxed(7, 0.0, 0.0, 0.5, 0.5).with_kind(ColliderKind::Sensor));
        assert_eq!(query(&set, region(0.0, 0.0, 0.5, 0.5)), vec![0]);
        assert_eq!(
            set.iter().next().map(|c| (c.id, c.kind)),
            Some((7, ColliderKind::Sensor))
        );
    }

    /// #316: a query region spanning 4e12 cells returns the colliders in it
    /// without walking its cells.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_huge_query_region_is_answered_without_walking_its_cells() {
        let mut set = ColliderSet::new(1.0);
        set.insert(boxed(0, 0.0, 0.0, 0.5, 0.5));
        set.insert(boxed(1, 9e5, -9e5, 0.5, 0.5));
        set.insert(boxed(2, 3e6, 0.0, 0.5, 0.5));
        assert_eq!(query(&set, region(0.0, 0.0, 1e6, 1e6)), vec![0, 1]);
    }

    /// #316: an inverted box (negative half-extents spanning cells) addresses
    /// no cells: as a collider it is never a candidate, and as a region it
    /// finds nothing, whether the query walks the grid or scans.
    #[wasm_bindgen_test(unsupported = test)]
    fn inverted_boxes_address_no_cells() {
        let mut set = ColliderSet::new(1.0);
        set.insert(boxed(0, 0.0, 0.0, 0.5, 0.5));
        set.insert(boxed(1, 0.0, 0.0, -3.0, -3.0));
        for i in 0..8u8 {
            set.insert(boxed(2 + u64::from(i), -50.0, f32::from(i) * 4.0, 0.5, 0.5));
        }
        // Grid path (4 cells, 10 colliders) and scan path (1e6 cells).
        assert_eq!(query(&set, region(0.0, 0.0, 0.5, 0.5)), vec![0]);
        assert_eq!(query(&set, region(0.0, 0.0, 500.0, 500.0)).len(), 9);
        assert!(query(&set, region(0.0, 0.0, -2.0, -2.0)).is_empty());
        assert!(query(&set, region(0.0, 0.0, -500.0, -500.0)).is_empty());
    }

    /// #316: a collider spanning 4e12 cells is inserted without walking its
    /// cells and is still found by a grid query from inside it, and only
    /// there.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_huge_collider_is_found_from_the_cells_it_covers() {
        let mut set = ColliderSet::new(1.0);
        set.insert(boxed(0, 3e6, 0.0, 0.5, 0.5));
        set.insert(boxed(1, 0.0, 0.0, 1e6, 1e6));
        // More colliders than a small query's cells, so it walks the grid.
        for i in 0..8u8 {
            set.insert(boxed(2 + u64::from(i), -3e6, f32::from(i) * 4.0, 0.5, 0.5));
        }
        assert_eq!(query(&set, region(5e5, -5e5, 0.5, 0.5)), vec![1]);
        assert_eq!(query(&set, region(3e6, 0.0, 0.5, 0.5)), vec![0]);
        set.clear();
        assert!(query(&set, region(5e5, -5e5, 0.5, 0.5)).is_empty());
    }

    /// A non-finite box must address no cells. Saturating casts would otherwise
    /// turn an infinite bound into a walk over four billion grid cells.
    #[wasm_bindgen_test(unsupported = test)]
    fn non_finite_regions_address_no_cells() {
        let mut set = ColliderSet::new(1.0);
        set.insert(boxed(0, 0.0, 0.0, 0.5, 0.5));
        assert!(
            query(&set, region(0.0, 0.0, f32::INFINITY, 1.0)).is_empty(),
            "an infinite region must not be walked cell by cell"
        );
        assert!(query(&set, region(f32::NAN, 0.0, 1.0, 1.0)).is_empty());
    }

    /// The grid works in three dimensions: a box is found from a region on
    /// its z extent and not from one beside it on z alone.
    #[wasm_bindgen_test(unsupported = test)]
    fn three_dimensional_boxes_are_bucketed_along_z() {
        let mut set = ColliderSet::new(1.0);
        set.insert(Collider::new(
            0,
            Shape::Box {
                half: Vec3::splat(0.5),
            },
            Vec3::new(0.0, 0.0, 5.0),
        ));
        let at =
            |z: f32| set.candidates(Vec3::new(-0.2, -0.2, z - 0.2), Vec3::new(0.2, 0.2, z + 0.2));
        assert_eq!(at(5.0), vec![0]);
        assert!(at(0.0).is_empty());
    }

    /// An id is unique within a set: inserting id 1 again replaces it, so
    /// the old wall (x 2..3) no longer blocks a body moving right and the
    /// new one (x 5..6) does, and the set holds one collider.
    #[wasm_bindgen_test(unsupported = test)]
    fn inserting_an_id_again_replaces_the_collider() {
        let mut set = ColliderSet::new(1.0);
        set.insert(boxed(1, 2.5, 0.0, 0.5, 2.0));
        set.insert(boxed(1, 5.5, 0.0, 0.5, 2.0));
        assert_eq!(set.len(), 1);
        let body = Shape::Box {
            half: Vec2::splat(0.5),
        };
        let hit = crate::cast(&body, Vec2::ZERO, Vec2::new(10.0, 0.0), &set, u32::MAX)
            .unwrap()
            .expect("the new wall blocks");
        // The body's right edge reaches x = 5 after 4.5 of 10 units.
        assert_eq!((hit.t, hit.id), (0.45, 1));
    }
}
