//! Who holds which part of the local-light shadow atlas. A port of Godot
//! b130438's servers/rendering/renderer_rd/storage_rd/light_storage.cpp
//! (`shadow_atlas_update_light`, `_shadow_atlas_find_shadow`,
//! `_shadow_atlas_find_omni_shadows`, `_shadow_atlas_invalidate_shadow` and
//! `shadow_atlas_set_quadrant_subdivision`'s size order), MIT
//! (src/LICENSE-godot.txt).
//!
//! The square atlas is four quadrants, each split into square power-of-two
//! slots. A light asks for the slot size its screen coverage wants and
//! takes free slots, or the least recently seen ones of lights not yet seen
//! this frame, searching from the largest size that fits down. It keeps its
//! slots until it wants another size, and then moves only once it has held
//! them for `REALLOCATION_FRAMES`, nor loses them to another light before
//! then. A light takes as many consecutive slots of one quadrant as it has
//! faces: Godot's omni lights take two, one per paraboloid; a cube takes six,
//! one per face, as Wicked Engine lays a point light's faces side by side.
//!
//! Divergences from Godot:
//! - Godot's ticks are wall-clock milliseconds; these are the stage's
//!   frames, and the tolerance is Godot's 500 ms at 60 frames per second, so
//!   it lasts longer in time at a lower frame rate. SGL3D reads no clock it
//!   is not given, and `FrameInput`'s elapsed time is the game's, which may
//!   stop or be left unset.
//! - Godot's default quadrants hold 4, 4, 16 and 64 slots; these hold 16,
//!   64, 256 and 1024, so dozens of cubes fit.
//! - A light whose face count changes (a spot that widens past one face, or
//!   a point light that becomes a narrow spot) moves at once, and gives up
//!   its slots if it finds none; Godot's lights never change kind.
use crate::content::identity::LightId;
use std::collections::HashMap;

/// The atlas's width and height in texels.
pub(crate) const ATLAS_SIZE: u32 = 4096;
/// Each quadrant's slots per side: slots of 512, 256, 128 and 64 texels,
/// room for over two hundred cubes.
const SUBDIVISIONS: [u32; 4] = [4, 8, 16, 32];
/// Frames a light holds its slots before it may move to another size, and
/// before another light may take them.
const REALLOCATION_FRAMES: u64 = 30;
// Every quadrant holds a cube's six faces.
const _: () = assert!(SUBDIVISIONS[0] * SUBDIVISIONS[0] >= 6);

#[derive(Clone, Copy, Default)]
struct Slot {
    owner: Option<LightId>,
    /// The frame it was allocated.
    allocated: u64,
}

/// A light's slots: `count` consecutive slots of `quadrant` from `first`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Allocation {
    quadrant: usize,
    first: usize,
    count: usize,
}

/// Where a light's faces are: one slot each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Placement {
    quadrant: usize,
    first: usize,
    /// A slot's width and height in texels.
    pub size: u32,
}

impl Placement {
    /// Face `face`'s slot among every slot of the atlas.
    pub fn cell(&self, face: usize) -> usize {
        cell_offset(self.quadrant) + self.first + face
    }

    /// Face `face`'s top-left texel.
    pub fn origin(&self, face: usize) -> [u32; 2] {
        let subdivision = SUBDIVISIONS[self.quadrant] as usize;
        let slot = self.first + face;
        let quadrant = ATLAS_SIZE / 2;
        [
            (self.quadrant as u32 & 1) * quadrant + (slot % subdivision) as u32 * self.size,
            (self.quadrant as u32 >> 1) * quadrant + (slot / subdivision) as u32 * self.size,
        ]
    }
}

/// The index of `quadrant`'s first slot among every slot of the atlas.
fn cell_offset(quadrant: usize) -> usize {
    SUBDIVISIONS[..quadrant]
        .iter()
        .map(|subdivision| (subdivision * subdivision) as usize)
        .sum()
}

/// Every slot of the atlas.
pub(crate) fn cell_count() -> usize {
    cell_offset(SUBDIVISIONS.len())
}

pub(crate) struct Atlas {
    quadrants: [Vec<Slot>; 4],
    /// Quadrants by subdivision, finest first.
    size_order: [usize; 4],
    owners: HashMap<LightId, Allocation>,
    /// The frame each light was last seen (Godot's `last_scene_pass`).
    seen: HashMap<LightId, u64>,
}

impl Default for Atlas {
    fn default() -> Self {
        let mut size_order = [0, 1, 2, 3];
        size_order.sort_by_key(|&quadrant| std::cmp::Reverse(SUBDIVISIONS[quadrant]));
        Self {
            quadrants: SUBDIVISIONS
                .map(|subdivision| vec![Slot::default(); (subdivision * subdivision) as usize]),
            size_order,
            owners: HashMap::new(),
            seen: HashMap::new(),
        }
    }
}

