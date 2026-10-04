//! The decal atlas: each image the scene's decals project, in each way they
//! sample it, packed into one texture with its mips, which group 0 binds.
//!
//! The packing is Godot b130438's
//! (`servers/rendering/renderer_rd/storage_rd/texture_storage.cpp`,
//! `TextureStorage::update_decal_atlas`), MIT (`src/LICENSE-godot.txt`): each
//! image on a grid of `BORDER` texels with half a cell of border around it,
//! placed largest first at the lowest point of a skyline, in an atlas whose
//! width doubles until its height is at most twice its width; its mips
//! halve the atlas, and the border keeps an image's texels apart from its
//! neighbours' down to the last. Changes: a side is rounded up to whole
//! cells before its border is added, which Godot's sizing leaves out (see
//! `Item::new`); texels are linear 16-bit floats, a
//! colour image's decoded from sRGB, so one view serves colour and data
//! where Godot shares an sRGB view of RGBA8; images of the same size are
//! placed in a fixed order; the mips are box-filtered here, as Godot's blits
//! halve them; and an atlas without images is one transparent texel.
use super::SceneError;
use crate::content::identity::{DecalImageId, Identity};
use crate::content::static_lighting::irradiance_half;

/// The atlas's mips, Godot's `DecalAtlas::mipmaps`.
const MIPS: u32 = 5;
/// The packing grid's cell, in texels: an image's border on each side is
/// half of it, a texel at the last mip.
const BORDER: u32 = 1 << MIPS;

/// How decals sample an image: as sRGB colour (a base colour) or as linear
/// data (a normal or metallic-roughness map).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Sampling {
    Color,
    Data,
}

/// An image in the way decals sample it.
pub(crate) type AtlasKey = (DecalImageId, Sampling);

/// Where each image is in an atlas, and the atlas's size, before its
/// texels are filled.
pub(crate) struct AtlasLayout {
    /// The atlas's width and height in texels.
    size: [u32; 2],
    /// Each image's top-left texel and size in texels.
    places: Vec<(AtlasKey, [u32; 2], [u32; 2])>,
}

impl AtlasLayout {
    /// No images: one transparent texel.
    fn empty() -> Self {
        Self {
            size: [1, 1],
            places: Vec::new(),
        }
    }

    /// `images`, each a key and its size in texels, placed as Godot's
    /// `update_decal_atlas` places them; `DeviceLimit` when the atlas would
    /// be larger than `limit` texels on a side.
    pub fn place(images: &[(AtlasKey, [u32; 2])], limit: u32) -> Result<Self, SceneError> {
        if images.is_empty() {
            return Ok(Self::empty());
        }
        let mut items: Vec<Item> = images
            .iter()
            .map(|&(key, size)| Item::new(key, size))
            .collect();
        let size = place(&mut items);
        if size[0] > limit || size[1] > limit {
            return Err(SceneError::DeviceLimit);
        }
        Ok(Self {
            size,
            places: items
                .iter()
                .map(|item| {
                    let corner = item.position.map(|cell| cell * BORDER + BORDER / 2);
                    (item.key, corner, item.pixel_size)
                })
                .collect(),
        })
    }

    /// Where `key` is, in atlas UV (offset in xy, size in zw), if the layout
    /// holds it.
    pub fn rect(&self, key: AtlasKey) -> Option<[f32; 4]> {
        let &(_, corner, pixel_size) = self.places.iter().find(|(place, ..)| *place == key)?;
        let [width, height] = self.size.map(|side| side as f32);
        Some([
            corner[0] as f32 / width,
            corner[1] as f32 / height,
            pixel_size[0] as f32 / width,
            pixel_size[1] as f32 / height,
        ])
    }
}

/// An atlas on the GPU and its layout.
pub(crate) struct DecalAtlas {
    pub view: wgpu::TextureView,
    pub layout: AtlasLayout,
}

/// An image to place: its key, its size in texels and in grid cells, and
/// its place.
struct Item {
    key: AtlasKey,
    pixel_size: [u32; 2],
    size: [u32; 2],
    position: [u32; 2],
}

impl Item {
    /// An image of `pixel_size` with half a cell of border on each side, in
    /// whole cells. Godot's `width / border + 1` cells leave too little room
    /// for a side more than half a cell past a whole number of cells, so its
    /// last texels reach the next image's border; this rounds the side up
    /// first.
    fn new(key: AtlasKey, pixel_size: [u32; 2]) -> Self {
        Self {
            key,
            pixel_size,
            size: pixel_size.map(|side| side.div_ceil(BORDER) + 1),
            position: [0; 2],
        }
    }
}

