//! Merging a draw list's per-instance draws into instanced batches, as Bevy
//! 9d12036 batches its render phases (crates/bevy_render/src/batching/,
//! MIT OR Apache-2.0): draws of equal geometry, material, pipeline variant
//! and mobility that draw the same index ranges become one draw of their
//! instances. `bin` merges them wherever they are in the walk, as Bevy's
//! binned phases do (`batch_and_prepare_binned_render_phase`), and
//! `merge_adjacent` only where they follow one another, as its sorted
//! phases do (`batch_and_prepare_sorted_render_phase`).
use super::{DrawBatch, Geometry};
use crate::Mobility;
use crate::content::identity::{MaterialId, ModelId};
use crate::shading::vertex::DrawInstance;
use crate::view::pipelines::Variant;
use rustc_hash::FxHashMap;
use std::ops::Range;

/// What draws must share to be one instanced draw, besides their index
/// ranges.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct BatchKey {
    pub variant: Variant,
    pub material: MaterialId,
    pub geometry: Geometry,
    pub mobility: Mobility,
}

/// One instance's draw of one mesh, before merging.
pub(super) struct InstanceDraw {
    pub key: BatchKey,
    /// Its index ranges in its list's ranges.
    pub ranges: Range<usize>,
    pub instance: DrawInstance,
    /// Where its bin goes among the batches: its instance's model's rank
    /// (`Batcher::rank`), then the mesh's index in that model.
    pub order: (u32, u32),
}

/// Draws merged into one batch while binning.
struct Bin {
    key: BatchKey,
    ranges: Range<usize>,
    order: (u32, u32),
    /// The next bin with the same `BinKey` and other ranges.
    next: Option<u32>,
    /// Its draws, then the next free place among its instances.
    count: u32,
    /// Its first instance in the list's instances.
    start: u32,
}

/// A bin's lookup key: its batch key, first index range and range count.
/// Bins that share it are chained and told apart by their ranges.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BinKey {
    key: BatchKey,
    first: (u32, u32),
    ranges: u32,
}

/// A list's ranges, and the batches and their instances merging writes.
pub(super) type Merged<'a> = (
    &'a [Range<u32>],
    &'a mut Vec<DrawBatch>,
    &'a mut Vec<DrawInstance>,
);

/// One build's draws, and what merging them needs, kept between builds for
/// their capacity.
#[derive(Default)]
pub(super) struct Batcher {
    draws: Vec<InstanceDraw>,
    /// Each draw's bin, in draw order.
    bin_of: Vec<u32>,
    bins: Vec<Bin>,
    bin_keys: FxHashMap<BinKey, u32>,
    /// Each walked model's rank, in the order the walk first met it.
    ranks: FxHashMap<ModelId, u32>,
}

impl Batcher {
    pub fn clear(&mut self) {
        self.draws.clear();
        self.ranks.clear();
    }

    /// The rank of `model` among the models of the instances walked so far.
    pub fn rank(&mut self, model: ModelId) -> u32 {
        let next = self.ranks.len() as u32;
        *self.ranks.entry(model).or_insert(next)
    }

    pub fn push(&mut self, draw: InstanceDraw) {
        self.draws.push(draw);
    }

    /// Orders the draws by `key`, ascending and stably.
    pub fn sort_by_depth(&mut self, key: impl Fn(&InstanceDraw) -> f32) {
        let mut keyed: Vec<_> = self
            .draws
            .drain(..)
            .map(|draw| (key(&draw), draw))
            .collect();
        keyed.sort_by(|(a, _), (b, _)| a.total_cmp(b));
        self.draws.extend(keyed.into_iter().map(|(_, draw)| draw));
    }

    /// Merges each draw into the batch before it where they are equal and
    /// draw one index range, so the draws keep their order: a batch draws
    /// each range for all its instances before the next range, which would
    /// interleave the instances of draws of several ranges.
    pub fn merge_adjacent(&self, (ranges, batches, instances): Merged<'_>) {
        for draw in &self.draws {
            let index = instances.len() as u32;
            instances.push(draw.instance);
            if let Some(last) = batches.last_mut()
                && last.key == draw.key
                && draw.ranges.len() == 1
                && ranges[last.ranges.clone()] == ranges[draw.ranges.clone()]
            {
                last.instances.end = index + 1;
                continue;
            }
            batches.push(batch(draw.key, draw.ranges.clone(), index..index + 1));
        }
    }

    /// Merges equal draws wherever they are. A bin's instances keep the
    /// walk's order, and bins are ordered by their first draw's model rank
    /// and mesh, so each instance's meshes draw in their model's order, as
    /// a draw per instance drew them.
    pub fn bin(&mut self, (ranges, batches, instances): Merged<'_>) {
        self.bins.clear();
        self.bin_keys.clear();
        self.bin_of.clear();
        for draw in &self.draws {
            let drawn = &ranges[draw.ranges.clone()];
            let lookup = BinKey {
                key: draw.key,
                first: (drawn[0].start, drawn[0].end),
                ranges: drawn.len() as u32,
            };
            let created = self.bins.len() as u32;
            let mut at = *self.bin_keys.entry(lookup).or_insert(created);
            if at != created {
                // Equal keys and first ranges: find the bin with these ranges.
                loop {
                    let bin = &self.bins[at as usize];
                    if ranges[bin.ranges.clone()] == *drawn {
                        break;
                    }
                    match bin.next {
                        Some(next) => at = next,
                        None => {
                            self.bins[at as usize].next = Some(created);
                            at = created;
                            break;
                        }
                    }
                }
            }
            if at == created {
                self.bins.push(Bin {
                    key: draw.key,
                    ranges: draw.ranges.clone(),
                    order: draw.order,
                    next: None,
                    count: 0,
                    start: 0,
                });
            }
            self.bins[at as usize].count += 1;
            self.bin_of.push(at);
        }
        let mut order: Vec<u32> = (0..self.bins.len() as u32).collect();
        order.sort_unstable_by_key(|&bin| (self.bins[bin as usize].order, bin));
        let mut start = 0;
        for &bin in &order {
            let bin = &mut self.bins[bin as usize];
            bin.start = start;
            start += bin.count;
            bin.count = 0;
        }
        instances.resize(
            start as usize,
            DrawInstance {
                object: 0,
                mesh: 0,
                first_vertex: 0,
            },
        );
        for (draw, &bin) in self.draws.iter().zip(&self.bin_of) {
            let bin = &mut self.bins[bin as usize];
            instances[(bin.start + bin.count) as usize] = draw.instance;
            bin.count += 1;
        }
        batches.extend(order.iter().map(|&bin| {
            let bin = &self.bins[bin as usize];
            batch(
                bin.key,
                bin.ranges.clone(),
                bin.start..bin.start + bin.count,
            )
        }));
    }
}

/// A batch of `key`'s draws of `ranges` for `instances`.
fn batch(key: BatchKey, ranges: Range<usize>, instances: Range<u32>) -> DrawBatch {
    DrawBatch {
        key,
        ranges,
        instances,
    }
}
