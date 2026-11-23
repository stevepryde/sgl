//! Material textures: each image materials were added with, as the sampled
//! textures raster binds and as its level 0 in the ray source. Materials
//! added together share them; a texture lives while a material uses it. A
//! decoded image's mip chain is built on the GPU by the mip chain stage
//! (`stages::mip_chains`), which takes the textures waiting here
//! (`Chains`); prepared and block-compressed chains are uploaded as stored.
use super::SceneError;
use super::rays::SceneRays;
use crate::asset::{CompressedFormat, CompressedImage, Image};
use std::ops::Range;

/// Tests read an RGBA8 texture's levels back.
#[cfg(test)]
const READBACK: wgpu::TextureUsages = wgpu::TextureUsages::COPY_SRC;
#[cfg(not(test))]
const READBACK: wgpu::TextureUsages = wgpu::TextureUsages::empty();

/// An RGBA8 texture of `levels` levels with `first`, its first level,
/// written, sampled as sRGB colour or as linear data.
fn rgba8_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    first: &image::RgbaImage,
    (levels, usage): (u32, wgpu::TextureUsages),
    srgb: bool,
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("retained material map"),
        size: wgpu::Extent3d {
            width: first.width(),
            height: first.height(),
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: if srgb {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        },
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | READBACK
            | usage,
        view_formats: &[],
    });
    write_level(queue, &texture, 0, first);
    texture
}

fn write_level(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    level: u32,
    texels: &image::RgbaImage,
) {
    crate::counters::write_texture(
        queue,
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: level,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        texels.as_raw(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(texels.width() * 4),
            rows_per_image: Some(texels.height()),
        },
        wgpu::Extent3d {
            width: texels.width(),
            height: texels.height(),
            depth_or_array_layers: 1,
        },
    );
}

/// A decoded image as a texture of one level, for a map nothing samples
/// below its first: the environment's panorama and the white fallback.
pub(crate) fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &image::RgbaImage,
    srgb: bool,
) -> wgpu::TextureView {
    rgba8_texture(
        device,
        queue,
        image,
        (1, wgpu::TextureUsages::empty()),
        srgb,
    )
    .create_view(&Default::default())
}

/// `image` as one sampled texture of sRGB colour with its mips, as a
/// material samples a colour map: a sprite image (`scene::sprites`). A
/// decoded image's chain waits in `chains` for the mip chain stage.
pub(crate) fn upload_color(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &Image,
    chains: &mut Chains,
) -> wgpu::TextureView {
    match image {
        Image::Rgba8(image) => chains.upload(device, queue, image, true),
        Image::Rgba8Mips(image) => upload_rgba8_levels(device, queue, image.levels(true), true),
        Image::Compressed(image) => {
            let [color, _] = upload_compressed(device, queue, image, [true, false]);
            color.expect("a colour view was asked for")
        }
    }
}

/// Uploads already filtered levels without cloning or filtering them.
fn upload_rgba8_levels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    levels: &[image::RgbaImage],
    srgb: bool,
) -> wgpu::TextureView {
    let usage = wgpu::TextureUsages::empty();
    let texture = rgba8_texture(
        device,
        queue,
        &levels[0],
        (levels.len() as u32, usage),
        srgb,
    );
    for (level, mip) in (1..).zip(&levels[1..]) {
        write_level(queue, &texture, level, mip);
    }
    texture.create_view(&Default::default())
}

/// The decoded images' textures whose levels below the first are not built
/// yet (the architecture's Material mip chains). The mip chain stage takes
/// them before anything draws a material.
#[derive(Default)]
pub(crate) struct Chains {
    waiting: Vec<wgpu::Texture>,
}

