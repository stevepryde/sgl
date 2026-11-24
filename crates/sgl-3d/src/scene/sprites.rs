//! Sprites: the images sprite batches show, each uploaded once as sRGB
//! colour with its mips, and the batches, each its look, its sprites as the
//! game gave them and, once it has sprites, its GPU records and group, with
//! the `Scene` operations that add, change and remove them. A scene without
//! sprites holds no layout, buffer or texture for them, and a batch without
//! sprites holds no buffer.
use super::slots::Slots;
use super::{Scene, SceneError};
use crate::asset::{Image, Sampler};
use crate::content::identity::{SpriteBatchId, SpriteImageId};
use crate::content::sprite::{
    Sprite, SpriteBatch, SpriteBlend, SpriteSizing, batch_valid, sprite_valid,
};
use crate::shading::sprite::{SpriteBatchUniform, SpriteRecord, group};
use glam::Vec3;
use std::collections::HashMap;

const RECORD: u64 = std::mem::size_of::<SpriteRecord>() as u64;

/// An image batches show, and how many batches show it.
struct SpriteImage {
    view: wgpu::TextureView,
    users: usize,
}

/// One of a batch's sets of sprites: the game's sprites, their bounds,
/// their records and the group that binds them with the batch's look,
/// image and sampler, made when it first holds sprites, and which edit of
/// the batch's sprites it holds (`Batch::sprites_revision`).
#[derive(Default)]
struct Slot {
    sprites: Vec<Sprite>,
    bounds: Option<[Vec3; 2]>,
    records: Option<wgpu::Buffer>,
    group: Option<wgpu::BindGroup>,
    content: u64,
}

/// The last submitted frame's sprites of a batch, where an edit since
/// replaced them: their group, count and bounds, for the masked colour
/// pass to mark where they stood.
pub(crate) struct Previous<'a> {
    pub group: &'a wgpu::BindGroup,
    pub count: u32,
    pub bounds: Option<[Vec3; 2]>,
}

/// One batch. An edit of its sprites writes a slot that neither the last
/// submitted frame nor a frame rendered and not yet finished drew, so their
/// sprites stay on the GPU while the game changes them, for the marks
/// (`previous`): two slots, and a third only for an edit made between a
/// frame's submission and its `finish_frame`.
pub(crate) struct Batch {
    pub look: SpriteBatch,
    slots: Vec<Slot>,
    /// The slot this frame draws.
    current: usize,
    /// The look's uniform, once a slot holds sprites.
    uniform: Option<wgpu::Buffer>,
    /// Change with every edit that changes its look, and its sprites.
    look_revision: u64,
    sprites_revision: u64,
    /// The revisions the frame being rendered draws, and the last submitted
    /// frame's (`Scene::finish_frame`); none before the batch was in one.
    rendered: Option<(u64, u64)>,
    submitted: Option<(u64, u64)>,
}

impl Batch {
    /// Its sprites as the game gave them, in the render frame.
    pub fn sprites(&self) -> &[Sprite] {
        &self.slots[self.current].sprites
    }

    /// Its sprites' world bounds while it is sized in metres and has any:
    /// the box of every corner any view can give them.
    pub fn bounds(&self) -> Option<[Vec3; 2]> {
        self.slots[self.current].bounds
    }

    /// The group its draws bind; none while it never held sprites.
    pub fn group(&self) -> Option<&wgpu::BindGroup> {
        self.slots[self.current].group.as_ref()
    }

    /// Whether its look or sprites changed since the last submitted frame,
    /// or it was in none.
    pub fn changed(&self) -> bool {
        self.submitted != Some(self.revision())
    }

    /// Changes with every edit that changes its look or sprites.
    pub fn revision(&self) -> (u64, u64) {
        (self.look_revision, self.sprites_revision)
    }

    /// The slot holding edit `content` of its sprites, with sprites.
    fn holding(&self, content: u64) -> Option<&Slot> {
        self.slots
            .iter()
            .find(|slot| slot.content == content && !slot.sprites.is_empty())
    }

