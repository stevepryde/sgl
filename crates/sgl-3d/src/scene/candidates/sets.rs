//! The draw sets: what draws with one pipeline and one material, keyed by
//! its material, whether its instances' poses mirror and whether they
//! deform; the pipeline variant follows from those and the material's sides
//! and alpha mode (`view::draw_list::gpu`). Each set holds a region of
//! every GPU-built view's cluster list, placed by `scene::ranges`, of at
//! least the capacity its candidates' sections sum to, and re-placed only
//! when it outgrows it or shrinks to a quarter of it, and one indirect
//! command, at its index in each view's draws. Sets need no scene: what
//! their records take of their material comes with each candidate
//! (`SetLook`), so a refused placement restores a copy of them.
use super::mirror::Mirror;
use crate::content::identity::MaterialId;
use crate::scene::ranges::Ranges;
use crate::shading::culling::{DrawSet, SET_CASTS_DIRECTIONAL_SHADOW};
use rustc_hash::FxHashMap;
use std::ops::Range;

/// What a set's draws share besides their sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SetKey {
    pub material: MaterialId,
    /// Its instances' poses reverse winding.
    pub mirrored: bool,
    /// Its instances deform, so its pulled passes read their deformed
    /// vertices.
    pub deforms: bool,
}

/// What a set's record takes of its material: its visibility group and
/// whether it casts the directional shadow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SetLook {
    pub group: u32,
    pub casts: bool,
}

/// A live set.
#[derive(Clone)]
struct Set {
    key: SetKey,
    look: SetLook,
    /// Its candidates, and the sections they draw at most, each counting
    /// the most among its levels.
    candidates: u32,
    need: u32,
    region: Range<u32>,
}

#[derive(Clone)]
pub(crate) struct Sets {
    sets: Vec<Option<Set>>,
    by_key: FxHashMap<SetKey, u32>,
    /// Indices of ended sets, for reuse.
    free: Vec<u32>,
    records: Mirror<DrawSet>,
    /// Every set's region of each GPU-built view's cluster list, in draw
    /// instances.
    regions: Ranges,
    /// A region could not be placed within `u32`: the sets fit no device.
    overflowed: bool,
}

impl Sets {
    pub fn new() -> Self {
        Self {
            sets: Vec::new(),
            by_key: FxHashMap::default(),
            free: Vec::new(),
            records: Mirror::new("draw sets"),
            regions: Ranges::new(0),
            overflowed: false,
        }
    }

    /// Adds a candidate drawing at most `need` sections to the set of
    /// `key`, whose material looks as `look` says, adding the set when there
    /// is none; returns its index. Its region grows to hold them.
    pub fn add(&mut self, key: SetKey, look: SetLook, need: u32) -> u32 {
        let index = match self.by_key.get(&key) {
            Some(&index) => index,
            None => {
                let index = self.free.pop().unwrap_or(self.sets.len() as u32);
                if index as usize == self.sets.len() {
                    self.sets.push(None);
                }
                self.sets[index as usize] = Some(Set {
                    key,
                    look,
                    candidates: 0,
                    need: 0,
                    region: 0..0,
                });
                self.by_key.insert(key, index);
                index
            }
        };
        let set = self.sets[index as usize].as_mut().unwrap();
        set.candidates += 1;
        set.need += need;
        self.place(index);
        index
    }

    /// Removes a candidate drawing at most `need` sections from set
    /// `index`, ending the set with its last one.
    pub fn remove(&mut self, index: u32, need: u32) {
        let set = self.sets[index as usize]
            .as_mut()
            .expect("a candidate's set lives");
        set.candidates -= 1;
        set.need -= need;
        if set.candidates == 0 {
            let set = self.sets[index as usize].take().unwrap();
            self.regions.free(set.region);
            self.by_key.remove(&set.key);
            self.free.push(index);
            self.records
                .set(index, DrawSet::default(), DrawSet::default());
            return;
        }
        self.place(index);
    }

    /// Re-places set `index`'s region when its candidates outgrew it or
    /// shrank to a quarter of it, with half as much again as they need,
    /// and writes its record.
    fn place(&mut self, index: u32) {
        let set = self.sets[index as usize].as_mut().unwrap();
        let capacity = set.region.len() as u32;
        if set.need > capacity || set.need <= capacity / 4 {
            self.regions.free(set.region.clone());
            let capacity = set.need.saturating_add(set.need / 2);
            set.region = self.regions.allocate(capacity).unwrap_or_else(|| {
                self.overflowed = true;
                0..0
            });
        }
        self.records.set(index, record(set), DrawSet::default());
    }

    /// Rewrites the records of material `id`'s sets, which now looks as
    /// `look` says.
    pub fn material_changed(&mut self, id: MaterialId, look: SetLook) {
        for (index, set) in self.sets.iter_mut().enumerate() {
            if let Some(set) = set.as_mut().filter(|set| set.key.material == id) {
                set.look = look;
                self.records
                    .set(index as u32, record(set), DrawSet::default());
            }
        }
    }

    /// Whether every region lies within `most` draw instances, which each
    /// GPU-built view's cluster list binds.
    pub fn fit(&self, most: u32) -> bool {
        !self.overflowed && self.regions.end() <= most
    }

    /// One past the last draw instance a region holds: each GPU-built
    /// view's cluster list holds that many.
    pub fn region_end(&self) -> u32 {
        self.regions.end()
    }

    /// One past the last set's index: each GPU-built view's draws hold that
    /// many commands.
    pub fn end(&self) -> u32 {
        self.sets.len() as u32
    }

    /// Each live set's index, key and region, in index order.
    pub fn iter(&self) -> impl Iterator<Item = (u32, SetKey, Range<u32>)> + '_ {
        self.sets.iter().enumerate().filter_map(|(index, set)| {
            set.as_ref()
                .map(|set| (index as u32, set.key, set.region.clone()))
        })
    }

    /// The set records' buffer.
    pub fn buffer(&self) -> &wgpu::Buffer {
        self.records.buffer()
    }

    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.records.upload(device, queue);
    }

    #[cfg(any(test, feature = "diagnostics"))]
    pub fn bytes(&self) -> u64 {
        self.records.bytes()
    }
}

/// Set `set`'s record.
fn record(set: &Set) -> DrawSet {
    DrawSet {
        region: set.region.start,
        capacity: set.region.len() as u32,
        visibility_group: set.look.group,
        flags: if set.look.casts {
            SET_CASTS_DIRECTIONAL_SHADOW
        } else {
            0
        },
    }
}
