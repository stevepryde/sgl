//! Shelf-packed texture atlas placement (AR-6).
//!
//! The game's sprites are small (≤384 px, PR-11), so the renderer packs them
//! into shared atlas pages and the common case is one draw call per z-run.
//! Large textures (512×512 light cookies, the 1920×1080 title) stay
//! standalone — see the size gate in `render::sprite`.
//!
//! This module is the **pure packing logic** (unit-testable, no wgpu): a
//! classic shelf packer. Sprites are placed left-to-right on horizontal
//! shelves; a sprite opens a new shelf when no existing shelf fits it. Each
//! placement is padded by [`PADDING`] on the right/bottom so nearest-sampled
//! edges never bleed a neighbor.

/// Transparent gap in pixels kept to the right/below every placed rect.
pub const PADDING: u32 = 1;

/// One horizontal shelf: a row of placed rects sharing a y-band.
#[derive(Debug)]
struct Shelf {
    y: u32,
    height: u32,
    /// Next free x on this shelf.
    cursor_x: u32,
}

/// An incremental shelf packer over a fixed `width × height` page.
#[derive(Debug)]
pub struct ShelfPacker {
    width: u32,
    height: u32,
    shelves: Vec<Shelf>,
    /// Top of the unused region below the last shelf.
    next_y: u32,
}

impl ShelfPacker {
    /// A packer for an empty page of the given pixel size.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            shelves: Vec::new(),
            next_y: 0,
        }
    }

    /// Place a `w × h` rect. Returns the top-left corner, or `None` if the
    /// page cannot fit it (the caller opens a new page). The [`PADDING`] is
    /// reserved internally; returned positions are the rect's own pixels.
    pub fn insert(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w == 0 || h == 0 || w > self.width || h > self.height {
            return None;
        }
        let (pw, ph) = (w + PADDING, h + PADDING);

        // First shelf the rect fits on. A shelf accepts heights within
        // [height/2, height] so tall shelves aren't wasted on tiny sprites.
        for shelf in &mut self.shelves {
            if ph <= shelf.height && ph * 2 >= shelf.height && shelf.cursor_x + pw <= self.width {
                let pos = (shelf.cursor_x, shelf.y);
                shelf.cursor_x += pw;
                return Some(pos);
            }
        }

        // Open a new shelf below the last one.
        if self.next_y + ph <= self.height && pw <= self.width {
            let shelf = Shelf {
                y: self.next_y,
                height: ph,
                cursor_x: pw,
            };
            let pos = (0, self.next_y);
            self.next_y += ph;
            self.shelves.push(shelf);
            return Some(pos);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #255: a rect exactly the page size fits once (with its padding
    /// reaching past the edge tolerated only for the rect itself), one
    /// pixel over does not, and a second shelf opens only when the padded
    /// height still fits.
    #[wasm_bindgen_test(unsupported = test)]
    fn page_sized_rects_and_padded_shelf_heights_are_exact() {
        let mut packer = ShelfPacker::new(16, 16);
        assert_eq!(packer.insert(17, 1), None);
        assert_eq!(packer.insert(1, 17), None);
        assert_eq!(packer.insert(0, 4), None);
        // The padding counts against the page, so the widest rect that fits
        // is one pixel short of the page width.
        assert_eq!(packer.insert(16, 7), None);
        assert_eq!(packer.insert(15, 7), Some((0, 0)));
        // The first shelf is 8 px tall with padding; a second 7 px rect fits
        // below (8 + 8 = 16), a third does not.
        assert_eq!(packer.insert(15, 7), Some((0, 8)));
        assert_eq!(packer.insert(1, 7), None);
    }

    /// Same-height sprites fill a shelf left to right, padded.
    #[wasm_bindgen_test(unsupported = test)]
    fn same_height_rects_share_a_shelf() {
        let mut p = ShelfPacker::new(128, 128);
        assert_eq!(p.insert(32, 32), Some((0, 0)));
        assert_eq!(p.insert(32, 32), Some((33, 0)), "1 px padding gap");
        assert_eq!(p.insert(32, 32), Some((66, 0)));
    }

    /// A taller sprite opens a new shelf below instead of stretching one.
    #[wasm_bindgen_test(unsupported = test)]
    fn taller_rect_opens_a_new_shelf() {
        let mut p = ShelfPacker::new(128, 128);
        assert_eq!(p.insert(32, 32), Some((0, 0)));
        assert_eq!(p.insert(32, 64), Some((0, 33)), "below the 33-high shelf");
        // Another 32-high sprite returns to the first shelf.
        assert_eq!(p.insert(32, 32), Some((33, 0)));
    }

    /// A much shorter sprite does NOT ride a tall shelf (waste gate): it
    /// opens its own.
    #[wasm_bindgen_test(unsupported = test)]
    fn tiny_rect_does_not_waste_a_tall_shelf() {
        let mut p = ShelfPacker::new(256, 256);
        assert_eq!(p.insert(64, 64), Some((0, 0)));
        // 8+1 = 9 < 65/2 → rejected from the 65-high shelf.
        assert_eq!(p.insert(8, 8), Some((0, 65)));
    }

    /// A full shelf row wraps to a new shelf.
    #[wasm_bindgen_test(unsupported = test)]
    fn full_shelf_wraps() {
        let mut p = ShelfPacker::new(70, 128);
        assert_eq!(p.insert(32, 32), Some((0, 0)));
        assert_eq!(p.insert(32, 32), Some((33, 0)));
        // 66 + 33 > 70 → next shelf.
        assert_eq!(p.insert(32, 32), Some((0, 33)));
    }

    /// Overflow (page exhausted) and oversized rects return `None`.
    #[wasm_bindgen_test(unsupported = test)]
    fn overflow_and_oversize_return_none() {
        let mut p = ShelfPacker::new(64, 64);
        assert!(p.insert(63, 63).is_some());
        assert_eq!(p.insert(32, 32), None, "page is spent");
        let mut q = ShelfPacker::new(64, 64);
        assert_eq!(q.insert(65, 8), None, "wider than the page");
        assert_eq!(q.insert(0, 8), None, "degenerate");
    }

    /// Everything the packer returns stays inside the page and no two rects
    /// overlap (a randomized soak of the invariant).
    #[wasm_bindgen_test(unsupported = test)]
    fn placements_never_overlap_or_escape() {
        let mut p = ShelfPacker::new(256, 256);
        let sizes = [
            (32, 32),
            (16, 16),
            (48, 24),
            (32, 32),
            (8, 8),
            (96, 32),
            (32, 32),
            (64, 64),
            (24, 48),
            (32, 32),
        ];
        let mut placed: Vec<(u32, u32, u32, u32)> = Vec::new();
        for (w, h) in sizes {
            if let Some((x, y)) = p.insert(w, h) {
                assert!(x + w <= 256 && y + h <= 256, "escaped the page");
                for &(px, py, pw, ph) in &placed {
                    let disjoint = x >= px + pw || px >= x + w || y >= py + ph || py >= y + h;
                    assert!(disjoint, "({x},{y},{w},{h}) overlaps ({px},{py},{pw},{ph})");
                }
                placed.push((x, y, w, h));
            }
        }
        assert!(placed.len() >= 8, "packer should fit most of the set");
    }
}
