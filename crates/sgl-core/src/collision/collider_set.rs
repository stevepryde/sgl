//! The world's colliders plus a uniform broadphase grid over them.

use std::collections::HashMap;

use super::aabb::Aabb;
use crate::math::IVec2;

/// How a collider behaves during a sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColliderFlags {
    /// Participates in solid collision response. A collider that is not solid
    /// is skipped entirely by the sweep.
    pub solid: bool,
    /// One-way platform: passable except when landing on the face that points
    /// along the world's up axis.
    pub one_way: bool,
    /// Sensor: never blocks, whatever `solid` says. Callers detect sensors by
    /// querying the set themselves.
    pub sensor: bool,
}

impl ColliderFlags {
    /// A plain solid collider.
    pub const SOLID: Self = Self {
        solid: true,
        one_way: false,
        sensor: false,
    };

    /// A one-way platform: solid only against a landing from above.
    pub const ONE_WAY: Self = Self {
        solid: true,
        one_way: true,
        sensor: false,
    };

    /// A sensor: reported by [`ColliderSet::query`] but never blocking.
    pub const SENSOR: Self = Self {
        solid: false,
        one_way: false,
        sensor: true,
    };
}

/// One collider in the world: its box and its flags.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Collider {
    /// World-space box.
    pub aabb: Aabb,
    /// Behavior during a sweep.
    pub flags: ColliderFlags,
}

/// Most grid cells one collider is bucketed into. A collider spanning more
/// (a long floor at a small cell size) is kept apart and offered to every
/// query whose cells it meets, so an insert never walks an unbounded cell
/// range.
const MAX_COLLIDER_CELLS: u64 = 1024;

/// The world's colliders, bucketed into a uniform grid for broadphase.
///
/// The grid's cell size is the caller's choice: it is a spatial hash bucket,
/// not a tile size. Cells near the typical collider size keep queries small;
/// much larger cells simply return more candidates, and much smaller cells make
/// each collider occupy more buckets. Either way the result is only a candidate
/// list — [`query`](Self::query) reports colliders whose *cells* meet the
/// region, not colliders that actually overlap it.
///
/// Work is bounded whatever the boxes' size: a collider spanning more than
/// 1024 cells is not bucketed but checked by every query, and a query region
/// spanning more cells than the set has colliders scans the colliders
/// instead of the cells.
#[derive(Debug)]
pub struct ColliderSet {
    cell_size: f32,
    colliders: Vec<Collider>,
    /// Grid cell -> indices of the colliders overlapping that cell.
    grid: HashMap<IVec2, Vec<usize>>,
    /// Colliders spanning more than [`MAX_COLLIDER_CELLS`] cells.
    oversized: Vec<usize>,
}

impl ColliderSet {
    /// An empty set bucketed at `cell_size` world units per grid cell.
    ///
    /// # Panics
    /// If `cell_size` is not positive and finite — an authoring error, not a
    /// runtime condition. A zero, negative, or non-finite cell size cannot
    /// address a grid at all.
    #[must_use]
    pub fn new(cell_size: f32) -> Self {
        assert!(
            cell_size > 0.0 && cell_size.is_finite(),
            "ColliderSet: cell size must be positive and finite, got {cell_size}"
        );
        Self {
            cell_size,
            colliders: Vec::new(),
            grid: HashMap::new(),
            oversized: Vec::new(),
        }
    }

    /// World units per broadphase grid cell.
    #[must_use]
    pub fn cell_size(&self) -> f32 {
        self.cell_size
    }

    /// Inserts a world-space collider, bucketing it into every grid cell its
    /// box overlaps (or keeping it apart when that is more than 1024 cells),
    /// and returns its index. A non-finite box addresses no cells and is never
    /// a query candidate.
    pub fn insert(&mut self, aabb: Aabb, flags: ColliderFlags) -> usize {
        let index = self.colliders.len();
        self.colliders.push(Collider { aabb, flags });
        if let Some(cells) = CellRange::of(&aabb, self.cell_size) {
            if cells.count() > MAX_COLLIDER_CELLS {
                self.oversized.push(index);
            } else {
                for cell in cells.iter() {
                    self.grid.entry(cell).or_default().push(index);
                }
            }
        }
        index
    }

