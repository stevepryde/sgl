//! Ranges of a growable buffer, in its units: allocated first fit from the
//! freed ranges, else at the end, and merged with their neighbours when
//! freed, so bounded content keeps a bounded buffer.
use std::ops::Range;

pub(crate) struct Ranges {
    /// Free ranges below `end`, sorted, never adjacent to each other or to
    /// `end`.
    free: Vec<Range<u32>>,
    end: u32,
}

impl Ranges {
    /// Ranges after a reserved `0..start`.
    pub fn new(start: u32) -> Self {
        Self {
            free: Vec::new(),
            end: start,
        }
    }

    /// The free units below the end.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn free_units(&self) -> u64 {
        self.free.iter().map(|range| range.len() as u64).sum()
    }

    /// One past the last allocated unit.
    pub fn end(&self) -> u32 {
        self.end
    }

    /// `len` units, or None when the end would pass `u32::MAX`.
    pub fn allocate(&mut self, len: u32) -> Option<Range<u32>> {
        self.allocate_aligned(len, 1)
    }

    /// `len` units starting at a multiple of `align`, or None when the end
    /// would pass `u32::MAX`. The units an alignment skips stay free. An
    /// empty range holds no unit, wherever it starts.
    pub fn allocate_aligned(&mut self, len: u32, align: u32) -> Option<Range<u32>> {
        if len == 0 {
            return Some(self.end..self.end);
        }
        let aligned = |start: u32| start.checked_next_multiple_of(align);
        if let Some((at, start)) = self.free.iter().enumerate().find_map(|(at, range)| {
            let start = aligned(range.start)?;
            (start.checked_add(len)? <= range.end).then_some((at, start))
        }) {
            let free = self.free.remove(at);
            let after = start + len..free.end;
            if !after.is_empty() {
                self.free.insert(at, after);
            }
            let before = free.start..start;
            if !before.is_empty() {
                self.free.insert(at, before);
            }
            return Some(start..start + len);
        }
        let start = aligned(self.end)?;
        let end = start.checked_add(len)?;
        let skipped = self.end..start;
        if !skipped.is_empty() {
            self.free.push(skipped);
        }
        self.end = end;
        Some(start..end)
    }

    pub fn free(&mut self, range: Range<u32>) {
        if range.is_empty() {
            return;
        }
        let at = self.free.partition_point(|free| free.start < range.start);
        self.free.insert(at, range);
        if at + 1 < self.free.len() && self.free[at].end == self.free[at + 1].start {
            let next = self.free.remove(at + 1);
            self.free[at].end = next.end;
        }
        if at > 0 && self.free[at - 1].end == self.free[at].start {
            let current = self.free.remove(at);
            self.free[at - 1].end = current.end;
        }
        let end = self.end;
        if let Some(last) = self.free.pop_if(|last| last.end == end) {
            self.end = last.start;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Ranges;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Plausible defects: a merge or split that hands out units a live range
    // holds (overlapping content in the ray source), a range that does not
    // start at the multiple asked for (a BLAS reading a model's vertices
    // from the middle of a record), or freed or skipped units that are
    // never reused, so content added and removed repeatedly grows the buffer
    // without bound. The oracles are the live ranges themselves, the
    // alignment each asked for and the reserved start the end must return to
    // once everything is freed.
    #[wasm_bindgen_test(unsupported = test)]
    fn live_ranges_never_overlap_and_freed_units_are_reused() {
        let mut ranges = Ranges::new(4);
        let mut live: Vec<std::ops::Range<u32>> = Vec::new();
        let mut state = 0x9e37_79b9u32;
        let mut random = |bound: u32| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 8) % bound
        };
        let mut high = 0;
        for _ in 0..4000 {
            if live.len() < 40 && random(3) != 0 {
                let align = [1, 3, 8][random(3) as usize];
                let range = ranges.allocate_aligned(random(64), align).unwrap();
                assert!(range.start >= 4 && range.end <= ranges.end());
                assert!(
                    range.is_empty() || range.start.is_multiple_of(align),
                    "{range:?} is not {align}-aligned"
                );
                for other in &live {
                    assert!(
                        range.is_empty()
                            || other.is_empty()
                            || range.end <= other.start
                            || other.end <= range.start,
                        "{range:?} overlaps {other:?}"
                    );
                }
                live.push(range);
            } else if !live.is_empty() {
                let at = random(live.len() as u32) as usize;
                ranges.free(live.swap_remove(at));
            }
            high = high.max(ranges.end());
        }
        // At most 40 ranges of under 64 units, each after at most 7 skipped,
        // live at once.
        assert!(high <= 8 + 40 * (64 + 7), "the end grew to {high}");
        for range in live.drain(..) {
            ranges.free(range);
        }
        assert_eq!(ranges.end(), 4);
    }
}