    /// The last submitted frame's sprites, where an edit since replaced
    /// them.
    pub fn previous(&self) -> Option<Previous<'_>> {
        let (_, content) = self.submitted?;
        if content == self.sprites_revision {
            return None;
        }
        let slot = self.holding(content)?;
        Some(Previous {
            group: slot.group.as_ref().expect("a drawn slot has its group"),
            count: slot.sprites.len() as u32,
            bounds: slot.bounds,
        })
    }

    /// The sets of sprites the last submitted frame, and a frame rendered
    /// and not yet finished, drew of it under `look`, where that was
    /// masked.
    fn drawn_masked(&self, look: &SpriteBatch) -> Vec<&Slot> {
        if !matches!(look.blend, SpriteBlend::Masked { .. }) {
            return Vec::new();
        }
        let mut contents: Vec<u64> = [self.submitted, self.rendered]
            .into_iter()
            .flatten()
            .map(|(_, content)| content)
            .collect();
        contents.dedup();
        contents
            .into_iter()
            .filter_map(|content| self.holding(content))
            .collect()
    }

    /// The slot an edit of its sprites writes: the current one unless a
    /// submitted or rendered frame drew it, else one neither drew, made
    /// where there is none.
    fn writable(&mut self) -> usize {
        let drawn: Vec<u64> = [self.submitted, self.rendered]
            .into_iter()
            .flatten()
            .map(|(_, content)| content)
            .collect();
        let free = |slot: &Slot| !drawn.contains(&slot.content);
        if free(&self.slots[self.current]) {
            return self.current;
        }
        if let Some(index) = self.slots.iter().position(free) {
            return index;
        }
        self.slots.push(Slot {
            content: u64::MAX,
            ..Slot::default()
        });
        self.slots.len() - 1
    }
}

/// Quads a masked batch drew that nothing draws or marks any more, after
/// the batch was removed or its look changed: what its pixels left, which
/// the masked colour pass marks in the frames rendered until one that
/// began after the edit is finished. Its group keeps the records, look and
/// image it binds alive until then.
pub(crate) struct Footprint {
    pub group: wgpu::BindGroup,
    pub count: u32,
    pub bounds: Option<[Vec3; 2]>,
    /// A removed batch's records and the sprites they hold, which no live
    /// batch moves with the render origin (a restyled batch's slots move
    /// with it).
    owned: Option<(wgpu::Buffer, Vec<Sprite>, SpriteBatch)>,
}

#[derive(Default)]
pub(crate) struct Sprites {
    images: Slots<SpriteImageId, SpriteImage>,
    pub batches: Slots<SpriteBatchId, Batch>,
    /// Footprints of removed or restyled masked batches, each with whether
    /// a frame begun since rendered it (`begin_frame`), which retires it
    /// when it is finished.
    pub departed: Vec<(Footprint, bool)>,
    /// The batches' group layout, made with the first batch to have sprites.
    layout: Option<wgpu::BindGroupLayout>,
    /// One sampler for each state a batch samples with.
    samplers: HashMap<Sampler, wgpu::Sampler>,
}

impl Sprites {
    pub fn begin_frame(&mut self) {
        for (_, batch) in self.batches.iter_mut() {
            batch.rendered = Some(batch.revision());
        }
        for (_, rendered) in &mut self.departed {
            *rendered = true;
        }
    }

    pub fn finish_frame(&mut self) {
        self.departed.retain(|(_, rendered)| !rendered);
        for (_, batch) in self.batches.iter_mut() {
            if let Some(rendered) = batch.rendered.take() {
                batch.submitted = Some(rendered);
            }
        }
    }