    /// Borrows a collider by index, as returned by [`query`](Self::query).
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Collider> {
        self.colliders.get(index)
    }

    /// Broadphase candidates: the indices of colliders bucketed into any grid
    /// cell that `region` touches.
    ///
    /// Each index appears exactly once, in ascending order, so a caller that
    /// breaks ties by "first candidate wins" behaves identically run to run.
    ///
    /// A region spanning more cells than the set has colliders is answered by
    /// scanning the colliders' cell ranges, so a query costs at most one visit
    /// per collider or per cell, whichever is fewer.
    #[must_use]
    pub fn query(&self, region: &Aabb) -> Vec<usize> {
        let Some(cells) = CellRange::of(region, self.cell_size) else {
            return Vec::new();
        };
        let meets = |index: &usize| {
            CellRange::of(&self.colliders[*index].aabb, self.cell_size)
                .is_some_and(|other| other.meets(&cells))
        };
        let mut found = Vec::new();
        if cells.count() > self.colliders.len() as u64 {
            found.extend((0..self.colliders.len()).filter(meets));
            return found;
        }
        for cell in cells.iter() {
            if let Some(bucket) = self.grid.get(&cell) {
                found.extend_from_slice(bucket);
            }
        }
        found.extend(self.oversized.iter().copied().filter(meets));
        found.sort_unstable();
        found.dedup();
        found
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
    /// Indices start again from zero.
    pub fn clear(&mut self) {
        self.colliders.clear();
        self.grid.clear();
        self.oversized.clear();
    }

    /// Iterates the colliders in insertion order, so position `i` is the
    /// collider at index `i`.
    pub fn iter(&self) -> std::slice::Iter<'_, Collider> {
        self.colliders.iter()
    }
}

impl<'a> IntoIterator for &'a ColliderSet {
    type Item = &'a Collider;
    type IntoIter = std::slice::Iter<'a, Collider>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// The inclusive range of grid cells a box overlaps.
#[derive(Debug, Clone, Copy)]
struct CellRange {
    min: IVec2,
    max: IVec2,
}

impl CellRange {
    /// The cells `aabb` overlaps, or `None` for a non-finite box: float-to-int
    /// casts saturate, so an infinite bound would address the whole `i32`
    /// grid.
    fn of(aabb: &Aabb, cell_size: f32) -> Option<Self> {
        let (min, max) = (aabb.min(), aabb.max());
        (min.is_finite() && max.is_finite()).then(|| Self {
            min: IVec2::new(cell_of(min.x, cell_size), cell_of(min.y, cell_size)),
            max: IVec2::new(cell_of(max.x, cell_size), cell_of(max.y, cell_size)),
        })
    }

    /// How many cells the range holds.
    fn count(&self) -> u64 {
        let span = |lo: i32, hi: i32| u64::try_from(i64::from(hi) - i64::from(lo) + 1).unwrap_or(0);
        span(self.min.x, self.max.x).saturating_mul(span(self.min.y, self.max.y))
    }

    /// Whether the two ranges share a cell.
    fn meets(&self, other: &Self) -> bool {
        self.min.cmple(other.max).all() && other.min.cmple(self.max).all()
    }

