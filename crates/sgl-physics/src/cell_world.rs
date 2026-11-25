//! A world the game answers cell by cell: a tile map or a voxel grid that
//! cannot be copied into a set.

use core::marker::PhantomData;

use crate::shape::Collider;
use crate::space::{self, CellRange, Vector};
use crate::world::{Unanswered, World};

/// Most cells a [`CellWorld`] walks for one query. A query whose region
/// meets more is [`Unanswered`]: the body moves nothing and the caller is
/// told. 4096 cells is a 16-cell cube in 3D (a reach of about 15 cells on
/// the diagonal) and a 64-cell square in 2D.
pub const MAX_QUERY_CELLS: u64 = 4096;

/// Most steps a swept walk takes, one per cell its motion crosses along its
/// longest axis: a motion crossing more meets more than [`MAX_QUERY_CELLS`]
/// cells anyway.
#[allow(clippy::cast_precision_loss)]
const MAX_STEPS: f32 = MAX_QUERY_CELLS as f32;

/// Largest cell coordinate, in cells, a query can address: the `i32` grid.
const GRID_LIMIT: f32 = 2_147_483_648.0;

/// A tile or voxel grid the game answers one cell at a time.
///
/// The game gives the cell size and a function from an integer cell to that
/// cell's colliders, which it pushes into a [`CellColliders`] in cell-local
/// units: a cell runs from `0` to `1` along each axis, so a full block is a
/// box of half-extents `0.5` at `0.5`, and a half-height slab one of
/// half-extents `(0.5, 0.25, 0.5)` at `(0.5, 0.25, 0.5)`. SGL converts them
/// to world units (`(cell + local) * cell_size`) and walks the cells a
/// query's region meets, or, for a moving query, the cells along its
/// motion. A collider must lie within its cell: one that reaches past it can
/// be missed by a query that does not walk its cell.
///
/// What an unloaded cell holds is the game's policy: it pushes a full box to
/// keep bodies out, or nothing to let them through. A query walks at most
/// [`MAX_QUERY_CELLS`] cells and answers [`Unanswered`] past that.
pub struct CellWorld<V: Vector, F> {
    cell_size: f32,
    cells: F,
    space: PhantomData<fn() -> V>,
}

impl<V, F> CellWorld<V, F>
where
    V: Vector,
    F: Fn(V::Cell, &mut CellColliders<'_, V>),
{
    /// A grid of cells `cell_size` world units across, whose colliders
    /// `cells` pushes for each cell asked about.
    ///
    /// # Panics
    /// If `cell_size` is not positive and finite: an authoring error.
    #[must_use]
    pub fn new(cell_size: f32, cells: F) -> Self {
        assert!(
            cell_size > 0.0 && cell_size.is_finite(),
            "CellWorld: cell size must be positive and finite, got {cell_size}"
        );
        Self {
            cell_size,
            cells,
            space: PhantomData,
        }
    }

    /// World units across one cell.
    #[must_use]
    pub fn cell_size(&self) -> f32 {
        self.cell_size
    }
}

impl<V: Vector, F> core::fmt::Debug for CellWorld<V, F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CellWorld")
            .field("cell_size", &self.cell_size)
            .finish_non_exhaustive()
    }
}

/// Where a [`CellWorld`]'s function puts one cell's colliders.
pub struct CellColliders<'a, V: Vector> {
    corner: V,
    cell_size: f32,
    visit: &'a mut dyn FnMut(&Collider<V>),
}

impl<V: Vector> CellColliders<'_, V> {
    /// Adds a collider given in cell-local units (the cell runs from `0` to
    /// `1` along each axis). Its id, flags and layers pass through unchanged.
    pub fn push(&mut self, collider: Collider<V>) {
        let world = Collider {
            position: (self.corner + collider.position) * self.cell_size,
            shape: collider.shape.scaled(self.cell_size),
            ..collider
        };
        (self.visit)(&world);
    }
}

