//! The two spaces the crate runs in: the sealed [`Vector`] trait over `Vec2`
//! and `Vec3`, its integer cells, and the few vector operations the crate
//! uses, each written out so its arithmetic and its order are the crate's own.

use core::cmp::Ordering;
use core::fmt::Debug;
use core::hash::Hash;
use core::ops::{Add, Index, IndexMut, Mul, Neg, Sub};

use sgl_core::StateHasher;
use sgl_core::math::{IVec2, IVec3, Vec2, Vec3};

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Vec2 {}
    impl Sealed for super::Vec3 {}
}

/// A position, motion or size in 2D (`Vec2`) or 3D (`Vec3`).
///
/// Every type and function in the crate is generic over it, so a 2D and a 3D
/// game run the same code. It is sealed: the crate implements it for those
/// two types only. Axis `0` is x, `1` is y and `2` is z.
pub trait Vector:
    sealed::Sealed
    + Copy
    + Debug
    + Default
    + PartialEq
    + Send
    + Sync
    + 'static
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<f32, Output = Self>
    + Neg<Output = Self>
    + Index<usize, Output = f32>
    + IndexMut<usize>
{
    /// How many axes the space has: 2 or 3.
    const AXES: usize;
    /// The zero vector.
    const ZERO: Self;
    /// A whole-numbered cell of a grid over this space: `IVec2` or `IVec3`.
    type Cell: Copy
        + Debug
        + Default
        + Eq
        + Hash
        + Send
        + Sync
        + 'static
        + Index<usize, Output = i32>
        + IndexMut<usize>;
}

impl Vector for Vec2 {
    const AXES: usize = 2;
    const ZERO: Self = Vec2::ZERO;
    type Cell = IVec2;
}

impl Vector for Vec3 {
    const AXES: usize = 3;
    const ZERO: Self = Vec3::ZERO;
    type Cell = IVec3;
}

/// The vector with every component `value`.
pub(crate) fn splat<V: Vector>(value: f32) -> V {
    let mut out = V::ZERO;
    for axis in 0..V::AXES {
        out[axis] = value;
    }
    out
}

/// The unit vector along `axis`, pointing toward `+axis` when `positive`.
pub(crate) fn unit<V: Vector>(axis: usize, positive: bool) -> V {
    let mut out = V::ZERO;
    out[axis] = if positive { 1.0 } else { -1.0 };
    out
}

/// Whether every component is finite.
pub(crate) fn is_finite<V: Vector>(v: V) -> bool {
    (0..V::AXES).all(|axis| v[axis].is_finite())
}

/// The dot product of `v` with the direction `d`, summed in axis order over
/// the axes where `d` is not exactly zero. An axis `d` does not use adds
/// nothing, not even the sign of a zero, so a 3D result along a direction
/// in the xy plane is bit-equal to the 2D one.
pub(crate) fn dot<V: Vector>(v: V, d: V) -> f32 {
    let mut sum: Option<f32> = None;
    for axis in 0..V::AXES {
        if d[axis] != 0.0 {
            let term = v[axis] * d[axis];
            sum = Some(sum.map_or(term, |sum| sum + term));
        }
    }
    sum.unwrap_or(0.0)
}

/// `v` with the part along the unit direction `d` removed (`v - d (v · d)`),
/// leaving exactly unchanged every axis `d` does not use.
pub(crate) fn reject<V: Vector>(v: V, d: V) -> V {
    let along = dot(v, d);
    let mut out = v;
    for axis in 0..V::AXES {
        if d[axis] != 0.0 {
            out[axis] = v[axis] - d[axis] * along;
        }
    }
    out
}

/// The smaller of `a` and `b` by comparison alone, never a platform minimum.
pub(crate) fn min(a: f32, b: f32) -> f32 {
    if b < a { b } else { a }
}

/// The larger of `a` and `b` by comparison alone, never a platform maximum.
pub(crate) fn max(a: f32, b: f32) -> f32 {
    if b > a { b } else { a }
}

/// Component-wise [`min`].
pub(crate) fn min_each<V: Vector>(a: V, b: V) -> V {
    let mut out = a;
    for axis in 0..V::AXES {
        out[axis] = min(a[axis], b[axis]);
    }
    out
}

/// Component-wise [`max`].
pub(crate) fn max_each<V: Vector>(a: V, b: V) -> V {
    let mut out = a;
    for axis in 0..V::AXES {
        out[axis] = max(a[axis], b[axis]);
    }
    out
}

/// The largest absolute component.
pub(crate) fn largest_abs<V: Vector>(v: V) -> f32 {
    (0..V::AXES).fold(0.0, |largest, axis| max(largest, v[axis].abs()))
}

/// Orders two vectors component by component, x first, by each float's
/// total order.
pub(crate) fn compare<V: Vector>(a: V, b: V) -> Ordering {
    (0..V::AXES)
        .map(|axis| a[axis].total_cmp(&b[axis]))
        .find(|order| order.is_ne())
        .unwrap_or(Ordering::Equal)
}

/// Writes the vector's components in axis order, each as its IEEE-754 bits.
pub(crate) fn write<V: Vector>(out: &mut StateHasher, v: V) {
    for axis in 0..V::AXES {
        out.u32(v[axis].to_bits());
    }
}

