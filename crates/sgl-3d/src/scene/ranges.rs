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

    /// One past the last allocated unit.
    pub fn end(&self) -> u32 {
        self.end
    }

    /// `len` units, or None when the end would pass `u32::MAX`.
    pub fn allocate(&mut self, len: u32) -> Option<Range<u32>> {
        if let Some(at) = self
            .free
            .iter()
            .position(|range| range.len() >= len as usize)
        {
            let start = self.free[at].start;
            self.free[at].start += len;
            if self.free[at].is_empty() {
                self.free.remove(at);
            }
            return Some(start..start + len);
        }
        let start = self.end;
        self.end = start.checked_add(len)?;
        Some(start..self.end)
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
    // holds (overlapping content in the ray source), or freed units that are
    // never reused, so content added and removed repeatedly grows the buffer
    // without bound. The oracles are the live ranges themselves and the
    // reserved start the end must return to once everything is freed.
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
                let range = ranges.allocate(random(64)).unwrap();
                assert!(range.start >= 4 && range.end <= ranges.end());
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
        // At most 40 ranges of under 64 units live at once.
        assert!(high <= 4 + 40 * 64, "the end grew to {high}");
        for range in live.drain(..) {
            ranges.free(range);
        }
        assert_eq!(ranges.end(), 4);
    }
}