impl<V, F> CellWorld<V, F>
where
    V: Vector,
    F: Fn(V::Cell, &mut CellColliders<'_, V>),
{
    /// How far to grow a region before walking it, on every axis.
    ///
    /// A world must offer every collider whose box, grown by its own
    /// rounding allowance `4ε(|position| + half)`, meets the region
    /// ([`World`]). A cell world cannot grow its colliders before it walks,
    /// since it learns them cell by cell, so it grows the region instead. A
    /// collider lies within its cell, and a cell that meets the grown region
    /// lies within a cell of it, so `|position| + half` is at most the
    /// region's largest coordinate plus a cell or so: without this a
    /// collider in a large cell whose face sits on the cell's boundary, met
    /// by a body near the origin, is left out by a region a rounding step
    /// short of that boundary. The factor 8 also covers the rounding of the
    /// steps of a swept walk.
    fn allowance(&self, min: V, max: V) -> V {
        let largest = space::max(space::largest_abs(min), space::largest_abs(max));
        space::splat(8.0 * f32::EPSILON * (largest + self.cell_size))
    }

    /// The cells the closed box `min..=max` meets, a cell whose upper face it
    /// starts on included; `None` when the box is inverted, [`Unanswered`]
    /// when it reaches past the `i32` grid or is not finite.
    fn range(&self, min: V, max: V) -> Result<Option<CellRange<V>>, Unanswered> {
        let mut range = CellRange::<V> {
            min: V::Cell::default(),
            max: V::Cell::default(),
        };
        for axis in 0..V::AXES {
            let lo = -(-min[axis] / self.cell_size).floor() - 1.0;
            let hi = (max[axis] / self.cell_size).floor();
            if !(lo.abs() < GRID_LIMIT && hi.abs() < GRID_LIMIT) {
                return Err(Unanswered);
            }
            if lo > hi {
                return Ok(None);
            }
            range.min[axis] = cell_index(lo);
            range.max[axis] = cell_index(hi);
        }
        Ok(Some(range))
    }

    /// Asks the game for one cell's colliders.
    fn ask(&self, cell: V::Cell, visit: &mut dyn FnMut(&Collider<V>)) {
        let mut corner = V::ZERO;
        for axis in 0..V::AXES {
            corner[axis] = cell_coordinate(cell[axis]);
        }
        (self.cells)(
            cell,
            &mut CellColliders {
                corner,
                cell_size: self.cell_size,
                visit,
            },
        );
    }
}

impl<V, F> World<V> for CellWorld<V, F>
where
    V: Vector,
    F: Fn(V::Cell, &mut CellColliders<'_, V>),
{
    /// Walks every cell the closed box `min..=max`, grown by the rounding
    /// allowance of the colliders it may hold, meets, its boundaries
    /// included: a region starting exactly on a cell's upper face walks that
    /// cell too.
    fn colliders_near(
        &self,
        min: V,
        max: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered> {
        let grow = self.allowance(min, max);
        let Some(range) = self.range(min - grow, max + grow)? else {
            return Ok(());
        };
        if range.count() > MAX_QUERY_CELLS {
            return Err(Unanswered);
        }
        range.for_each(|cell| self.ask(cell, &mut *visit));
        Ok(())
    }

    /// Walks the cells along the motion, each once: the motion is cut into
    /// as many equal steps as it crosses cells along its longest axis, so a
    /// step moves the box at most one cell along every axis, and each step
    /// walks the cells its swept box meets that the step before did not.
    /// Only the cells walked count against [`MAX_QUERY_CELLS`], so a long
    /// ray is answered where the box enclosing it would not be.
    fn colliders_along(
        &self,
        min: V,
        max: V,
        motion: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), Unanswered> {
        let (lo, hi) = (
            space::min_each(min, min + motion),
            space::max_each(max, max + motion),
        );
        let grow = self.allowance(lo, hi);
        if self.range(lo - grow, hi + grow)?.is_none() {
            return Ok(());
        }
        // Ceiling by floor alone: `-floor(-x)`.
        let steps = -(-space::largest_abs(motion) / self.cell_size).floor();
        if steps.is_nan() || steps > MAX_STEPS {
            return Err(Unanswered);
        }
        let steps = cell_index(space::max(steps, 1.0));
        // The swept box of step `i`, from `i / steps` of the motion to
        // `(i + 1) / steps`. Consecutive steps share an end computed by the
        // same expression, and the last ends at `motion` exactly.
        let step = |i: i32| -> Result<Option<CellRange<V>>, Unanswered> {
            let (from, to) = (
                motion * (cell_coordinate(i) / cell_coordinate(steps)),
                motion * (cell_coordinate(i + 1) / cell_coordinate(steps)),
            );
            let (a, b) = (min + from, min + to);
            let (c, d) = (max + from, max + to);
            self.range(space::min_each(a, b) - grow, space::max_each(c, d) + grow)
        };
        // Count first, so a walk over the cap asks about no cell. Along each
        // axis a step's range never moves back, so a cell an earlier step
        // walked and this one meets lies in the step just before.
        let mut walked = 0u64;
        let mut before: Option<CellRange<V>> = None;
        for i in 0..steps {
            let range = step(i)?;
            if let Some(range) = range {
                let seen = before.map_or(0, |before| range.shared(&before));
                walked = walked.saturating_add(range.count() - seen);
                if walked > MAX_QUERY_CELLS {
                    return Err(Unanswered);
                }
            }
            before = range;
        }
        let mut before: Option<CellRange<V>> = None;
        for i in 0..steps {
            let range = step(i)?;
            if let Some(range) = range {
                range.for_each(|cell| {
                    if !before.is_some_and(|before| before.contains(cell)) {
                        self.ask(cell, &mut *visit);
                    }
                });
            }
            before = range;
        }
        Ok(())
    }
}

/// A whole cell coordinate already checked to lie inside the `i32` grid.
#[allow(clippy::cast_possible_truncation)]
fn cell_index(whole: f32) -> i32 {
    whole as i32
}

/// A cell coordinate as a float, rounded to nearest past 2^24 cells as every
/// target rounds it.
#[allow(clippy::cast_precision_loss)]
fn cell_coordinate(cell: i32) -> f32 {
    cell as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::Shape;
    use sgl_core::math::{IVec2, Vec2, Vec3};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn visited<V: Vector, F: Fn(V::Cell, &mut CellColliders<'_, V>)>(
        world: &CellWorld<V, F>,
        min: V,
        max: V,
    ) -> Result<Vec<Collider<V>>, Unanswered> {
        let mut found = Vec::new();
        world.colliders_near(min, max, &mut |c| found.push(*c))?;
        Ok(found)
    }

    /// A cell's colliders come back in world units: a half-height slab in
    /// cell (2, -1) of a 0.5-unit grid spans x 1.0..1.5 and y -0.5..-0.25,
    /// worked by hand from `(cell + local) * size`.
    #[wasm_bindgen_test(unsupported = test)]
    fn cell_local_colliders_convert_to_world_units() {
        let world = CellWorld::new(0.5, |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
            if cell == IVec2::new(2, -1) {
                out.push(Collider::new(
                    9,
                    Shape::Box {
                        half: Vec2::new(0.5, 0.25),
                    },
                    Vec2::new(0.5, 0.25),
                ));
            }
        });
        let found = visited(&world, Vec2::new(1.1, -0.4), Vec2::new(1.2, -0.3)).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, 9);
        assert_eq!(found[0].position, Vec2::new(1.25, -0.375));
        assert_eq!(found[0].shape.half_extents(), Vec2::new(0.25, 0.125));
    }

    /// A region that starts exactly on a cell's upper face still walks that
    /// cell, so a body resting against a block's face meets the block: the
    /// closed-box contract.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_region_on_a_cell_face_walks_the_cell_below_it() {
        let world = CellWorld::new(1.0, |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
            out.push(Collider::new(
                u64::from(cell.x.unsigned_abs()),
                Shape::Box {
                    half: Vec2::splat(0.5),
                },
                Vec2::splat(0.5),
            ));
        });
        let found = visited(&world, Vec2::new(3.0, 0.2), Vec2::new(3.5, 0.4)).unwrap();
        let mut ids: Vec<u64> = found.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![2, 3]);
    }

    /// A region over the cap is refused before any cell is asked about; one
    /// at the cap is walked. 4096 cells is 16 x 16 x 16.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_region_over_the_cap_is_unanswered_without_asking() {
        let asked = core::cell::Cell::new(0u64);
        let world = CellWorld::new(
            1.0,
            |_: sgl_core::math::IVec3, _: &mut CellColliders<'_, Vec3>| {
                asked.set(asked.get() + 1);
            },
        );
        let at_cap = visited(&world, Vec3::splat(0.5), Vec3::splat(15.5));
        assert!(at_cap.is_ok());
        assert_eq!(asked.get(), 4096);
        asked.set(0);
        let over = visited(&world, Vec3::splat(0.5), Vec3::new(15.5, 15.5, 16.5));
        assert_eq!(over, Err(Unanswered));
        assert_eq!(asked.get(), 0);
        let far = visited(&world, Vec3::splat(-1e30), Vec3::splat(-1e30));
        assert_eq!(far, Err(Unanswered));
        let infinite = visited(&world, Vec3::ZERO, Vec3::new(f32::INFINITY, 0.0, 0.0));
        assert_eq!(infinite, Err(Unanswered));
    }

    /// A ray crossing (60, 3, 70) cells is answered: its enclosing box holds
    /// 61 x 4 x 71 cells, far over the cap, but the walk along it counts
    /// only the few hundred it passes. Its hit on a wall of blocks at z = 68
    /// equals, bit for bit, the hit on the same blocks given as a slice. A
    /// ray of 5000 cells walks more than the cap and is still unanswered.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_long_ray_is_walked_along_its_path() {
        let wall = |cell: sgl_core::math::IVec3| {
            cell.z == 68 && (0..64).contains(&cell.x) && (0..6).contains(&cell.y)
        };
        let block = |cell: sgl_core::math::IVec3, at: Vec3| {
            Collider::new(
                u64::from(cell.x.unsigned_abs()) << 8 | u64::from(cell.y.unsigned_abs()),
                Shape::Box {
                    half: Vec3::splat(0.5),
                },
                at,
            )
        };
        let cells = CellWorld::new(
            1.0,
            |cell: sgl_core::math::IVec3, out: &mut CellColliders<'_, Vec3>| {
                if wall(cell) {
                    out.push(block(cell, Vec3::splat(0.5)));
                }
            },
        );
        let mut slice = Vec::new();
        for x in 0..64 {
            for y in 0..6 {
                let cell = sgl_core::math::IVec3::new(x, y, 68);
                slice.push(block(cell, cell.as_vec3() + Vec3::splat(0.5)));
            }
        }
        let (origin, motion) = (Vec3::splat(0.5), Vec3::new(60.0, 3.0, 70.0));
        let through_cells = crate::ray(origin, motion, &cells, u32::MAX).unwrap();
        let through_slice = crate::ray(origin, motion, slice.as_slice(), u32::MAX).unwrap();
        let hit = through_slice.expect("the wall is met");
        assert_eq!(hit.normal, Vec3::NEG_Z);
        let bits = |h: crate::Hit<Vec3>| (h.t.to_bits(), h.normal, h.id);
        assert_eq!(through_cells.map(bits), Some(bits(hit)));
        let far = crate::ray(origin, Vec3::new(5000.0, 0.0, 0.0), &cells, u32::MAX);
        assert_eq!(far, Err(Unanswered));
    }
}
