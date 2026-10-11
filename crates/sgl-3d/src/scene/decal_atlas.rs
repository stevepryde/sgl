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
//! `Item::new`); texels are linear 16-bit floats, a colour image's decoded
//! from sRGB, so one view serves colour and data where Godot shares an sRGB
//! view of RGBA8; images of the same size are placed in a fixed order; the
//! mips are box-filtered here, each from the 16-bit level above, as Godot's
//! blits halve its 8-bit ones, so the atlas is bounded by the memory its
//! build takes (`MAX_BUILD_BYTES`) as well as the device's texture size;
//! and an atlas without images is one transparent texel.
use super::SceneError;
use crate::content::identity::{DecalImageId, Identity};
use crate::content::static_lighting::{half_value, irradiance_half};

/// The atlas's mips, Godot's `DecalAtlas::mipmaps`.
const MIPS: u32 = 5;
/// The packing grid's cell, in texels: an image's border on each side is
/// half of it, a texel at the last mip.
const BORDER: u32 = 1 << MIPS;
/// Bytes in a texel: RGBA16F, which `fill` builds and uploads.
const TEXEL_BYTES: usize = 8;
/// The most memory `fill` may build an atlas in: its levels' texels on the
/// CPU. Godot bounds its atlas only by the device's texture size, as it
/// blits the atlas on the GPU; SGL3D builds it on the CPU, where a size the
/// device allows can be more than the target can allocate. In the browser,
/// 2 GiB: half the 4 GiB a wasm32 module addresses, which the game's other
/// content shares. It holds a 16384 by 8192 atlas (1.33 GiB with its mips,
/// from an 8192 by 4096 image) and refuses a 16384-square one (2.66 GiB,
/// from an 8192-square image), whose first level alone is more than one
/// wasm32 allocation can be.
#[cfg(target_arch = "wasm32")]
const MAX_BUILD_BYTES: u64 = 2 << 30;
/// Natively, 8 GiB: the 16384-square atlas most devices' textures top out
/// at, and a 32768 by 16384 one (5.33 GiB, from a 16384 by 8192 image)
/// where the device allows that side; it refuses a 32768-square one
/// (10.66 GiB, from a 16384-square image).
#[cfg(not(target_arch = "wasm32"))]
const MAX_BUILD_BYTES: u64 = 8 << 30;

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
    /// be larger than `limit` texels on a side or take more than
    /// `MAX_BUILD_BYTES` to build.
    pub fn place(images: &[(AtlasKey, [u32; 2])], limit: u32) -> Result<Self, SceneError> {
        if images.is_empty() {
            return Ok(Self::empty());
        }
        // An image with its border wider than `limit` fits no atlas; refused
        // before packing, whose grid would be as wide as the image.
        if images
            .iter()
            .any(|(_, size)| size.iter().any(|side| side.saturating_add(BORDER) > limit))
        {
            return Err(SceneError::DeviceLimit);
        }
        let mut items: Vec<Item> = images
            .iter()
            .map(|&(key, size)| Item::new(key, size))
            .collect();
        let size = place(&mut items);
        if size[0] > limit
            || size[1] > limit
            || build_bytes(size).is_none_or(|bytes| bytes > MAX_BUILD_BYTES)
        {
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
            view: upload(device, queue, [1, 1], &[vec![0; TEXEL_BYTES]]),
            layout: AtlasLayout::empty(),
        }
    }

    /// The atlas `layout` places, filled with the texels `texels` gives each
    /// image, with its mips; an image it gives none for stays transparent.
    /// Each level is built in the upload format, so the upload copies none.
    pub fn fill<'a>(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: AtlasLayout,
        texels: impl Fn(AtlasKey) -> Option<&'a image::RgbaImage>,
    ) -> Self {
        let size = layout.size;
        // Byte counts and indices are `usize`: a 32768 by 16384 atlas has
        // more bytes than `u32` counts. `place` bounds them by
        // `MAX_BUILD_BYTES`.
        let width = size[0] as usize;
        let mut level = vec![0u8; width * size[1] as usize * TEXEL_BYTES];
        for &(key, corner, _) in &layout.places {
            let Some(image) = texels(key) else {
                continue;
            };
            let color = key.1 == Sampling::Color;
            for (x, y, texel) in image.enumerate_pixels() {
                let at =
                    ((corner[1] + y) as usize * width + (corner[0] + x) as usize) * TEXEL_BYTES;
                for channel in 0..4 {
                    let value = f32::from(texel[channel]) / 255.;
                    let value = if color && channel < 3 {
                        crate::shading::srgb::to_linear(value)
                    } else {
                        value
                    };
                    level[at + channel * 2..][..2]
                        .copy_from_slice(&irradiance_half(value).to_le_bytes());
                }
            }
        }
        let mut levels = vec![level];
        if !layout.places.is_empty() {
            for mip in 1..MIPS {
                let [width, height] = size.map(|side| (side >> (mip - 1)) as usize);
                let previous = levels.last().unwrap();
                let texel = |x: usize, y: usize, channel: usize| {
                    let at = (y * width + x) * TEXEL_BYTES + channel * 2;
                    half_value(u16::from_le_bytes([previous[at], previous[at + 1]]))
                };
                let mut halved = Vec::with_capacity(width / 2 * (height / 2) * TEXEL_BYTES);
                for y in (0..height).step_by(2) {
                    for x in (0..width).step_by(2) {
                        for channel in 0..4 {
                            let mean = (texel(x, y, channel)
                                + texel(x + 1, y, channel)
                                + texel(x, y + 1, channel)
                                + texel(x + 1, y + 1, channel))
                                / 4.;
                            halved.extend_from_slice(&irradiance_half(mean).to_le_bytes());
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

/// The bytes `fill` builds an atlas of `size` in, `None` past `u64`.
fn build_bytes(size: [u32; 2]) -> Option<u64> {
    (0..MIPS).try_fold(0u64, |sum, mip| {
        let [width, height] = size.map(|side| u64::from(side >> mip));
        width
            .checked_mul(height)?
            .checked_mul(TEXEL_BYTES as u64)?
            .checked_add(sum)
    })
}

/// An RGBA16F texture of `size` with mips `levels`, each little-endian
/// RGBA16F texels in [0, 1].
fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: [u32; 2],
    levels: &[Vec<u8>],
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
    for (mip, bytes) in levels.iter().enumerate() {
        let [width, height] = size.map(|side| (side >> mip).max(1));
        crate::counters::write_texture(
            queue,
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: mip as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * TEXEL_BYTES as u32),
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