/// The largest postcard encoding of one vector: four bytes a component.
pub(crate) const fn postcard_bytes<V: Vector>() -> usize {
    4 * V::AXES
}

/// The inclusive box of grid cells a region meets, at least one cell along
/// every axis.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CellRange<V: Vector> {
    pub min: V::Cell,
    pub max: V::Cell,
}

impl<V: Vector> CellRange<V> {
    /// The cells the closed box `min..=max` meets at `cell_size`, or `None`
    /// when a corner is not finite (a float-to-integer cast saturates, so an
    /// infinite corner would address the whole `i32` grid) or the box is
    /// inverted. Coordinates beyond the `i32` grid saturate to its edge.
    pub fn of_corners(min: V, max: V, cell_size: f32) -> Option<Self> {
        if !(is_finite(min) && is_finite(max)) {
            return None;
        }
        let mut range = Self {
            min: V::Cell::default(),
            max: V::Cell::default(),
        };
        for axis in 0..V::AXES {
            range.min[axis] = cell_of(min[axis], cell_size);
            range.max[axis] = cell_of(max[axis], cell_size);
            if range.min[axis] > range.max[axis] {
                return None;
            }
        }
        Some(range)
    }

    /// How many cells the range holds, saturating at `u64::MAX`.
    pub fn count(&self) -> u64 {
        (0..V::AXES).fold(1u64, |count, axis| {
            let span = i64::from(self.max[axis]) - i64::from(self.min[axis]) + 1;
            count.saturating_mul(u64::try_from(span).unwrap_or(0))
        })
    }

    /// How many cells the two ranges share.
    pub fn shared(&self, other: &Self) -> u64 {
        (0..V::AXES).fold(1u64, |count, axis| {
            let lo = i64::from(self.min[axis]).max(i64::from(other.min[axis]));
            let hi = i64::from(self.max[axis]).min(i64::from(other.max[axis]));
            count.saturating_mul(u64::try_from(hi - lo + 1).unwrap_or(0))
        })
    }

    /// Whether `cell` lies in the range.
    pub fn contains(&self, cell: V::Cell) -> bool {
        (0..V::AXES).all(|axis| self.min[axis] <= cell[axis] && cell[axis] <= self.max[axis])
    }

    /// Whether the two ranges share a cell.
    pub fn meets(&self, other: &Self) -> bool {
        (0..V::AXES)
            .all(|axis| self.min[axis] <= other.max[axis] && other.min[axis] <= self.max[axis])
    }

    /// Calls `visit` with every cell in the range, x fastest.
    pub fn for_each(&self, mut visit: impl FnMut(V::Cell)) {
        let mut cell = self.min;
        loop {
            visit(cell);
            let mut axis = 0;
            loop {
                if axis == V::AXES {
                    return;
                }
                if cell[axis] < self.max[axis] {
                    cell[axis] += 1;
                    break;
                }
                cell[axis] = self.min[axis];
                axis += 1;
            }
        }
    }
}

/// The grid coordinate a world coordinate falls in.
// Coordinates beyond the `i32` grid saturate, which is the intended clamp.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn cell_of(value: f32, cell_size: f32) -> i32 {
    (value / cell_size).floor() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// The dot product leaves out the axes the direction does not use, so a
    /// zero along an unused axis never turns a `-0.0` result into `+0.0`.
    /// Including the term would: `-0.0 + 0.0` is `+0.0` in IEEE-754.
    #[wasm_bindgen_test(unsupported = test)]
    fn an_unused_axis_never_changes_a_dot_product() {
        let v2 = Vec2::new(-0.0, 0.0);
        let v3 = Vec3::new(-0.0, 0.0, 0.0);
        let d2 = Vec2::new(1.0, 0.0);
        let d3 = Vec3::new(1.0, 0.0, 0.0);
        assert_eq!(dot(v2, d2).to_bits(), (-0.0f32).to_bits());
        assert_eq!(dot(v3, d3).to_bits(), (-0.0f32).to_bits());
        assert_eq!(
            dot(Vec3::new(2.0, 3.0, 4.0), Vec3::new(0.0, -1.0, 0.5)).to_bits(),
            (-1.0f32).to_bits()
        );
    }

    /// Every cell of a range is visited once: a 3 x 2 x 2 range holds twelve
    /// cells, the count says so, and none repeats.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_cell_range_visits_each_of_its_cells_once() {
        let range =
            CellRange::<Vec3>::of_corners(Vec3::new(-1.5, 0.0, 2.0), Vec3::new(0.5, 1.0, 3.5), 1.0)
                .expect("finite");
        let mut cells = Vec::new();
        range.for_each(|cell| cells.push(cell));
        assert_eq!(range.count(), 12);
        assert_eq!(cells.len(), 12);
        cells.sort_by_key(|c| (c.x, c.y, c.z));
        cells.dedup();
        assert_eq!(cells.len(), 12);
        assert_eq!(cells[0], IVec3::new(-2, 0, 2));
        assert_eq!(cells[11], IVec3::new(0, 1, 3));
    }
}
