//! Deterministic row-major grids.

use crate::{CanonicalWrite, StateHasher};

/// An invalid grid construction request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridError {
    /// The supplied cells did not equal `width * height`.
    IncorrectCellCount,
}

/// A row-major grid with fixed-width dimensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grid2<T> {
    width: u16,
    height: u16,
    cells: Vec<T>,
}

impl<T> Grid2<T> {
    /// Constructs a grid when `cells` exactly matches its checked dimensions.
    pub fn from_cells(width: u16, height: u16, cells: Vec<T>) -> Result<Self, GridError> {
        let expected = Self::cell_count(width, height);
        if cells.len() != expected {
            return Err(GridError::IncorrectCellCount);
        }
        Ok(Self {
            width,
            height,
            cells,
        })
    }

    /// Returns the grid width.
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// Returns the grid height.
    #[must_use]
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Returns a cell by coordinate.
    #[must_use]
    pub fn get(&self, x: u16, y: u16) -> Option<&T> {
        self.cells.get(self.index(x, y)?)
    }

    /// Returns a mutable cell by coordinate.
    #[must_use]
    pub fn get_mut(&mut self, x: u16, y: u16) -> Option<&mut T> {
        let index = self.index(x, y)?;
        self.cells.get_mut(index)
    }

    /// Returns one row as a contiguous slice.
    #[must_use]
    pub fn row(&self, y: u16) -> Option<&[T]> {
        let start = self.index(0, y)?;
        self.cells
            .get(start..start.checked_add(usize::from(self.width))?)
    }

    /// Returns all cells in deterministic row-major order.
    #[must_use]
    pub fn cells(&self) -> &[T] {
        &self.cells
    }

    fn cell_count(width: u16, height: u16) -> usize {
        let count = u32::from(width)
            .checked_mul(u32::from(height))
            .expect("u16 grid dimensions have a representable product");
        usize::try_from(count).expect("u16 grid dimensions fit usize")
    }

    // This is the sole coordinate-to-linear conversion in this type.
    fn index(&self, x: u16, y: u16) -> Option<usize> {
        if y >= self.height || (x >= self.width && !(self.width == 0 && x == 0)) {
            return None;
        }
        usize::try_from(u32::from(y) * u32::from(self.width) + u32::from(x)).ok()
    }
}

impl<T: Clone> Grid2<T> {
    /// Constructs a grid filled with clones of `value`.
    #[must_use]
    pub fn filled(width: u16, height: u16, value: T) -> Self {
        Self {
            width,
            height,
            cells: vec![value; Self::cell_count(width, height)],
        }
    }
}

impl<T: CanonicalWrite> CanonicalWrite for Grid2<T> {
    fn canonical_write(&self, out: &mut StateHasher) {
        out.u16(self.width);
        out.u16(self.height);
        out.sequence(&self.cells);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #249: the dimension getters report the constructed size.
    #[wasm_bindgen_test(unsupported = test)]
    fn dimensions_are_reported() {
        let grid = Grid2::filled(3, 5, 0u8);
        assert_eq!((grid.width(), grid.height()), (3, 5));
        assert_eq!(grid.cells().len(), 15);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn construction_and_access_preserve_row_major_invariants() {
        let mut grid = Grid2::from_cells(3, 2, vec![0, 1, 2, 3, 4, 5]).expect("matching shape");
        assert_eq!(grid.get(2, 1), Some(&5));
        assert_eq!(grid.row(1), Some(&[3, 4, 5][..]));
        *grid.get_mut(1, 0).expect("in bounds") = 9;
        assert_eq!(grid.cells(), &[0, 9, 2, 3, 4, 5]);
        assert_eq!(grid.get(3, 0), None);
        assert_eq!(grid.get(0, 2), None);
        assert_eq!(
            Grid2::from_cells(2, 2, vec![1, 2, 3]),
            Err(GridError::IncorrectCellCount)
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn dimensions_have_checked_widened_size_arithmetic() {
        assert_eq!(Grid2::<()>::cell_count(u16::MAX, u16::MAX), 4_294_836_225);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn zero_width_rows_are_valid_and_empty() {
        let grid = Grid2::from_cells(0, 2, Vec::<u8>::new()).expect("zero-width shape");
        assert_eq!(grid.row(0), Some(&[][..]));
        assert_eq!(grid.row(1), Some(&[][..]));
        assert_eq!(grid.row(2), None);
        assert_eq!(grid.get(0, 0), None);
    }
}