    /// Moves the render origin by `by` (`Scene::move_origin`): every
    /// sprite's position in every slot, rewritten as `set_sprites` writes
    /// it, with no change of revision, so nothing is marked.
    pub fn move_origin(&mut self, queue: &wgpu::Queue, by: Vec3) {
        for (footprint, _) in &mut self.departed {
            if let Some((records, sprites, look)) = &mut footprint.owned {
                for sprite in sprites.iter_mut() {
                    sprite.position -= by;
                }
                footprint.bounds = bounds(look, sprites);
                write_records(queue, records, sprites);
            }
        }
        for (_, batch) in self.batches.iter_mut() {
            let look = batch.look;
            for slot in &mut batch.slots {
                for sprite in &mut slot.sprites {
                    sprite.position -= by;
                }
                slot.bounds = bounds(&look, &slot.sprites);
                if let Some(records) = &slot.records {
                    write_records(queue, records, &slot.sprites);
                }
            }
        }
    }

    /// Makes slot `slot` of batch `id`'s group over its look, its records,
    /// its image and its sampler, where the slot holds records.
    fn regroup(&mut self, device: &wgpu::Device, id: SpriteBatchId, slot: usize) {
        let batch = self.batches.get(id).expect("a regrouped batch lives");
        let (Some(uniform), Some(records)) = (&batch.uniform, &batch.slots[slot].records) else {
            return;
        };
        let (uniform, records, look) = (uniform.clone(), records.clone(), batch.look);
        let group = self.group_over(device, &look, &uniform, &records);
        self.batches.get_mut(id).expect("lives").slots[slot].group = Some(group);
    }

    /// A batch's group over `uniform`, `records`, and `look`'s image and
    /// sampler.
    fn group_over(
        &mut self,
        device: &wgpu::Device,
        look: &SpriteBatch,
        uniform: &wgpu::Buffer,
        records: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let layout = self
            .layout
            .get_or_insert_with(|| crate::shading::sprite::batch_layout(device));
        let sampler = self.samplers.entry(look.sampler).or_insert_with(|| {
            device.create_sampler(&crate::shading::samplers::descriptor(&look.sampler, 1))
        });
        let image = &self
            .images
            .get(look.image)
            .expect("a batch's image lives")
            .view;
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sprite batch"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: group::BATCH,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: group::RECORDS,
                    resource: records.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: group::IMAGE,
                    resource: wgpu::BindingResource::TextureView(image),
                },
                wgpu::BindGroupEntry {
                    binding: group::SAMPLER,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }
}

/// Writes `sprites`' records at the start of `buffer`.
fn write_records(queue: &wgpu::Queue, buffer: &wgpu::Buffer, sprites: &[Sprite]) {
    if sprites.is_empty() {
        return;
    }
    let records: Vec<SpriteRecord> = sprites.iter().map(SpriteRecord::new).collect();
    crate::counters::write_buffer(queue, buffer, 0, bytemuck::cast_slice(&records));
}

/// The world box every corner of `sprites` lies in for any view, for a
/// batch sized in metres: each sprite's position grown by the farthest its
/// pivot lets a corner reach, whichever way it faces.
fn bounds(look: &SpriteBatch, sprites: &[Sprite]) -> Option<[Vec3; 2]> {
    if look.sizing != SpriteSizing::World || sprites.is_empty() {
        return None;
    }
    let mut min = Vec3::INFINITY;
    let mut max = Vec3::NEG_INFINITY;
    for sprite in sprites {
        let reach = ((sprite.pivot - 0.5).abs() + 0.5) * sprite.size;
        let reach = Vec3::splat(reach.length());
        min = min.min(sprite.position - reach);
        max = max.max(sprite.position + reach);
    }
    Some([min, max])
}