impl Atlas {
    /// The placement of `light`'s `faces` slots for its screen `coverage`
    /// (its screen diameter over the view's half extents, as Godot's) at
    /// `frame`, marking it seen; `None` when the atlas has no room for it.
    /// A light the atlas places again at another size or place loses what
    /// it drew.
    pub fn update(
        &mut self,
        light: LightId,
        coverage: f32,
        faces: usize,
        frame: u64,
    ) -> Option<Placement> {
        self.seen.insert(light, frame);
        let quadrant_size = ATLAS_SIZE >> 1;
        let smallest_subdivision = *SUBDIVISIONS.iter().min().unwrap();
        let wanted = (quadrant_size as f32 * coverage.max(0.)).min(u32::MAX as f32) as u32;
        let desired_fit = (quadrant_size / smallest_subdivision).min(wanted.next_power_of_two());
        // The quadrants it fits, and the best size among them.
        let mut valid = Vec::with_capacity(4);
        let mut best_size = None;
        let mut best_subdivision = None;
        for &quadrant in &self.size_order {
            let subdivision = SUBDIVISIONS[quadrant];
            let max_fit = quadrant_size / subdivision;
            if best_size.is_some_and(|best| max_fit > best) {
                break;
            }
            valid.push(quadrant);
            best_subdivision = Some(subdivision);
            if max_fit >= desired_fit {
                best_size = Some(max_fit);
            }
        }
        let best_subdivision = best_subdivision?;
        let old = self.owners.get(&light).copied();
        let mut old_subdivision = None;
        if let Some(old) = old {
            let held = frame.saturating_sub(self.quadrants[old.quadrant][old.first].allocated);
            let realloc = old.count != faces
                || (SUBDIVISIONS[old.quadrant] != best_subdivision && held > REALLOCATION_FRAMES);
            if !realloc {
                return Some(self.placement(old));
            }
            if old.count == faces {
                old_subdivision = Some(SUBDIVISIONS[old.quadrant]);
            }
        }
        let Some((quadrant, first)) = self.find(&valid, faces, old_subdivision, frame) else {
            return match old {
                Some(old) if old.count == faces => Some(self.placement(old)),
                Some(old) => {
                    self.release(light, old);
                    None
                }
                None => None,
            };
        };
        if let Some(old) = old {
            self.release(light, old);
        }
        for slot in first..first + faces {
            if let Some(owner) = self.quadrants[quadrant][slot].owner {
                self.invalidate(owner);
            }
        }
        for slot in &mut self.quadrants[quadrant][first..first + faces] {
            *slot = Slot {
                owner: Some(light),
                allocated: frame,
            };
        }
        let allocation = Allocation {
            quadrant,
            first,
            count: faces,
        };
        self.owners.insert(light, allocation);
        Some(self.placement(allocation))
    }

    /// Forgets the lights `live` rejects, lights the scene removed, and
    /// frees their slots.
    pub fn retain(&mut self, live: impl Fn(LightId) -> bool) {
        let ended: Vec<_> = self
            .owners
            .iter()
            .filter(|(light, _)| !live(**light))
            .map(|(&light, &allocation)| (light, allocation))
            .collect();
        for (light, allocation) in ended {
            self.release(light, allocation);
        }
        self.seen.retain(|&light, _| live(light));
    }

    fn placement(&self, allocation: Allocation) -> Placement {
        Placement {
            quadrant: allocation.quadrant,
            first: allocation.first,
            size: (ATLAS_SIZE >> 1) / SUBDIVISIONS[allocation.quadrant],
        }
    }

    /// `faces` consecutive slots in one of `quadrants`, searched from the
    /// last (largest) down, stopping at a quadrant of the light's current
    /// size (`current`): free ones first, else those whose owners were seen
    /// least recently, among owners not seen at `frame` and holding their
    /// slots for longer than `REALLOCATION_FRAMES`.
    fn find(
        &self,
        quadrants: &[usize],
        faces: usize,
        current: Option<u32>,
        frame: u64,
    ) -> Option<(usize, usize)> {
        for &quadrant in quadrants.iter().rev() {
            if current == Some(SUBDIVISIONS[quadrant]) {
                return None;
            }
            let slots = &self.quadrants[quadrant];
            let mut found = None;
            let mut min_pass = 0;
            'window: for first in 0..=slots.len() - faces {
                let mut pass = 0;
                for slot in &slots[first..first + faces] {
                    let Some(owner) = slot.owner else {
                        continue;
                    };
                    let seen = self.seen.get(&owner).copied().unwrap_or(0);
                    if seen == frame || frame.saturating_sub(slot.allocated) < REALLOCATION_FRAMES {
                        continue 'window;
                    }
                    pass += seen + 1;
                }
                if found.is_none() || pass < min_pass {
                    found = Some(first);
                    min_pass = pass;
                    if pass == 0 {
                        break;
                    }
                }
            }
            if let Some(first) = found {
                return Some((quadrant, first));
            }
        }
        None
    }

    /// Frees `light`'s slots.
    fn release(&mut self, light: LightId, allocation: Allocation) {
        let slots = &mut self.quadrants[allocation.quadrant];
        for slot in &mut slots[allocation.first..allocation.first + allocation.count] {
            if slot.owner == Some(light) {
                *slot = Slot::default();
            }
        }
        self.owners.remove(&light);
    }

    /// Takes every slot from `light`, whose slots another light takes.
    fn invalidate(&mut self, light: LightId) {
        if let Some(allocation) = self.owners.get(&light).copied() {
            self.release(light, allocation);
        }
    }
}

#[cfg(test)]
mod tests;
