//! Decals: the images decals project, kept as texels and packed into the
//! decal atlas in each way a decal samples them (`decal_atlas`), and each
//! decal's description and its record in the scene's decal buffer, which
//! group 0 binds with the atlas, with the `Scene` operations that add, read,
//! change and remove them. A removed decal's record stays until its index is
//! reused; no view lists it, and the atlas keeps its images until it next
//! packs.
//!
//! As Godot marks its decal atlas dirty and packs it once per frame
//! (`TextureStorage::update_decal_atlas`), a decal that brings in an image
//! the atlas lacks only places the new layout, so the addition refuses an
//! atlas the device cannot hold; the frame's prepare (or a probe capture)
//! then uploads its texels and rewrites every record once.
use super::decal_atlas::{AtlasKey, AtlasLayout, DecalAtlas, Sampling};
use super::slots::Slots;
use super::{Scene, SceneError};
use crate::asset::Image;
use crate::content::decal::Decal;
use crate::content::identity::{DecalId, DecalImageId, Identity};
use crate::shading::decals::{DecalRecord, DecalRects};

const RECORD: u64 = std::mem::size_of::<DecalRecord>() as u64;

/// An image decals project, and how many decals use it.
struct DecalImage {
    texels: image::RgbaImage,
    users: usize,
}

pub(crate) struct Decals {
    images: Slots<DecalImageId, DecalImage>,
    pub slots: Slots<DecalId, Decal>,
    buffer: wgpu::Buffer,
    pub atlas: DecalAtlas,
    /// The layout the atlas takes at the next upload, placed when a decal
    /// brought in an image the atlas lacks (Godot's `decal_atlas.dirty`).
    pending: Option<AtlasLayout>,
    /// Every record is rewritten at the next upload: the layout or the
    /// buffer changed since they were written.
    stale: bool,
    /// Filters the atlas: trilinear and clamped, Godot's default decal
    /// filter (`rendering/textures/decals/filter`, linear with mipmaps).
    pub sampler: wgpu::Sampler,
}

/// The images `decal` projects, each in the way it samples it.
fn keys(decal: &Decal) -> impl Iterator<Item = AtlasKey> {
    [
        Some((decal.base_color, Sampling::Color)),
        decal.normal.map(|image| (image, Sampling::Data)),
        decal
            .metallic_roughness
            .map(|image| (image, Sampling::Data)),
    ]
    .into_iter()
    .flatten()
}