impl Scene {
    /// Adds an image for sprite batches to show, uploaded once as sRGB
    /// colour with its mips (a decoded image's built on the GPU before the
    /// next frame draws, as a material's colour map's are). Batches cut cells from it with each sprite's
    /// region; pad an atlas's cells so their mips do not bleed.
    pub fn add_sprite_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &Image,
    ) -> Result<SpriteImageId, SceneError> {
        super::textures::validate(device, image)?;
        let view = super::textures::upload_color(device, queue, image, self.materials.chains());
        Ok(self.sprites.images.insert(SpriteImage { view, users: 0 }))
    }

    /// Removes a sprite image no batch shows.
    pub fn remove_sprite_image(&mut self, id: SpriteImageId) -> Result<(), SceneError> {
        let image = self
            .sprites
            .images
            .get(id)
            .ok_or(SceneError::UnknownSpriteImage)?;
        if image.users > 0 {
            return Err(SceneError::SpriteImageInUse);
        }
        if let Some(image) = self.sprites.images.remove(id) {
            self.materials.chains().forget(image.view.texture());
        }
        Ok(())
    }

    /// Adds a batch of sprites with the look `batch`, holding none until
    /// `set_sprites` gives it some. It holds no GPU buffer until then.
    pub fn add_sprite_batch(&mut self, batch: &SpriteBatch) -> Result<SpriteBatchId, SceneError> {
        if !batch_valid(batch) {
            return Err(SceneError::InvalidSprite);
        }
        let image = self
            .sprites
            .images
            .get_mut(batch.image)
            .ok_or(SceneError::UnknownSpriteImage)?;
        image.users += 1;
        Ok(self.sprites.batches.insert(Batch {
            look: *batch,
            slots: vec![Slot::default()],
            current: 0,
            uniform: None,
            look_revision: 0,
            sprites_revision: 0,
            rendered: None,
            submitted: None,
        }))
    }

    /// Replaces batch `id`'s look. An identical look changes nothing. Where
    /// a masked batch was drawn, the quads it drew under its old look are
    /// marked in the next frame (`Sprites::departed`), as the new look's
    /// are as any changed batch's.
    pub fn set_sprite_batch(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: SpriteBatchId,
        batch: &SpriteBatch,
    ) -> Result<(), SceneError> {
        let sprites = &mut self.sprites;
        let old = sprites
            .batches
            .get(id)
            .ok_or(SceneError::UnknownSpriteBatch)?
            .look;
        if !batch_valid(batch) {
            return Err(SceneError::InvalidSprite);
        }
        if sprites.images.get(batch.image).is_none() {
            return Err(SceneError::UnknownSpriteImage);
        }
        if old == *batch {
            return Ok(());
        }
        sprites.images.get_mut(old.image).expect("lives").users -= 1;
        sprites.images.get_mut(batch.image).expect("lives").users += 1;
        let regroup = old.image != batch.image || old.sampler != batch.sampler;
        // The old look's quads keep their own uniform where the facing or
        // sizing moves them; otherwise the slot's group, whose quads the
        // new look places the same, serves.
        let moved = old.facing != batch.facing || old.sizing != batch.sizing;
        let kept = moved.then(|| {
            crate::counters::buffer_init(
                device,
                &wgpu::util::BufferInitDescriptor {
                    label: Some("restyled sprite batch"),
                    contents: bytemuck::bytes_of(&SpriteBatchUniform::new(&old)),
                    usage: wgpu::BufferUsages::UNIFORM,
                },
            )
        });
        let drawn: Vec<_> = sprites
            .batches
            .get(id)
            .expect("checked above")
            .drawn_masked(&old)
            .into_iter()
            .map(|slot| {
                let group = slot.group.clone().expect("a drawn slot has its group");
                let records = slot.records.clone().expect("a drawn slot has records");
                (
                    group,
                    records,
                    slot.sprites.len() as u32,
                    bounds(&old, &slot.sprites),
                )
            })
            .collect();
        for (group, records, count, bounds) in drawn {
            let group = match &kept {
                Some(uniform) => sprites.group_over(device, &old, uniform, &records),
                None => group,
            };
            let footprint = Footprint {
                group,
                count,
                bounds,
                owned: None,
            };
            sprites.departed.push((footprint, false));
        }
        let entry = sprites.batches.get_mut(id).expect("checked above");
        entry.look = *batch;
        entry.look_revision += 1;
        for slot in &mut entry.slots {
            slot.bounds = bounds(batch, &slot.sprites);
        }
        if let Some(uniform) = &entry.uniform {
            crate::counters::write_buffer(
                queue,
                uniform,
                0,
                bytemuck::bytes_of(&SpriteBatchUniform::new(batch)),
            );
        }
        if regroup {
            let slots = entry.slots.len();
            for slot in 0..slots {
                sprites.regroup(device, id, slot);
            }
        }
        Ok(())
    }

    /// Replaces batch `id`'s sprites, uploading their records once, the
    /// batch's buffer growing to hold them. Sprites identical to the
    /// batch's upload nothing and change nothing; none clears it, keeping
    /// its buffer, and it then draws nothing. A sprite outside its ranges
    /// fails with `InvalidSprite`, and more than the device's largest
    /// storage binding holds with `DeviceLimit`, changing nothing.
    pub fn set_sprites(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: SpriteBatchId,
        sprites: &[Sprite],
    ) -> Result<(), SceneError> {
        let batch = self
            .sprites
            .batches
            .get(id)
            .ok_or(SceneError::UnknownSpriteBatch)?;
        if batch.sprites() == sprites {
            return Ok(());
        }
        if !sprites.iter().all(sprite_valid) {
            return Err(SceneError::InvalidSprite);
        }
        let limits = device.limits();
        let limit = limits
            .max_storage_buffer_binding_size
            .min(limits.max_buffer_size);
        let bytes = u32::try_from(sprites.len())
            .ok()
            .and_then(|count| u64::from(count).checked_mul(RECORD))
            .filter(|&bytes| bytes <= limit)
            .ok_or(SceneError::DeviceLimit)?;
        let look = self.sprites.batches.get(id).expect("lives").look;
        let batch = self.sprites.batches.get_mut(id).expect("lives");
        let slot = batch.writable();
        let held = batch.slots[slot]
            .records
            .as_ref()
            .map_or(0, wgpu::Buffer::size);
        if batch.uniform.is_none() && !sprites.is_empty() {
            let uniform = crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("sprite batch"),
                    size: std::mem::size_of::<SpriteBatchUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            );
            crate::counters::write_buffer(
                queue,
                &uniform,
                0,
                bytemuck::bytes_of(&SpriteBatchUniform::new(&look)),
            );
            batch.uniform = Some(uniform);
        }
        if bytes > held {
            batch.slots[slot].records = Some(crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("sprite records"),
                    size: bytes.max(held.saturating_mul(2)).min(limit),
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ));
            self.sprites.regroup(device, id, slot);
        }
        let batch = self.sprites.batches.get_mut(id).expect("lives");
        batch.sprites_revision += 1;
        let content = batch.sprites_revision;
        let target = &mut batch.slots[slot];
        target.sprites.clear();
        target.sprites.extend_from_slice(sprites);
        target.bounds = bounds(&look, sprites);
        target.content = content;
        if let Some(records) = &target.records {
            write_records(queue, records, sprites);
        }
        batch.current = slot;
        Ok(())
    }

    /// Removes batch `id` with its buffers: those of the sprites a masked
    /// batch drew live until the next frame has marked where they stood.
    pub fn remove_sprite_batch(&mut self, id: SpriteBatchId) -> Result<(), SceneError> {
        let batch = self
            .sprites
            .batches
            .remove(id)
            .ok_or(SceneError::UnknownSpriteBatch)?;
        if let Some(image) = self.sprites.images.get_mut(batch.look.image) {
            image.users -= 1;
        }
        // What a masked batch drew is marked in the next frame, its group
        // keeping its buffers alive until then; the rest is freed now.
        let footprints: Vec<_> = batch
            .drawn_masked(&batch.look)
            .into_iter()
            .map(|slot| Footprint {
                group: slot.group.clone().expect("a drawn slot has its group"),
                count: slot.sprites.len() as u32,
                bounds: slot.bounds,
                owned: Some((
                    slot.records.clone().expect("a drawn slot has records"),
                    slot.sprites.clone(),
                    batch.look,
                )),
            })
            .collect();
        self.sprites
            .departed
            .extend(footprints.into_iter().map(|footprint| (footprint, false)));
        Ok(())
    }
}