impl Chains {
    /// A decoded image as a texture with every level down to one texel,
    /// its first written and the rest waiting for the mip chain stage,
    /// which draws each into the texture.
    pub(crate) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &image::RgbaImage,
        srgb: bool,
    ) -> wgpu::TextureView {
        let levels = 32 - image.width().max(image.height()).leading_zeros();
        if levels == 1 {
            return upload(device, queue, image, srgb);
        }
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let texture = rgba8_texture(device, queue, image, (levels, usage), srgb);
        let view = texture.create_view(&Default::default());
        self.waiting.push(texture);
        view
    }

    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }

    /// `texture` no longer waits: what sampled it is gone (a material's
    /// texture freed, a sprite image removed, the materials of a failed
    /// `add_materials`), so neither its chain's passes nor its memory are
    /// spent on it.
    pub fn forget(&mut self, texture: &wgpu::Texture) {
        self.waiting.retain(|waiting| waiting != texture);
    }

    /// The textures waiting, which the caller builds: none wait after.
    pub fn take(&mut self) -> Vec<wgpu::Texture> {
        std::mem::take(&mut self.waiting)
    }
}

/// A block-compressed map with its stored levels, as one texture viewed as
/// sRGB colour and as linear data where `color` and `data` ask. A texture
/// sampled one way has that way's format, as Bevy creates it; one sampled
/// both ways is linear with an sRGB view, as Godot b130438 shares an sRGB
/// view of one texture (`servers/rendering/renderer_rd/storage_rd/
/// texture_storage.cpp`: `shareable_formats`, `rd_texture_srgb`; MIT,
/// src/LICENSE-godot.txt), which needs `DownlevelFlags::VIEW_FORMATS`
/// (`validate_uses`).
fn upload_compressed(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &CompressedImage,
    [color, data]: [bool; 2],
) -> [Option<wgpu::TextureView>; 2] {
    let linear = match image.format {
        CompressedFormat::Bc7 => wgpu::TextureFormat::Bc7RgbaUnorm,
        CompressedFormat::Etc2Rgba8 => wgpu::TextureFormat::Etc2Rgba8Unorm,
        CompressedFormat::Astc4x4 => wgpu::TextureFormat::Astc {
            block: wgpu::AstcBlock::B4x4,
            channel: wgpu::AstcChannel::Unorm,
        },
    };
    let srgb = linear.add_srgb_suffix();
    let srgb_view = [srgb];
    let (format, view_formats): (_, &[_]) = match [color, data] {
        [true, false] => (srgb, &[]),
        [false, true] => (linear, &[]),
        _ => (linear, &srgb_view),
    };
    let texture = crate::counters::texture_init(
        device,
        queue,
        &wgpu::TextureDescriptor {
            label: Some("retained compressed material map"),
            size: wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: image.levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats,
        },
        wgpu::util::TextureDataOrder::MipMajor,
        &image.levels.concat(),
    );
    let view = |format| {
        texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(format),
            ..Default::default()
        })
    };
    [color.then(|| view(srgb)), data.then(|| view(linear))]
}

/// Whether `device` can sample a compressed image that materials use as
/// both colour and data (`uses`), from one texture with an sRGB view:
/// `DownlevelFlags::VIEW_FORMATS`, which every wgpu backend but GL has. A
/// device does not report its adapter's downlevel flags, so its backend
/// stands for them.
pub(crate) fn validate_uses(
    device: &wgpu::Device,
    image: &Image,
    uses: [bool; 2],
) -> Result<(), SceneError> {
    if matches!(image, Image::Compressed(_))
        && uses == [true; 2]
        && device.adapter_info().backend == wgpu::Backend::Gl
    {
        return Err(SceneError::CompressedImageViews);
    }
    Ok(())
}

/// An image of `width` by `height` texels as a sampled texture of `device`:
/// it has texels and fits the device's 2D textures.
pub(crate) fn validate_size(
    device: &wgpu::Device,
    [width, height]: [u32; 2],
) -> Result<(), SceneError> {
    if width == 0 || height == 0 {
        return Err(SceneError::EmptyImage);
    }
    if width.max(height) > device.limits().max_texture_dimension_2d {
        return Err(SceneError::DeviceLimit);
    }
    Ok(())
}