impl DecalAtlas {
    /// An atlas of no images.
    pub fn empty(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self {
            view: upload(device, queue, [1, 1], &[vec![0.; 4]]),
            layout: AtlasLayout::empty(),
        }
    }

    /// The atlas `layout` places, filled with the texels `texels` gives each
    /// image, with its mips; an image it gives none for stays transparent.
    pub fn fill<'a>(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: AtlasLayout,
        texels: impl Fn(AtlasKey) -> Option<&'a image::RgbaImage>,
    ) -> Self {
        let size = layout.size;
        let mut level = vec![0f32; (size[0] * size[1] * 4) as usize];
        for &(key, corner, _) in &layout.places {
            let Some(image) = texels(key) else {
                continue;
            };
            let color = key.1 == Sampling::Color;
            for (x, y, texel) in image.enumerate_pixels() {
                let at = (((corner[1] + y) * size[0] + corner[0] + x) * 4) as usize;
                for channel in 0..4 {
                    let value = f32::from(texel[channel]) / 255.;
                    level[at + channel] = if color && channel < 3 {
                        srgb_to_linear(value)
                    } else {
                        value
                    };
                }
            }
        }
        let mut levels = vec![level];
        if !layout.places.is_empty() {
            for mip in 1..MIPS {
                let [width, height] = size.map(|side| side >> (mip - 1));
                let previous = levels.last().unwrap();
                let texel = |x: u32, y: u32, channel: u32| {
                    previous[((y * width + x) * 4 + channel) as usize]
                };
                let mut halved = Vec::with_capacity((width * height) as usize);
                for y in (0..height).step_by(2) {
                    for x in (0..width).step_by(2) {
                        for channel in 0..4 {
                            halved.push(
                                (texel(x, y, channel)
                                    + texel(x + 1, y, channel)
                                    + texel(x, y + 1, channel)
                                    + texel(x + 1, y + 1, channel))
                                    / 4.,
                            );
                        }
                    }
                }
                levels.push(halved);
            }
        }
        Self {
            view: upload(device, queue, size, &levels),
            layout,
        }
    }
}

/// Places `items` (sizes in grid cells), as Godot's `update_decal_atlas`
/// does, and returns the atlas's size in texels: a power of two on each
/// side, its width at least `8 * BORDER` and its height at least
/// `2 * BORDER`, so every mip halves it.
fn place(items: &mut [Item]) -> [u32; 2] {
    // Larger to smaller, as Godot's SortItem orders them, then in a fixed
    // order.
    items.sort_by_key(|item| {
        let (image, sampling) = item.key;
        (
            std::cmp::Reverse(item.size[1]),
            std::cmp::Reverse(item.size[0]),
            image.index(),
            image.generation(),
            sampling == Sampling::Data,
        )
    });
    let mut base_size = items
        .iter()
        .map(|item| item.size[0].next_power_of_two())
        .fold(8, u32::max);
    let atlas_height = loop {
        let mut offsets = vec![0u32; base_size as usize];
        // Room for the border at the least.
        let mut max_height = 2;
        for item in items.iter_mut() {
            let [width, height] = item.size;
            let (mut best_index, mut best_height) = (0, u32::MAX);
            for start in 0..=base_size - width {
                let mut lowest = 0;
                for &offset in &offsets[start as usize..(start + width) as usize] {
                    lowest = lowest.max(offset);
                    if lowest > best_height {
                        break;
                    }
                }
                if lowest < best_height {
                    best_height = lowest;
                    best_index = start;
                }
            }
            for offset in &mut offsets[best_index as usize..(best_index + width) as usize] {
                *offset = best_height + height;
            }
            item.position = [best_index, best_height];
            max_height = max_height.max(best_height + height);
        }
        if max_height <= base_size * 2 {
            break max_height;
        }
        base_size *= 2;
    };
    [
        base_size * BORDER,
        (atlas_height * BORDER).next_power_of_two(),
    ]
}

/// The sRGB transfer function's inverse.
fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// An RGBA16F texture of `size` with mips `levels`, each RGBA in [0, 1].
fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: [u32; 2],
    levels: &[Vec<f32>],
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("decal atlas"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (mip, values) in levels.iter().enumerate() {
        let [width, height] = size.map(|side| (side >> mip).max(1));
        let bytes: Vec<u8> = values
            .iter()
            .flat_map(|&value| irradiance_half(value).to_le_bytes())
            .collect();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: mip as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 8),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }
    texture.create_view(&Default::default())
}

#[cfg(test)]
mod tests;