impl Decals {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self {
            images: Slots::default(),
            slots: Slots::default(),
            buffer: decal_buffer(device, 1),
            atlas: DecalAtlas::empty(device, queue),
            pending: None,
            stale: false,
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("decal atlas"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            }),
        }
    }

    /// The records, as group 0 binds them.
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    pub fn get(&self, id: DecalId) -> Result<&Decal, SceneError> {
        self.slots.get(id).ok_or(SceneError::UnknownDecal)
    }

    /// Every decal, in index order: the ones every view lists.
    pub fn iter(&self) -> impl Iterator<Item = (DecalId, &Decal)> {
        self.slots.iter()
    }

    /// Whether the atlas waits for its next upload.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn packing(&self) -> bool {
        self.pending.is_some()
    }

    /// The layout records name: the one waiting for its upload, else the
    /// atlas's.
    fn layout(&self) -> &AtlasLayout {
        self.pending.as_ref().unwrap_or(&self.atlas.layout)
    }

    fn record(&self, decal: &Decal) -> DecalRecord {
        let rect = |key| {
            self.layout()
                .rect(key)
                .expect("the layout holds every live decal's images")
        };
        DecalRecord::new(
            decal,
            DecalRects {
                base_color: rect((decal.base_color, Sampling::Color)),
                normal: decal.normal.map(|image| rect((image, Sampling::Data))),
                metallic_roughness: decal
                    .metallic_roughness
                    .map(|image| rect((image, Sampling::Data))),
            },
        )
    }

    /// Writes `decal`'s record now, unless every record waits for the next
    /// upload.
    fn write(&self, queue: &wgpu::Queue, index: usize, decal: &Decal) {
        if !self.stale {
            queue.write_buffer(
                &self.buffer,
                index as u64 * RECORD,
                bytemuck::bytes_of(&self.record(decal)),
            );
        }
    }

    /// The records a buffer with room for `count` would hold, when the
    /// buffer has none; `DeviceLimit` when the device cannot bind it.
    fn room(&self, device: &wgpu::Device, count: usize) -> Result<Option<u64>, SceneError> {
        let capacity = self.buffer.size() / RECORD;
        if count as u64 <= capacity {
            return Ok(None);
        }
        let limits = device.limits();
        let limit = limits
            .max_storage_buffer_binding_size
            .min(limits.max_buffer_size)
            / RECORD;
        if count as u64 > limit {
            return Err(SceneError::DeviceLimit);
        }
        Ok(Some((count as u64).max(capacity * 2).min(limit)))
    }

    /// A new layout when `decal` brings in an image the current one lacks:
    /// the images `decal` and the live decals other than `replacing` use;
    /// `DeviceLimit` when the device cannot hold it.
    fn place_for(
        &self,
        device: &wgpu::Device,
        decal: &Decal,
        replacing: Option<DecalId>,
    ) -> Result<Option<AtlasLayout>, SceneError> {
        if keys(decal).all(|key| self.layout().rect(key).is_some()) {
            return Ok(None);
        }
        let mut used: Vec<AtlasKey> = self
            .slots
            .iter()
            .filter(|(id, _)| Some(*id) != replacing)
            .flat_map(|(_, decal)| keys(decal))
            .chain(keys(decal))
            .collect();
        used.sort_by_key(|(image, sampling)| (image.index(), *sampling == Sampling::Data));
        used.dedup();
        let images: Vec<(AtlasKey, [u32; 2])> = used
            .into_iter()
            .map(|key| {
                let texels = &self.images.get(key.0).unwrap().texels;
                (key, [texels.width(), texels.height()])
            })
            .collect();
        AtlasLayout::place(&images, device.limits().max_texture_dimension_2d).map(Some)
    }

    /// Takes `layout` for the next upload, which rewrites every record.
    fn repack(&mut self, layout: Option<AtlasLayout>) {
        if let Some(layout) = layout {
            self.pending = Some(layout);
            self.stale = true;
        }
    }

    /// Uploads the pending atlas and rewrites every record when either
    /// waits: once per frame. Returns whether the atlas was replaced.
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        let packed = if let Some(layout) = self.pending.take() {
            let images = &self.images;
            self.atlas = DecalAtlas::fill(device, queue, layout, |(image, _)| {
                images.get(image).map(|image| &image.texels)
            });
            true
        } else {
            false
        };
        if std::mem::take(&mut self.stale) {
            for (id, decal) in self.slots.iter() {
                self.write(queue, id.index(), decal);
            }
        }
        packed
    }

    /// Counts `decal`'s images as used once more, or once less.
    fn count_users(&mut self, decal: &Decal, more: bool) {
        let mut images = vec![decal.base_color];
        images.extend(decal.normal);
        images.extend(decal.metallic_roughness);
        images.sort_by_key(|image| image.index());
        images.dedup();
        for image in images {
            let users = &mut self.images.get_mut(image).unwrap().users;
            if more {
                *users += 1;
            } else {
                *users -= 1;
            }
        }
    }

    /// `decal` is valid and its images are in the scene.
    fn validate(&self, decal: &Decal) -> Result<(), SceneError> {
        if !valid(decal) {
            return Err(SceneError::InvalidDecal);
        }
        if keys(decal).any(|(image, _)| self.images.get(image).is_none()) {
            return Err(SceneError::UnknownDecalImage);
        }
        Ok(())
    }
}

