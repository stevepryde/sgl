//! A small persistent wgpu sprite renderer with no event loop or game policy.
//!
//! Textures use sRGB sampling, tint/clear colours are linear, and display output
//! is sRGB-encoded on native and browser surfaces.

use core::fmt;
use std::ops::Range;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use winit::window::Window;

pub use crate::surface::RendererInitError;
use crate::surface::{self, Swapchain, WindowGpu, WindowSurface};

/// Hard limit on sprites submitted in one frame.
pub const MAX_SPRITES: usize = 262_144;
const INITIAL_INSTANCE_CAPACITY: usize = 256;

/// Opaque renderer-owned texture identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureId(u32);

impl TextureId {
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Pixel rectangle inside an uploaded texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// One caller-authored textured quad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sprite {
    pub texture: TextureId,
    /// World-space position of `pivot`.
    pub position: [f32; 2],
    /// World-space quad size.
    pub size: [f32; 2],
    /// Normalized pivot where `[0, 0]` is top-left and `[1, 1]` bottom-right.
    pub pivot: [f32; 2],
    pub rotation_radians: f32,
    pub source: PixelRect,
    pub tint: [f32; 4],
    pub flip_x: bool,
    pub flip_y: bool,
}

/// Reused, caller-owned sprite storage for one frame.
#[derive(Debug, Default)]
pub struct SpriteBatch {
    sprites: Vec<Sprite>,
}

impl SpriteBatch {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sprites: Vec::new(),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.sprites.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sprites.is_empty()
    }

    /// Clears the batch without releasing its allocation.
    pub fn clear(&mut self) {
        self.sprites.clear();
    }

    pub fn push(&mut self, sprite: Sprite) -> Result<(), SpriteError> {
        if self.sprites.len() >= MAX_SPRITES {
            return Err(SpriteError::CapacityExceeded);
        }
        validate_sprite(&sprite)?;
        self.sprites.push(sprite);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpriteError {
    CapacityExceeded,
    EmptySource,
    InvalidTransform,
    InvalidTint,
}

impl fmt::Display for SpriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CapacityExceeded => "sprite batch capacity exceeded",
            Self::EmptySource => "sprite source rectangle is empty",
            Self::InvalidTransform => "sprite transform is non-finite or invalid",
            Self::InvalidTint => "sprite tint must contain finite values in [0, 1]",
        })
    }
}

impl std::error::Error for SpriteError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureUploadError {
    Empty,
    TooLarge,
    ByteLength,
    TextureCountExhausted,
}

impl fmt::Display for TextureUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "texture dimensions must be nonzero",
            Self::TooLarge => "texture exceeds the adapter dimension limit",
            Self::ByteLength => "RGBA byte length does not match texture dimensions",
            Self::TextureCountExhausted => "texture identity space exhausted",
        })
    }
}

impl std::error::Error for TextureUploadError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderError {
    UnknownTexture(TextureId),
    SourceOutsideTexture {
        texture: TextureId,
        source: PixelRect,
    },
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTexture(texture) => {
                write!(formatter, "unknown texture {}", texture.raw())
            }
            Self::SourceOutsideTexture { texture, source } => write!(
                formatter,
                "source rectangle ({}, {}, {}, {}) exceeds texture {}",
                source.x,
                source.y,
                source.width,
                source.height,
                texture.raw()
            ),
        }
    }
}

impl std::error::Error for RenderError {}

/// Outcome of one caller-requested presentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameOutcome {
    Presented,
    /// Minimized, occluded, timed out, or temporarily invalid surface.
    Skipped,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct InstanceRaw {
    model_x: [f32; 2],
    model_y: [f32; 2],
    translation: [f32; 2],
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    tint: [f32; 4],
    flips: [f32; 2],
    padding: [f32; 2],
}

struct Texture {
    _gpu_resource: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    size: [u32; 2],
}

struct DrawGroup {
    texture: TextureId,
    instances: Range<u32>,
}

/// Persistent GPU state. The game owns when it is created, resized, and drawn.
pub struct Renderer {
    window: Arc<Window>,
    surface: WindowSurface,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_size: [u32; 2],
    pipeline: wgpu::RenderPipeline,
    quad_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    textures: Vec<Texture>,
    instances: Vec<InstanceRaw>,
    groups: Vec<DrawGroup>,
}

