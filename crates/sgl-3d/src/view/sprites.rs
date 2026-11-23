//! The camera's sprite batches: those with sprites whose bounds meet its
//! clip volume, each with the view depth of its bounds' centre, which
//! places a blended batch among the blended surfaces as Bevy 9d12036's
//! `Transparent3d` key places a mesh (crates/bevy_core_pipeline/src/
//! core_3d/mod.rs; MIT OR Apache-2.0). Built in prepare, beside the blended
//! list, which it splits where a blended batch stands (`blended_depths`).
use super::ViewData;
use super::culling::clip_intersects;
use crate::Scene;
use crate::content::identity::SpriteBatchId;
use crate::content::sprite::SpriteBlend;
use glam::{Mat4, Vec3};

/// One batch the camera draws.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ListedBatch {
    pub id: SpriteBatchId,
    pub masked: bool,
    /// The view-space Z of its bounds' centre (of its sprites' positions
    /// for a batch kept its size on screen): ascending is back to front.
    pub depth: f32,
}

#[derive(Default)]
pub(crate) struct SpriteList {
    /// The masked batches nearest first, then the blended ones back to
    /// front.
    pub batches: Vec<ListedBatch>,
    /// Where the blended ones start in `batches`.
    blended: usize,
    /// What masked batches drew that nothing draws now, meeting the clip
    /// volume: where the masked colour pass marks what they may have left.
    pub footprints: Vec<Mark>,
}

/// A footprint to mark.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Mark {
    /// A batch's last submitted sprites, which an edit since replaced
    /// (`Batch::previous`), whether or not the batch is listed now.
    Previous(SpriteBatchId),
    /// A removed or restyled batch's (`Sprites::departed`, by index).
    Departed(usize),
}

impl SpriteList {
    /// Replaces the list with the batches of `scene` that have sprites and
    /// that `view`'s clip volume meets; a batch kept its size on screen is
    /// always listed, its reach unknown until a view sizes it.
    pub fn build(&mut self, scene: &Scene, view: &ViewData) {
        self.batches.clear();
        self.footprints.clear();
        let clip_from_world = Mat4::from_cols_array_2d(&view.uniform.view_projection);
        let seen = |bounds: Option<[Vec3; 2]>| {
            bounds.is_none_or(|bounds| clip_intersects(bounds, clip_from_world))
        };
        for (index, (footprint, _)) in scene.sprites.departed.iter().enumerate() {
            if seen(footprint.bounds) {
                self.footprints.push(Mark::Departed(index));
            }
        }
        if scene.sprites.batches.is_empty() {
            self.blended = 0;
            return;
        }
        let view_from_world = Mat4::from_cols_array_2d(&view.uniform.view);
        for (id, batch) in scene.sprites.batches.iter() {
            let masked = matches!(batch.look.blend, SpriteBlend::Masked { .. });
            if masked
                && batch
                    .previous()
                    .is_some_and(|previous| seen(previous.bounds))
            {
                self.footprints.push(Mark::Previous(id));
            }
            if batch.sprites().is_empty() || batch.group().is_none() {
                continue;
            }
            let centre = match batch.bounds() {
                Some(bounds) if !clip_intersects(bounds, clip_from_world) => continue,
                Some([min, max]) => (min + max) * 0.5,
                None => {
                    batch
                        .sprites()
                        .iter()
                        .map(|sprite| sprite.position)
                        .sum::<Vec3>()
                        / batch.sprites().len() as f32
                }
            };
            self.batches.push(ListedBatch {
                id,
                masked,
                depth: view_from_world.transform_point3(centre).z,
            });
        }
        // Masked batches first, nearest first, as opaque draws are ordered
        // for the depth test; then blended ones back to front.
        self.batches.sort_by(|a, b| {
            b.masked.cmp(&a.masked).then(if a.masked {
                b.depth.total_cmp(&a.depth)
            } else {
                a.depth.total_cmp(&b.depth)
            })
        });
        self.blended = self.batches.partition_point(|batch| batch.masked);
    }

    pub fn masked(&self) -> &[ListedBatch] {
        &self.batches[..self.blended]
    }

    pub fn blended(&self) -> &[ListedBatch] {
        &self.batches[self.blended..]
    }

    /// The blended batches' depths, back to front: where the blended list
    /// must not merge two draws, so each batch can be drawn between them.
    pub fn blended_depths(&self) -> impl Iterator<Item = f32> + '_ {
        self.blended().iter().map(|batch| batch.depth)
    }
}