    /// Every cell in the range, row by row.
    fn iter(self) -> impl Iterator<Item = IVec2> {
        (self.min.y..=self.max.y)
            .flat_map(move |y| (self.min.x..=self.max.x).map(move |x| IVec2::new(x, y)))
    }
}

/// The grid coordinate a world coordinate falls in.
// Coordinates beyond the `i32` grid saturate, which is the intended clamp.
#[allow(clippy::cast_possible_truncation)]
fn cell_of(value: f32, cell_size: f32) -> i32 {
    (value / cell_size).floor() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Vec2;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #249: the getters report what was configured and inserted.
    #[wasm_bindgen_test(unsupported = test)]
    fn getters_report_the_configured_size_and_count() {
        let mut set = ColliderSet::new(2.5);
        assert_eq!(set.cell_size().to_bits(), 2.5f32.to_bits());
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
        set.insert(aabb(0.0, 0.0, 1.0, 1.0), ColliderFlags::SOLID);
        set.insert(aabb(9.0, 9.0, 1.0, 1.0), ColliderFlags::SENSOR);
        assert!(!set.is_empty());
        assert_eq!(set.len(), 2);
        assert_eq!(set.iter().count(), 2);
        assert_eq!((&set).into_iter().count(), 2);
    }

    /// #249: colliders at negative, fractional coordinates with a cell size
    /// that does not divide them are found by a query in their cells, and a
    /// non-finite region finds nothing even with a collider at the origin.
    #[wasm_bindgen_test(unsupported = test)]
    fn negative_fractional_boxes_are_found_and_non_finite_regions_find_nothing() {
        let mut set = ColliderSet::new(1.5);
        let origin = set.insert(aabb(0.0, 0.0, 0.5, 0.5), ColliderFlags::SOLID);
        let low = set.insert(aabb(-2.2, -3.7, 0.4, 0.4), ColliderFlags::SOLID);
        assert_eq!(set.query(&aabb(-2.5, -4.0, 0.2, 0.2)), vec![low]);
        assert_eq!(set.query(&aabb(-5.0, -5.0, 0.2, 0.2)), Vec::<usize>::new());
        assert_eq!(set.query(&aabb(0.2, 0.2, 0.1, 0.1)), vec![origin]);
        assert!(set.query(&aabb(f32::NAN, 0.0, 1.0, 1.0)).is_empty());
        assert!(set.query(&aabb(0.0, 0.0, f32::INFINITY, 1.0)).is_empty());
    }

    fn aabb(cx: f32, cy: f32, hx: f32, hy: f32) -> Aabb {
        Aabb::new(Vec2::new(cx, cy), Vec2::new(hx, hy))
    }

    /// The configured cell size must drive bucketing. With cells far larger
    /// than the colliders, two boxes 50 units apart share a cell and both come
    /// back as candidates; a hard-coded cell size would return only the near
    /// one.
    #[wasm_bindgen_test(unsupported = test)]
    fn configured_cell_size_decides_bucketing() {
        let mut coarse = ColliderSet::new(100.0);
        coarse.insert(aabb(0.0, 0.0, 0.5, 0.5), ColliderFlags::SOLID);
        coarse.insert(aabb(50.0, 0.0, 0.5, 0.5), ColliderFlags::SOLID);
        assert_eq!(coarse.query(&aabb(0.0, 0.0, 1.0, 1.0)), vec![0, 1]);

        let mut fine = ColliderSet::new(1.0);
        fine.insert(aabb(0.0, 0.0, 0.5, 0.5), ColliderFlags::SOLID);
        fine.insert(aabb(50.0, 0.0, 0.5, 0.5), ColliderFlags::SOLID);
        assert_eq!(fine.query(&aabb(0.0, 0.0, 1.0, 1.0)), vec![0]);
    }

    /// A collider wider than one cell must be reachable from every cell it
    /// covers and still appear exactly once, in ascending index order. At a
    /// 0.25 cell size the wide floor occupies dozens of buckets, so a missing
    /// dedupe repeats it and center-only bucketing loses it.
    #[wasm_bindgen_test(unsupported = test)]
    fn multi_cell_colliders_are_returned_once_in_index_order() {
        let mut set = ColliderSet::new(0.25);
        let floor = set.insert(aabb(0.0, 0.0, 1.0, 0.25), ColliderFlags::SOLID);
        let far = set.insert(aabb(10.0, 10.0, 0.5, 0.5), ColliderFlags::SOLID);
        let box_above = set.insert(aabb(0.5, 0.5, 0.3, 0.3), ColliderFlags::SOLID);

        assert_eq!(set.query(&aabb(0.0, 0.0, 1.0, 1.0)), vec![floor, box_above]);
        // Reachable from a region touching only its right end.
        assert_eq!(set.query(&aabb(0.9, -0.2, 0.05, 0.02)), vec![floor]);
        assert_eq!(set.query(&aabb(10.0, 10.0, 0.1, 0.1)), vec![far]);
    }

    /// Clearing must drop the grid too. Colliders alone would leave buckets
    /// holding indices that no longer exist, and the next query would hand a
    /// caller indices past the end of the set.
    #[wasm_bindgen_test(unsupported = test)]
    fn clear_drops_the_grid_with_the_colliders() {
        let mut set = ColliderSet::new(1.0);
        set.insert(aabb(0.0, 0.0, 0.5, 0.5), ColliderFlags::SOLID);
        set.insert(aabb(1.0, 0.0, 0.5, 0.5), ColliderFlags::SOLID);
        set.clear();

        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
        assert!(set.query(&aabb(0.0, 0.0, 2.0, 2.0)).is_empty());

        let reused = set.insert(aabb(0.0, 0.0, 0.5, 0.5), ColliderFlags::SENSOR);
        assert_eq!(reused, 0, "indices restart after a clear");
        assert_eq!(set.query(&aabb(0.0, 0.0, 0.5, 0.5)), vec![0]);
        assert_eq!(
            set.iter().next().map(|c| c.flags),
            Some(ColliderFlags::SENSOR)
        );
    }

    /// #316: a query region spanning 4e12 cells returns the colliders in it
    /// without walking its cells.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_huge_query_region_is_answered_without_walking_its_cells() {
        let mut set = ColliderSet::new(1.0);
        let near = set.insert(aabb(0.0, 0.0, 0.5, 0.5), ColliderFlags::SOLID);
        let far = set.insert(aabb(9e5, -9e5, 0.5, 0.5), ColliderFlags::SOLID);
        set.insert(aabb(3e6, 0.0, 0.5, 0.5), ColliderFlags::SOLID);
        assert_eq!(set.query(&aabb(0.0, 0.0, 1e6, 1e6)), vec![near, far]);
    }