fn decal_buffer(device: &wgpu::Device, records: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("scene decals"),
        size: records * RECORD,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// A decal a record can hold: a finite position, a finite rotation of
/// nonzero length, a positive finite size, colour, mix and normal fade in
/// their ranges and nonnegative finite fades.
fn valid(decal: &Decal) -> bool {
    let unit = |value: f32| (0. ..=1.).contains(&value);
    let fade = |value: f32| value.is_finite() && value >= 0.;
    decal.position.is_finite()
        && decal.rotation.is_finite()
        && decal.rotation.length_squared() > 0.
        && decal.rotation.normalize().is_finite()
        && decal.size.is_finite()
        && decal.size.min_element() > 0.
        && decal.color.iter().all(|&channel| unit(channel))
        && unit(decal.base_color_mix)
        && fade(decal.upper_fade)
        && fade(decal.lower_fade)
        && (0. ..1.).contains(&decal.normal_fade)
}

impl Scene {
    /// Adds an image for decals to project; a compressed image's level 0 is
    /// decoded. Decals sample it as sRGB colour or linear data by the map
    /// that names it.
    pub fn add_decal_image(&mut self, image: Image) -> Result<DecalImageId, SceneError> {
        if let Image::Compressed(compressed) = &image
            && !compressed.is_valid()
        {
            return Err(SceneError::InvalidCompressedImage);
        }
        let [width, height] = image.size();
        if width == 0 || height == 0 {
            return Err(SceneError::EmptyImage);
        }
        let texels = match image {
            Image::Rgba8(texels) => texels,
            compressed => compressed.texels().into_owned(),
        };
        Ok(self.decals.images.insert(DecalImage { texels, users: 0 }))
    }

    /// Removes a decal image no decal uses.
    pub fn remove_decal_image(&mut self, id: DecalImageId) -> Result<(), SceneError> {
        let image = self
            .decals
            .images
            .get(id)
            .ok_or(SceneError::UnknownDecalImage)?;
        if image.users > 0 {
            return Err(SceneError::DecalImageInUse);
        }
        self.decals.images.remove(id);
        Ok(())
    }

    /// Adds a decal. One that projects an image, in a way, that the scene's
    /// decals did not yet project packs the decal atlas anew before the next
    /// frame.
    pub fn add_decal(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        decal: Decal,
    ) -> Result<DecalId, SceneError> {
        let decals = &mut self.decals;
        decals.validate(&decal)?;
        let room = decals.room(device, decals.slots.next_index() + 1)?;
        let layout = decals.place_for(device, &decal, None)?;
        if let Some(records) = room {
            decals.buffer = decal_buffer(device, records);
            decals.stale = true;
            self.resources = super::next_generation();
        }
        decals.repack(layout);
        let id = decals.slots.insert(decal);
        decals.count_users(&decal, true);
        decals.write(queue, id.index(), &decal);
        Ok(id)
    }

    /// A decal's current description.
    pub fn decal(&self, id: DecalId) -> Result<&Decal, SceneError> {
        self.decals.get(id)
    }

    /// Replaces a decal's description. One that projects an image, in a
    /// way, that the scene's decals did not yet project packs the decal
    /// atlas anew before the next frame.
    pub fn set_decal(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: DecalId,
        decal: Decal,
    ) -> Result<(), SceneError> {
        let decals = &mut self.decals;
        let old = *decals.get(id)?;
        decals.validate(&decal)?;
        let layout = decals.place_for(device, &decal, Some(id))?;
        decals.repack(layout);
        decals.count_users(&old, false);
        decals.count_users(&decal, true);
        *decals.slots.get_mut(id).unwrap() = decal;
        decals.write(queue, id.index(), &decal);
        Ok(())
    }

    /// Removes a decal. Its index is reused under a new identity.
    pub fn remove_decal(&mut self, id: DecalId) -> Result<(), SceneError> {
        let decal = self
            .decals
            .slots
            .remove(id)
            .ok_or(SceneError::UnknownDecal)?;
        self.decals.count_users(&decal, false);
        Ok(())
    }

    /// Uploads a decal atlas packed since the last frame and rewrites the
    /// records it moved, through the queue: before a frame or a probe
    /// capture draws.
    pub(crate) fn upload_decals(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.decals.upload(device, queue) {
            self.resources = super::next_generation();
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
