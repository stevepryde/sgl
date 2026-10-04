//! Material textures: each image materials were added with, as the sampled
//! textures raster binds and as its level 0 in the ray source. Materials
//! added together share them; a texture lives while a material uses it.
use super::SceneError;
use super::rays::SceneRays;
use crate::asset::{CompressedFormat, CompressedImage, Image};
use std::ops::Range;

/// A decoded material map with its mip chain, filtered in linear light for
/// colour.
pub(crate) fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &image::RgbaImage,
    srgb: bool,
) -> wgpu::TextureView {
    let levels = 32 - image.width().max(image.height()).leading_zeros();
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("retained material map"),
        size: wgpu::Extent3d {
            width: image.width(),
            height: image.height(),
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
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut mip = image.clone();
    for level in 0..levels {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &mip,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(mip.width() * 4),
                rows_per_image: Some(mip.height()),
            },
            wgpu::Extent3d {
                width: mip.width(),
                height: mip.height(),
                depth_or_array_layers: 1,
            },
        );
        // Filter color mips in linear light; data maps stay linear.
        if level + 1 < levels {
            let mut linear = image::Rgba32FImage::new(mip.width(), mip.height());
            for (to, from) in linear.pixels_mut().zip(mip.pixels()) {
                for c in 0..4 {
                    let x = from[c] as f32 / 255.0;
                    to[c] = if srgb && c < 3 {
                        if x <= 0.04045 {
                            x / 12.92
                        } else {
                            ((x + 0.055) / 1.055).powf(2.4)
                        }
                    } else {
                        x
                    };
                }
            }
            let small = image::imageops::resize(
                &linear,
                (mip.width() / 2).max(1),
                (mip.height() / 2).max(1),
                image::imageops::FilterType::Triangle,
            );
            mip = image::RgbaImage::new(small.width(), small.height());
            for (to, from) in mip.pixels_mut().zip(small.pixels()) {
                for c in 0..4 {
                    let x = from[c];
                    let x = if srgb && c < 3 {
                        if x <= 0.0031308 {
                            x * 12.92
                        } else {
                            1.055 * x.powf(1.0 / 2.4) - 0.055
                        }
                    } else {
                        x
                    };
                    to[c] = (x.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
    }
    tex.create_view(&Default::default())
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
    use wgpu::util::DeviceExt;
    let linear = match image.format {
        CompressedFormat::Bc7 => wgpu::TextureFormat::Bc7RgbaUnorm,
    };
    let srgb = linear.add_srgb_suffix();
    let srgb_view = [srgb];
    let (format, view_formats): (_, &[_]) = match [color, data] {
        [true, false] => (srgb, &[]),
        [false, true] => (linear, &[]),
        _ => (linear, &srgb_view),
    };
    let texture = device.create_texture_with_data(
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
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            return Err(SceneError::CompressionUnsupported);
        }
        if !image.is_valid() {
            return Err(SceneError::InvalidCompressedImage);
        }
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
                color.then(|| upload(device, queue, image, true)),
                data.then(|| upload(device, queue, image, false)),
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
            self.free.push(index);
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
