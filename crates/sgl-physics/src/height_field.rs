//! A world of ground given as a grid of heights: terrain, a road, a swell.
//!
//! The grid's layout and its cells' two triangles follow `Box3D` v0.1.0
//! `src/height_field.c` (MIT, Erin Catto; `PROVENANCE.md`): heights row by
//! row, a row along x and rows along z, and the cell triangles of
//! `b3GetHeightFieldCellCorners` and `b3QueryHeightField`. The walk is the
//! one a [`CellWorld`](crate::CellWorld) makes, over the grid's columns.

use core::fmt;

use sgl_core::math::{IVec2, Vec2, Vec3};

use crate::cell_world::Grid;
use crate::shape::Collider;
use crate::space::{self, CellRange};
use crate::surface::{SurfaceError, Triangle, box_meets_along, boxes_meet};
use crate::world::{Unanswered, World};

/// Most height samples one [`HeightField`] holds: 2048 x 2048, 16 MiB of
/// heights.
pub const MAX_HEIGHT_SAMPLES: usize = 1 << 22;

/// Ground as a grid of heights on the xz plane, y up, built once: a world a
/// ray, a cast, an overlap or [`ground_under`](crate::ground_under) can ask.
///
/// `columns` samples run along x and `rows` along z, `cell` apart; sample
/// `(column, row)` is `heights[row * columns + column]` and stands at
/// `origin + (column * cell, height, row * cell)`. Each cell between four
/// samples is two triangles split along the diagonal from its `(column + 1,
/// row)` corner to its `(column, row + 1)` corner: `(column, row)`,
/// `(column, row + 1)`, `(column + 1, row)` and `(column + 1, row + 1)`,
/// `(column + 1, row)`, `(column, row + 1)`, as `Box3D` splits them. Both
/// carry the cell's id: one per cell (cell `(column, row)` is
/// `ids[row * (columns - 1) + column]`) or one for all.
///
/// Each triangle is offered to a query as a solid convex collider of its
/// three corners, as a [`TriangleSet`](crate::TriangleSet) offers them, met
/// exactly by a ray and through the convex kernel by every other shape, and
/// two-sided. A query walks the cells its
/// region or path covers, at most [`MAX_QUERY_CELLS`](crate::MAX_QUERY_CELLS)
/// of them, as a [`CellWorld`](crate::CellWorld) does; cells outside the
/// grid are not walked or counted. Outside the grid there is no ground.
#[derive(Clone)]
pub struct HeightField {
    heights: Vec<f32>,
    columns: usize,
    rows: usize,
    cell: f32,
    origin: Vec3,
    ids: Vec<u64>,
    /// The largest magnitude an x or z coordinate of the grid's reaches:
    /// what the walk's region is grown by the rounding of.
    span: f32,
}

impl HeightField {
    /// The grid of `columns` by `rows` samples `heights`, `cell` apart from
    /// `origin`, with `ids` one per cell or one for all.
    ///
    /// # Errors
    /// [`SurfaceError::Grid`] for fewer than two samples along an axis, a
    /// sample count other than `columns * rows`, a cell size that is not
    /// positive, or cells too small to tell apart at `origin` (two samples
    /// at the same x or z); [`SurfaceError::TooMany`] for more than
    /// [`MAX_HEIGHT_SAMPLES`]; [`SurfaceError::NotFinite`] when a height,
    /// the cell size, the origin or a sample's position is not finite;
    /// [`SurfaceError::Ids`] when `ids` is neither one id nor one per cell.
    pub fn new(
        heights: &[f32],
        columns: usize,
        rows: usize,
        cell: f32,
        origin: Vec3,
        ids: &[u64],
    ) -> Result<Self, SurfaceError> {
        if columns < 2 || rows < 2 || columns.checked_mul(rows) != Some(heights.len()) {
            return Err(SurfaceError::Grid);
        }
        if heights.len() > MAX_HEIGHT_SAMPLES {
            return Err(SurfaceError::TooMany);
        }
        if !(cell.is_finite() && space::is_finite(origin)) {
            return Err(SurfaceError::NotFinite);
        }
        if cell <= 0.0 {
            return Err(SurfaceError::Grid);
        }
        if !heights.iter().all(|&h| (origin.y + h).is_finite()) {
            return Err(SurfaceError::NotFinite);
        }
        let cells = (columns - 1) * (rows - 1);
        if !(ids.len() == 1 || ids.len() == cells) {
            return Err(SurfaceError::Ids);
        }
        // Every sample apart from the one before it along each axis.
        let apart = |start: f32, count: usize| -> Result<(), SurfaceError> {
            let mut before = start;
            for i in 1..count {
                let at = start + coordinate(i) * cell;
                if !at.is_finite() {
                    return Err(SurfaceError::NotFinite);
                }
                if at <= before {
                    return Err(SurfaceError::Grid);
                }
                before = at;
            }
            Ok(())
        };
        apart(origin.x, columns)?;
        apart(origin.z, rows)?;
        let span = origin.x.abs() + origin.z.abs() + coordinate(columns + rows) * cell;
        Ok(Self {
            heights: heights.to_vec(),
            columns,
            rows,
            cell,
            origin,
            ids: ids.to_vec(),
            span,
        })
    }

