//! Decals: the images decals and spot lights' cookies project, kept as
//! texels and packed into the decal atlas in each way they are sampled
//! (`decal_atlas`), and each decal's description and its record in the
//! scene's decal buffer, which group 0 binds with the atlas, with the
//! `Scene` operations that add, read, change and remove them. A removed
//! decal's record stays until its index is reused; no view lists it, and
//! the atlas keeps its images until it next packs.
//!
//! As Godot marks its decal atlas dirty and packs it once per frame
//! (`TextureStorage::update_decal_atlas`), a decal or light that brings in
//! an image the atlas lacks only places the new layout, so the addition
//! refuses an atlas the device cannot hold; the frame's prepare (or a probe
//! capture) then uploads its texels and rewrites every decal's record, and
//! every cookied light's, once. Godot keeps its spot projectors' images in
//! the same atlas (`LightStorage::light_set_projector`).
use super::decal_atlas::{AtlasKey, AtlasLayout, DecalAtlas, Sampling};
use super::slots::Slots;
use super::{Scene, SceneError};
use crate::asset::Image;
use crate::content::decal::Decal;
use crate::content::identity::{DecalId, DecalImageId, Identity, LightId};
use crate::shading::decals::{DecalRecord, DecalRects};

const RECORD: u64 = std::mem::size_of::<DecalRecord>() as u64;