impl Renderer {
    /// Creates persistent renderer state for a game-owned window.
    pub async fn new(window: Arc<Window>) -> Result<Self, RendererInitError> {
        let WindowGpu {
            device,
            queue,
            surface,
        } = surface::bring_up(
            &window,
            |adapter| wgpu::DeviceDescriptor {
                label: Some("sgl device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits()),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            },
            |caps| {
                // The surface's preferred format and present mode.
                let format = *caps.formats.first()?;
                // Browser surfaces commonly default to an unorm format. Render
                // through its sRGB view so linear texture samples and tints are
                // encoded for display, just as they are on native sRGB surfaces.
                let view_format = format.add_srgb_suffix();
                Some(Swapchain {
                    format,
                    present_mode: *caps.present_modes.first()?,
                    alpha_mode: wgpu::CompositeAlphaMode::Auto,
                    view_formats: if view_format == format {
                        Vec::new()
                    } else {
                        vec![view_format]
                    },
                })
            },
        )
        .await?;
        let view_format = surface.format().add_srgb_suffix();
        let size = window.inner_size();

        let (camera_buffer, camera_layout, camera_bind_group) = create_camera(&device);
        let (texture_layout, sampler) = create_texture_binding(&device);
        let pipeline = create_pipeline(&device, view_format, &camera_layout, &texture_layout);
        let quad_buffer = create_quad_buffer(&device);
        let instance_buffer = create_instance_buffer(&device, INITIAL_INSTANCE_CAPACITY);

        Ok(Self {
            window,
            surface,
            device,
            queue,
            surface_size: [size.width, size.height],
            pipeline,
            quad_buffer,
            instance_buffer,
            instance_capacity: INITIAL_INSTANCE_CAPACITY,
            camera_buffer,
            camera_bind_group,
            texture_layout,
            sampler,
            textures: Vec::new(),
            instances: Vec::new(),
            groups: Vec::new(),
        })
    }

    #[must_use]
    pub fn surface_size(&self) -> [u32; 2] {
        self.surface_size
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.surface_size = [width, height];
        self.surface.resize(&self.device, width, height);
    }

    /// Uploads one decoded straight-alpha RGBA8 texture once.
    pub fn upload_rgba8(
        &mut self,
        label: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<TextureId, TextureUploadError> {
        if width == 0 || height == 0 {
            return Err(TextureUploadError::Empty);
        }
        if width > self.device.limits().max_texture_dimension_2d
            || height > self.device.limits().max_texture_dimension_2d
        {
            return Err(TextureUploadError::TooLarge);
        }
        let expected = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(TextureUploadError::ByteLength)?;
        if rgba.len() != expected {
            return Err(TextureUploadError::ByteLength);
        }
        let raw = u32::try_from(self.textures.len())
            .map_err(|_| TextureUploadError::TextureCountExhausted)?;
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            extent,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.textures.push(Texture {
            _gpu_resource: texture,
            bind_group,
            size: [width, height],
        });
        Ok(TextureId(raw))
    }

    /// Draws one game-authored frame. No simulation or timing work occurs here.
    pub fn render(
        &mut self,
        clear: [f64; 4],
        view_projection: [[f32; 4]; 4],
        batch: &SpriteBatch,
    ) -> Result<FrameOutcome, RenderError> {
        if self.surface_size[0] == 0 || self.surface_size[1] == 0 {
            return Ok(FrameOutcome::Skipped);
        }
        self.prepare(batch)?;
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&view_projection));
        if !self.instances.is_empty() {
            self.queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances),
            );
        }

        let Some(frame) = self.surface.acquire(&self.device) else {
            return Ok(FrameOutcome::Skipped);
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.surface.format().add_srgb_suffix()),
            ..Default::default()
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sgl frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sgl sprite pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear[0],
                            g: clear[1],
                            b: clear[2],
                            a: clear[3],
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_vertex_buffer(0, self.quad_buffer.slice(..));
            pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
            for group in &self.groups {
                let texture = &self.textures[group.texture.raw() as usize];
                pass.set_bind_group(1, &texture.bind_group, &[]);
                pass.draw(0..6, group.instances.clone());
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.window.pre_present_notify();
        frame.present();
        Ok(FrameOutcome::Presented)
    }

    fn prepare(&mut self, batch: &SpriteBatch) -> Result<(), RenderError> {
        self.instances.clear();
        self.groups.clear();
        if batch.sprites.len() > self.instance_capacity {
            self.instance_capacity = batch.sprites.len().next_power_of_two();
            self.instance_buffer = create_instance_buffer(&self.device, self.instance_capacity);
        }
        for sprite in &batch.sprites {
            let Some(texture) = self.textures.get(sprite.texture.raw() as usize) else {
                return Err(RenderError::UnknownTexture(sprite.texture));
            };
            if !source_within_texture(sprite.source, texture.size) {
                return Err(RenderError::SourceOutsideTexture {
                    texture: sprite.texture,
                    source: sprite.source,
                });
            }
            let start = u32::try_from(self.instances.len()).expect("MAX_SPRITES fits u32");
            self.instances.push(instance_raw(*sprite, texture.size));
            match self.groups.last_mut() {
                Some(group) if group.texture == sprite.texture => group.instances.end += 1,
                _ => self.groups.push(DrawGroup {
                    texture: sprite.texture,
                    instances: start..start + 1,
                }),
            }
        }
        Ok(())
    }
}

fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sgl sprite instances"),
        size: (capacity * std::mem::size_of::<InstanceRaw>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_camera(device: &wgpu::Device) -> (wgpu::Buffer, wgpu::BindGroupLayout, wgpu::BindGroup) {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sgl camera"),
        size: 64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sgl camera layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sgl camera bind group"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });
    (buffer, layout, bind_group)
}

fn create_texture_binding(device: &wgpu::Device) -> (wgpu::BindGroupLayout, wgpu::Sampler) {
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sgl texture layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("sgl nearest sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    (layout, sampler)
}

fn create_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    camera_layout: &wgpu::BindGroupLayout,
    texture_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("sgl sprite pipeline layout"),
        bind_group_layouts: &[Some(camera_layout), Some(texture_layout)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sgl sprite shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("sprite.wgsl").into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sgl sprite pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vertex"),
            buffers: &[
                wgpu::VertexBufferLayout {
                    array_stride: 8,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2],
                },
                wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<InstanceRaw>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        1 => Float32x2,
                        2 => Float32x2,
                        3 => Float32x2,
                        4 => Float32x2,
                        5 => Float32x2,
                        6 => Float32x4,
                        7 => Float32x2
                    ],
                },
            ],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fragment"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn create_quad_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("sgl unit quad"),
        contents: bytemuck::cast_slice(&[
            [0.0_f32, 0.0],
            [1.0, 0.0],
            [1.0, 1.0],
            [0.0, 0.0],
            [1.0, 1.0],
            [0.0, 1.0],
        ]),
        usage: wgpu::BufferUsages::VERTEX,
    })
}

fn validate_sprite(sprite: &Sprite) -> Result<(), SpriteError> {
    if sprite.source.width == 0 || sprite.source.height == 0 {
        return Err(SpriteError::EmptySource);
    }
    if !sprite.position.into_iter().all(f32::is_finite)
        || !sprite
            .size
            .into_iter()
            .all(|value| value.is_finite() && value > 0.0)
        || !sprite
            .pivot
            .into_iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(&value))
        || !sprite.rotation_radians.is_finite()
    {
        return Err(SpriteError::InvalidTransform);
    }
    if !sprite
        .tint
        .into_iter()
        .all(|value| value.is_finite() && (0.0..=1.0).contains(&value))
    {
        return Err(SpriteError::InvalidTint);
    }
    Ok(())
}

fn source_within_texture(source: PixelRect, texture_size: [u32; 2]) -> bool {
    source
        .x
        .checked_add(source.width)
        .is_some_and(|right| right <= texture_size[0])
        && source
            .y
            .checked_add(source.height)
            .is_some_and(|bottom| bottom <= texture_size[1])
}

#[allow(clippy::cast_precision_loss)]
fn instance_raw(sprite: Sprite, texture_size: [u32; 2]) -> InstanceRaw {
    let (sin, cos) = sprite.rotation_radians.sin_cos();
    let model_x = [cos * sprite.size[0], sin * sprite.size[0]];
    let model_y = [-sin * sprite.size[1], cos * sprite.size[1]];
    let translation = [
        sprite.position[0] - model_x[0] * sprite.pivot[0] - model_y[0] * sprite.pivot[1],
        sprite.position[1] - model_x[1] * sprite.pivot[0] - model_y[1] * sprite.pivot[1],
    ];
    let texture_width = texture_size[0] as f32;
    let texture_height = texture_size[1] as f32;
    InstanceRaw {
        model_x,
        model_y,
        translation,
        uv_min: [
            sprite.source.x as f32 / texture_width,
            sprite.source.y as f32 / texture_height,
        ],
        uv_max: [
            (sprite.source.x + sprite.source.width) as f32 / texture_width,
            (sprite.source.y + sprite.source.height) as f32 / texture_height,
        ],
        tint: sprite.tint,
        flips: [f32::from(sprite.flip_x), f32::from(sprite.flip_y)],
        padding: [0.0; 2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sprite() -> Sprite {
        Sprite {
            texture: TextureId(0),
            position: [10.0, 20.0],
            size: [8.0, 8.0],
            pivot: [0.5, 0.5],
            rotation_radians: 0.0,
            source: PixelRect::new(0, 0, 8, 8),
            tint: [1.0; 4],
            flip_x: false,
            flip_y: false,
        }
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn batch_reuses_storage_and_rejects_invalid_sprites() {
        let mut batch = SpriteBatch::new();
        batch.push(sprite()).unwrap();
        batch.clear();
        assert!(batch.is_empty());
        let mut invalid = sprite();
        invalid.size[0] = f32::NAN;
        assert_eq!(batch.push(invalid), Err(SpriteError::InvalidTransform));
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn instance_transform_places_the_pivot_at_the_position() {
        let raw = instance_raw(sprite(), [8, 8]);
        assert!((raw.translation[0] - 6.0).abs() <= f32::EPSILON);
        assert!((raw.translation[1] - 16.0).abs() <= f32::EPSILON);
        assert!(
            raw.uv_min
                .into_iter()
                .all(|value| value.abs() <= f32::EPSILON)
        );
        assert!(
            raw.uv_max
                .into_iter()
                .all(|value| (value - 1.0).abs() <= f32::EPSILON)
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn source_rectangle_must_fit_the_uploaded_texture() {
        assert!(source_within_texture(PixelRect::new(1, 2, 3, 4), [4, 6]));
        assert!(!source_within_texture(PixelRect::new(2, 2, 3, 4), [4, 6]));
        assert!(!source_within_texture(
            PixelRect::new(u32::MAX, 0, 1, 1),
            [u32::MAX, 1]
        ));
    }
}