    /// The sample `(column, row)`'s position.
    fn sample(&self, column: usize, row: usize) -> Vec3 {
        Vec3::new(
            self.origin.x + coordinate(column) * self.cell,
            self.origin.y + self.heights[row * self.columns + column],
            self.origin.z + coordinate(row) * self.cell,
        )
    }

    /// The cell's two triangles (`b3QueryHeightField`'s order), either left
    /// out when `f32` sees no area in it (a cliff far steeper than the cell
    /// is wide).
    fn triangles(&self, cell: IVec2) -> [Option<Triangle>; 2] {
        let (Ok(column), Ok(row)) = (usize::try_from(cell.x), usize::try_from(cell.y)) else {
            return [None, None];
        };
        let index = row * (self.columns - 1) + column;
        let id = if self.ids.len() == 1 {
            self.ids[0]
        } else {
            self.ids[index]
        };
        let c11 = self.sample(column, row);
        let c12 = self.sample(column + 1, row);
        let c21 = self.sample(column, row + 1);
        let c22 = self.sample(column + 1, row + 1);
        [
            Triangle::new(c11, c21, c12, id),
            Triangle::new(c22, c12, c21, id),
        ]
    }

    /// The walk over the grid's cells, in coordinates about its origin.
    fn grid(&self) -> Grid<Vec2> {
        Grid {
            cell_size: self.cell,
            within: Some(CellRange {
                min: IVec2::ZERO,
                max: IVec2::new(cell_number(self.columns - 2), cell_number(self.rows - 2)),
            }),
        }
    }

    /// The region `min..=max`, grown by the rounding of the grid's
    /// coordinates and of `largest`, the largest the query's reach, on the
    /// xz plane about the origin: so a triangle whose box meets the region
    /// lies in a cell the walk of this region meets, however the samples'
    /// positions round.
    fn local(&self, min: Vec3, max: Vec3, largest: f32) -> (Vec2, Vec2) {
        let grow = 8.0 * f32::EPSILON * (largest + self.span);
        let (lo, hi) = (min - Vec3::splat(grow), max + Vec3::splat(grow));
        (
            Vec2::new(lo.x - self.origin.x, lo.z - self.origin.z),
            Vec2::new(hi.x - self.origin.x, hi.z - self.origin.z),
        )
    }

    /// Offers each triangle of `cell` whose box `meets` accepts.
    fn offer(
        &self,
        cell: IVec2,
        meets: impl Fn(Vec3, Vec3) -> bool,
        visit: &mut dyn FnMut(&Collider<Vec3>),
    ) {
        for triangle in self.triangles(cell).into_iter().flatten() {
            let (lo, hi) = triangle.reach();
            if meets(lo, hi) {
                visit(&triangle.collider());
            }
        }
    }
}

impl fmt::Debug for HeightField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeightField")
            .field("columns", &self.columns)
            .field("rows", &self.rows)
            .field("cell", &self.cell)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl World<Vec3> for HeightField {
    /// Walks the cells the closed box `min..=max` covers on the xz plane
    /// (`b3QueryHeightField`) and offers each of their triangles whose box,
    /// grown by its rounding allowance, meets the box. [`Unanswered`] when
    /// the region covers more than
    /// [`MAX_QUERY_CELLS`](crate::MAX_QUERY_CELLS) of the grid's cells or is
    /// not finite.
    fn colliders_near(
        &self,
        min: Vec3,
        max: Vec3,
        visit: &mut dyn FnMut(&Collider<Vec3>),
    ) -> Result<(), Unanswered> {
        if !(space::is_finite(min) && space::is_finite(max)) {
            return Err(Unanswered);
        }
        let largest = space::max(space::largest_abs(min), space::largest_abs(max));
        let (lo, hi) = self.local(min, max, largest);
        self.grid().near(lo, hi, |cell| {
            self.offer(cell, |a, b| boxes_meet(a, b, min, max), visit);
        })
    }

    /// Walks the cells along the motion, each once, as a
    /// [`CellWorld`](crate::CellWorld) walks its cells, and offers each of
    /// their triangles whose box the swept box meets. [`Unanswered`] when
    /// the walk covers more than [`MAX_QUERY_CELLS`](crate::MAX_QUERY_CELLS)
    /// of the grid's cells, crosses more cells along its longest axis than
    /// that, or is not finite.
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
        let largest = [min, max, min + motion, max + motion]
            .into_iter()
            .fold(0.0, |largest, v| space::max(largest, space::largest_abs(v)));
        let (lo, hi) = self.local(min, max, largest);
        let along = Vec2::new(motion.x, motion.z);
        self.grid().along(lo, hi, along, |cell| {
            self.offer(cell, |a, b| box_meets_along(a, b, min, max, motion), visit);
        })
    }
}

/// A sample or cell count below [`MAX_HEIGHT_SAMPLES`] as a coordinate:
/// exact, as it is below 2^24.
#[allow(clippy::cast_precision_loss)]
fn coordinate(count: usize) -> f32 {
    count as f32
}

/// A cell number below [`MAX_HEIGHT_SAMPLES`] as the walk's integer.
fn cell_number(count: usize) -> i32 {
    i32::try_from(count).unwrap_or(i32::MAX)
}