/// An image decals and cookied lights project, and how many of them use
/// it.
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
    /// The device's largest texture side (`max_texture_dimension_2d`),
    /// which bounds the atlas: kept from the scene's creation, so an edit
    /// that takes no device (`Scene::set_light`) can place a layout.
    limit: u32,
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
            limit: device.limits().max_texture_dimension_2d,
            // Lit group 0's one trilinear clamp-to-edge sampler, which the
            // blended draws' transmission also filters the transparent
            // stage's copy of the frame through (transmission_extended.wgsl):
            // keep it trilinear and clamped to the edge.
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

    /// Whether the scene holds no decal: the lit pipelines apply decals only
    /// while it holds one.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
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
            crate::counters::write_buffer(
                queue,
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

    /// A cookied light's image's place (`key`) in the layout records name
    /// (`layout`), in atlas UV; zero for a light without a cookie.
    pub fn cookie_rect(&self, key: Option<AtlasKey>) -> [f32; 4] {
        key.map_or([0.; 4], |key| {
            self.layout()
                .rect(key)
                .expect("the layout holds every cookied light's image")
        })
    }

    /// Takes `layout` for the next upload, which rewrites every record.
    pub fn repack(&mut self, layout: Option<AtlasLayout>) {
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

    /// Moves the render origin by `by` (`Scene::move_origin`): each decal's
    /// position, whose record the next upload rewrites with every other.
    pub fn move_origin(&mut self, by: glam::Vec3) {
        for (_, decal) in self.slots.iter_mut() {
            decal.position -= by;
        }
        self.stale |= !self.slots.is_empty();
    }

    /// Counts `decal`'s images as used once more, or once less.
    fn count_users(&mut self, decal: &Decal, more: bool) {
        let mut images = vec![decal.base_color];
        images.extend(decal.normal);
        images.extend(decal.metallic_roughness);
        images.sort_by_key(|image| image.index());
        images.dedup();
        for image in images {
            self.count_image(image, more);
        }
    }

    /// Counts `image` as used by one user (a decal or a light) more, or
    /// one less.
    pub fn count_image(&mut self, image: DecalImageId, more: bool) {
        let users = &mut self.images.get_mut(image).unwrap().users;
        if more {
            *users += 1;
        } else {
            *users -= 1;
        }
    }

    /// The images of `keys` are in the scene.
    pub fn validate_images(&self, keys: &[AtlasKey]) -> Result<(), SceneError> {
        if keys
            .iter()
            .any(|(image, _)| self.images.get(*image).is_none())
        {
            return Err(SceneError::UnknownDecalImage);
        }
        Ok(())
    }

    /// `decal` is valid and its images are in the scene.
    fn validate(&self, decal: &Decal) -> Result<(), SceneError> {
        if !valid(decal) {
            return Err(SceneError::InvalidDecal);
        }
        self.validate_images(&keys(decal).collect::<Vec<_>>())
    }
}

fn decal_buffer(device: &wgpu::Device, records: u64) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some("scene decals"),
            size: records * RECORD,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
    )
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
    /// A new layout of the decal atlas when `adding` brings in an image the
    /// current one lacks: the images of `adding`, the live decals other
    /// than `decal` and the lights other than `light` use; `DeviceLimit`
    /// when the device cannot hold it.
    pub(crate) fn place_atlas(
        &self,
        adding: &[AtlasKey],
        decal: Option<DecalId>,
        light: Option<LightId>,
    ) -> Result<Option<AtlasLayout>, SceneError> {
        let decals = &self.decals;
        if adding
            .iter()
            .all(|&key| decals.layout().rect(key).is_some())
        {
            return Ok(None);
        }
        let mut used: Vec<AtlasKey> = decals
            .slots
            .iter()
            .filter(|(id, _)| Some(*id) != decal)
            .flat_map(|(_, decal)| keys(decal))
            .chain(self.lights.cookie_keys(light))
            .chain(adding.iter().copied())
            .collect();
        used.sort_by_key(|(image, sampling)| (image.index(), *sampling == Sampling::Data));
        used.dedup();
        let images: Vec<(AtlasKey, [u32; 2])> = used
            .into_iter()
            .map(|key| {
                let texels = &decals.images.get(key.0).unwrap().texels;
                (key, [texels.width(), texels.height()])
            })
            .collect();
        AtlasLayout::place(&images, decals.limit).map(Some)
    }

    /// Adds an image for decals and spot lights' cookies to project; a
    /// compressed image's level 0 is decoded. Decals sample it as sRGB
    /// colour or linear data by the map that names it, a cookie as sRGB
    /// colour.
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
            compressed => compressed
                .texels()
                .ok_or(SceneError::MissingDecodedImage)?
                .into_owned(),
        };
        self.edited();
        Ok(self.decals.images.insert(DecalImage { texels, users: 0 }))
    }

    /// Removes a decal image no decal or light uses.
    pub fn remove_decal_image(&mut self, id: DecalImageId) -> Result<(), SceneError> {
        self.edited();
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
        self.edited();
        self.decals.validate(&decal)?;
        let room = self
            .decals
            .room(device, self.decals.slots.next_index() + 1)?;
        let layout = self.place_atlas(&keys(&decal).collect::<Vec<_>>(), None, None)?;
        let decals = &mut self.decals;
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
        queue: &wgpu::Queue,
        id: DecalId,
        decal: Decal,
    ) -> Result<(), SceneError> {
        if *self.decals.get(id)? != decal {
            self.edited();
        }
        let old = *self.decals.get(id)?;
        self.decals.validate(&decal)?;
        let layout = self.place_atlas(&keys(&decal).collect::<Vec<_>>(), Some(id), None)?;
        let decals = &mut self.decals;
        decals.repack(layout);
        decals.count_users(&old, false);
        decals.count_users(&decal, true);
        *decals.slots.get_mut(id).unwrap() = decal;
        decals.write(queue, id.index(), &decal);
        Ok(())
    }

    /// Removes a decal. Its index is reused under a new identity.
    pub fn remove_decal(&mut self, id: DecalId) -> Result<(), SceneError> {
        self.edited();
        let decal = self
            .decals
            .slots
            .remove(id)
            .ok_or(SceneError::UnknownDecal)?;
        self.decals.count_users(&decal, false);
        Ok(())
    }

    /// Uploads a decal atlas packed since the last frame and rewrites the
    /// records it moved, the decals' and the cookied lights', through the
    /// queue: before a frame or a probe capture draws, so a record never
    /// names a place in another atlas than the one bound with it.
    pub(crate) fn upload_decals(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.decals.upload(device, queue) {
            self.resources = super::next_generation();
            let decals = &self.decals;
            self.lights
                .write_cookied(queue, &super::lights::cookie_rects(decals));
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