    /// #316: a collider spanning 4e12 cells is inserted without walking its
    /// cells and is still found by a grid query from inside it, and only
    /// there.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_huge_collider_is_found_from_the_cells_it_covers() {
        let mut set = ColliderSet::new(1.0);
        let small = set.insert(aabb(3e6, 0.0, 0.5, 0.5), ColliderFlags::SOLID);
        let block = set.insert(aabb(0.0, 0.0, 1e6, 1e6), ColliderFlags::SOLID);
        // More colliders than a small query's cells, so it walks the grid.
        for i in 0..8u8 {
            set.insert(
                aabb(-3e6, f32::from(i) * 4.0, 0.5, 0.5),
                ColliderFlags::SOLID,
            );
        }
        assert_eq!(set.query(&aabb(5e5, -5e5, 0.5, 0.5)), vec![block]);
        assert_eq!(set.query(&aabb(3e6, 0.0, 0.5, 0.5)), vec![small]);
        set.clear();
        assert!(set.query(&aabb(5e5, -5e5, 0.5, 0.5)).is_empty());
    }

    /// A non-finite box must address no cells. Saturating casts would otherwise
    /// turn an infinite bound into a walk over four billion grid cells.
    #[wasm_bindgen_test(unsupported = test)]
    fn non_finite_regions_address_no_cells() {
        let mut set = ColliderSet::new(1.0);
        set.insert(aabb(0.0, 0.0, 0.5, 0.5), ColliderFlags::SOLID);
        assert!(
            set.query(&aabb(0.0, 0.0, f32::INFINITY, 1.0)).is_empty(),
            "an infinite region must not be walked cell by cell"
        );
        assert!(set.query(&aabb(f32::NAN, 0.0, 1.0, 1.0)).is_empty());
    }
}