/// `image` as a material texture of `device`: `validate_size`, and a
/// compressed one is a chain the device can sample.
pub(crate) fn validate(device: &wgpu::Device, image: &Image) -> Result<(), SceneError> {
    validate_size(device, image.size())?;
    if let Image::Compressed(image) = image {
        if !device
            .features()
            .contains(crate::graphics_device::compressed_image_feature(
                image.format,
            ))
        {
            return Err(SceneError::CompressionUnsupported);
        }
        if !image.is_valid() {
            return Err(SceneError::InvalidCompressedImage);
        }
    }
    Ok(())
}

/// ETC2/ASTC blocks have no CPU/ray decoder. Require the offline companion
/// only at boundaries that actually sample decoded texels, before uploading.
pub(crate) fn validate_cpu_texels(image: &Image) -> Result<(), SceneError> {
    if let Image::Compressed(image) = image
        && image.format != CompressedFormat::Bc7
        && image.decoded_level_0.is_none()
    {
        return Err(SceneError::MissingDecodedImage);
    }
    Ok(())
}

/// One image as its materials sample it.
pub(crate) struct Texture {
    /// Sampled as sRGB colour, when a material uses it for colour.
    pub color: Option<wgpu::TextureView>,
    /// Sampled as linear data, when a material uses it for data.
    pub data: Option<wgpu::TextureView>,
    /// Its record and level 0 in the ray source.
    pub ray: Range<u32>,
    /// Materials using it.
    users: usize,
}

#[derive(Default)]
pub(crate) struct Textures {
    entries: Vec<Option<Texture>>,
    free: Vec<usize>,
    /// Those of them whose mip chains are not built yet.
    pub chains: Chains,
}

impl Textures {
    /// `image`, as colour and data where `color` and `data` ask, with no
    /// users yet.
    pub fn add(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &mut SceneRays,
        image: &Image,
        [color, data]: [bool; 2],
    ) -> Result<usize, SceneError> {
        let ray = rays.add_image(device, queue, image)?;
        let [color, data] = match image {
            Image::Rgba8(image) => [
                color.then(|| self.chains.upload(device, queue, image, true)),
                data.then(|| self.chains.upload(device, queue, image, false)),
            ],
            Image::Rgba8Mips(image) => [
                color.then(|| upload_rgba8_levels(device, queue, image.levels(true), true)),
                data.then(|| upload_rgba8_levels(device, queue, image.levels(false), false)),
            ],
            Image::Compressed(image) => upload_compressed(device, queue, image, [color, data]),
        };
        let texture = Texture {
            color,
            data,
            ray,
            users: 0,
        };
        Ok(match self.free.pop() {
            Some(index) => {
                self.entries[index] = Some(texture);
                index
            }
            None => {
                self.entries.push(Some(texture));
                self.entries.len() - 1
            }
        })
    }

    pub fn get(&self, index: usize) -> &Texture {
        self.entries[index]
            .as_ref()
            .expect("a material's texture lives while it does")
    }

    pub fn use_texture(&mut self, index: usize) {
        self.entries[index]
            .as_mut()
            .expect("a material's texture lives while it does")
            .users += 1;
    }

    /// One user fewer; the last frees the texture and its ray image.
    pub fn release(&mut self, rays: &mut SceneRays, index: usize) {
        self.entries[index]
            .as_mut()
            .expect("a material's texture lives while it does")
            .users -= 1;
        self.free_unused(rays, index);
    }

    /// Frees the texture at `index` and its ray image if it is still there and
    /// no material uses it.
    pub fn free_unused(&mut self, rays: &mut SceneRays, index: usize) {
        if self.entries[index]
            .as_ref()
            .is_some_and(|texture| texture.users == 0)
        {
            let texture = self.entries[index].take().unwrap();
            rays.free(texture.ray);
            for view in [&texture.color, &texture.data].into_iter().flatten() {
                self.chains.forget(view.texture());
            }
            self.free.push(index);
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
