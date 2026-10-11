//! Offscreen scene targets + sprite/light/composite passes + letterbox blit
//! (PR-1, PR-4, AR-6).
//!
//! One frame is now (R-6):
//!
//! 1. **scene pass** — the world-channel sprites into three MRT targets at
//!    the fixed logical resolution: **albedo** (the base scene, full-bright),
//!    a screen-space **normal** target and a **light-mask** target (see
//!    `sprite.wgsl`).
//! 2. **light passes** — per-light additive accumulation with occluder
//!    shadows (see [`render::light`](crate::canvas::light)).
//! 3. **composite** — `albedo × (canvas_modulate + light_accum)` into the
//!    **composited** target (Godot add-mode; the identity when unlit).
//! 4. **screen pass** — the screen-channel sprites (UI/HUD) drawn over the
//!    composited target, bypassing lighting entirely.
//! 5. **letterbox blit** — the composited target scaled to the window inside
//!    an aspect-preserving viewport (nearest sampling, fractional scale
//!    allowed; the bars come from the swapchain clear outside the viewport).
//!
//! **Color space** ([`LightingSpace`]): the **composited** target always
//! holds sRGB-encoded 8-bit values ([`SCENE_FORMAT`]), so the screen pass,
//! the blit (which decodes for an sRGB swapchain or copies for a unorm one)
//! and [`Renderer::capture_scene`] never depend on the mode. What differs:
//! - **Gamma** (default, Godot parity — D-8): the albedo target is 8-bit
//!   gamma too, sprites are sampled raw, and the composite multiply-add runs
//!   on gamma values and stores the result unchanged.
//! - **Linear**: the scene pass decodes sprite texels to linear into a
//!   half-float albedo ([`LINEAR_ALBEDO_FORMAT`]), the clear color is
//!   converted to linear, the modulate / light colors are taken as linear,
//!   and the composite multiplies in linear then sRGB-encodes exactly once
//!   into the composited target.

use std::path::Path;

use crate::assets::{Assets, Handle, Texture};
use crate::canvas::camera::Camera;
use crate::canvas::draw::DrawList;
use crate::canvas::gpu::{Context, Frame, Gpu};
use crate::canvas::letterbox::fit_fractional;
use crate::canvas::light::{LightFrame, LightPass};
use crate::canvas::sprite::{MASK_FORMAT, NORMAL_FORMAT, SpritePass, TextureError};
use crate::canvas::{LightingSpace, linear_to_srgb, srgb_to_linear};

/// Format of the composited target (both modes) and of the albedo target
/// under [`LightingSpace::Gamma`]. **Not** `*_Srgb`: the stored bytes are
/// the sRGB-encoded values themselves (Godot's framebuffer, D-8); the blit
/// decodes them for an sRGB swapchain.
pub const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Albedo target format under [`LightingSpace::Linear`]. Half-float rather
/// than 8-bit because linear values need far more precision in the darks
/// than sRGB-encoded ones: sRGB's sixty darkest 8-bit codes decode to
/// linear `0..0.045`, which an 8-bit linear target would collapse into
/// eleven steps — visible posterization once the composite scales it by a
/// dim ambient (shadow-sp's Dark ambient brightness is `0.003`) and the
/// result is re-encoded for display.
pub const LINEAR_ALBEDO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The albedo target format for a lighting space.
fn albedo_format(space: LightingSpace) -> wgpu::TextureFormat {
    match space {
        LightingSpace::Gamma => SCENE_FORMAT,
        LightingSpace::Linear => LINEAR_ALBEDO_FORMAT,
    }
}

/// The [`LightingSpace::Linear`] composite for one pixel, mirroring
/// `fs_main_linear` in `composite.wgsl`: `albedo × (modulate + accum)` in
/// linear, clamped to `[0, 1]`, then sRGB-encoded once for the 8-bit
/// composited target. (The gamma composite is the same multiply-add stored
/// without encoding.)
#[must_use]
pub fn composite_linear(albedo: [f32; 3], modulate: [f32; 3], accum: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0; 3];
    for i in 0..3 {
        let combined = (albedo[i] * (modulate[i] + accum[i])).clamp(0.0, 1.0);
        out[i] = linear_to_srgb(combined);
    }
    out
}

/// Owns the offscreen targets, the sprite + light passes, the composite, and
/// the letterbox blit.
///
/// **Per-scene resolution (D-26)**: game scenes render at the fixed logical
/// resolution and letterbox-blit up (PR-1, parity-locked). The editor scene
/// calls [`set_target_size`](Self::set_target_size) with the surface size
/// (native-res render; the blit becomes a 1:1 copy) and
/// [`set_ui_scale`](Self::set_ui_scale) so the screen channel lays out in
/// UI points (`physical / ui_scale`, see [`ui_size`](Self::ui_size)). Both
/// default to the game behavior.
pub struct Renderer {
    logical_width: u32,
    logical_height: u32,
    /// The size last asked of [`set_target_size`](Self::set_target_size)
    /// (the window), before the target-size policy: the screen channel
    /// lays out over it even when the target is scaled down.
    requested_size: (u32, u32),
    /// The device's max 2D texture side, captured at init — the hard cap
    /// [`set_target_size`](Self::set_target_size) clamps against.
    max_texture_dim: u32,
    /// Physical pixels per screen-channel layout unit (1.0 = game scenes).
    ui_scale: f32,
    /// The color space the world channel is lit and composited in.
    lighting_space: LightingSpace,
    /// The scene clear color in the albedo target's space (as given in
    /// `Gamma`, converted to linear in `Linear`).
    clear_color: wgpu::Color,
    /// Base scene target (world channel, full-bright — PR-4).
    albedo_view: wgpu::TextureView,
    normal_view: wgpu::TextureView,
    mask_view: wgpu::TextureView,
    /// Lit scene + UI; what the blit and [`Self::capture_scene`] read.
    composited_texture: wgpu::Texture,
    composited_view: wgpu::TextureView,
    sprites: SpritePass,
    lights: LightPass,
    composite: CompositePass,
    blit_pipeline: wgpu::RenderPipeline,
    blit_bgl: wgpu::BindGroupLayout,
    blit_sampler: wgpu::Sampler,
    blit_bind_group: wgpu::BindGroup,
}

/// The offscreen scene targets at one pixel size.
struct SceneTargets {
    albedo_view: wgpu::TextureView,
    normal_view: wgpu::TextureView,
    mask_view: wgpu::TextureView,
    composited_texture: wgpu::Texture,
    composited_view: wgpu::TextureView,
}

/// Create the four offscreen scene targets at `width × height`; the albedo
/// target takes the lighting `space`'s format.
fn scene_targets(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    space: LightingSpace,
) -> SceneTargets {
    let target = |label: &str, format: wgpu::TextureFormat, extra: wgpu::TextureUsages| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | extra,
            view_formats: &[],
        })
    };
    let none = wgpu::TextureUsages::empty();
    let albedo_view = target("scene albedo target", albedo_format(space), none)
        .create_view(&wgpu::TextureViewDescriptor::default());
    let normal_view = target("scene normal target", NORMAL_FORMAT, none)
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mask_view = target("scene light-mask target", MASK_FORMAT, none)
        .create_view(&wgpu::TextureViewDescriptor::default());
    // Screenshot readback happens from the composited target (AR-9).
    let composited_texture = target(
        "composited target (sRGB-encoded)",
        SCENE_FORMAT,
        wgpu::TextureUsages::COPY_SRC,
    );
    let composited_view = composited_texture.create_view(&wgpu::TextureViewDescriptor::default());
    SceneTargets {
        albedo_view,
        normal_view,
        mask_view,
        composited_texture,
        composited_view,
    }
}

/// Scene-capture failures ([`Renderer::capture_scene`]).
#[derive(Debug)]
pub enum CaptureError {
    /// The GPU readback failed (map/poll error).
    Readback(String),
    /// Encoding or writing the PNG failed.
    Encode(image::ImageError),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Readback(msg) => write!(f, "scene readback: {msg}"),
            Self::Encode(e) => write!(f, "png encode: {e}"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// The lighting composite: `albedo × (canvas_modulate + light_accum)` over a
/// fullscreen triangle into the composited target, with the fragment entry
/// point of the renderer's [`LightingSpace`] (see `composite.wgsl`).
struct CompositePass {
    pipeline: wgpu::RenderPipeline,
    /// The `vec4` canvas modulate.
    uniform: wgpu::Buffer,
    /// Layout of `bind_group` (kept for [`CompositePass::rebind`]).
    bgl: wgpu::BindGroupLayout,
    /// Uniform + albedo + light accumulation.
    bind_group: wgpu::BindGroup,
}

/// The composite's group-0 bind group over the given albedo/accum views.
fn composite_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    albedo_view: &wgpu::TextureView,
    accum_view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("composite bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(albedo_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(accum_view),
            },
        ],
    })
}

impl CompositePass {
    /// Build the composite pipeline for `space` (its albedo format is read
    /// with `textureLoad`, so one bind group layout — `Float { filterable }`
    /// covers both `Rgba8Unorm` and the filterable `Rgba16Float` — serves
    /// both modes) and bind `albedo_view` / `accum_view`.
    fn new(
        device: &wgpu::Device,
        space: LightingSpace,
        albedo_view: &wgpu::TextureView,
        accum_view: &wgpu::TextureView,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("composite.wgsl").into()),
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("composite uniform"),
            size: size_of::<[f32; 4]>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
            ],
        });
        let bind_group = composite_bind_group(device, &bgl, &uniform, albedo_view, accum_view);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("composite pipeline layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("composite pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(match space {
                    LightingSpace::Gamma => "fs_main",
                    LightingSpace::Linear => "fs_main_linear",
                }),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            uniform,
            bgl,
            bind_group,
        }
    }

    /// Rebind freshly (re)created albedo/accum views (target resize).
    fn rebind(
        &mut self,
        device: &wgpu::Device,
        albedo_view: &wgpu::TextureView,
        accum_view: &wgpu::TextureView,
    ) {
        self.bind_group =
            composite_bind_group(device, &self.bgl, &self.uniform, albedo_view, accum_view);
    }

    /// Upload the canvas modulate (as given — sRGB in `Gamma`, linear in
    /// `Linear`).
    fn set_modulate(&self, queue: &wgpu::Queue, modulate: [f32; 4]) {
        queue.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&modulate));
    }

    /// Encode the composite into `target` (fully overwritten).
    fn run(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("composite pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    // Fully overwritten by the fullscreen triangle.
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// Pixel-area budget for the offscreen scene targets (16.7 Mpx = 4096²).
/// The renderer owns six full-size targets (albedo/normal/mask/composited
/// at 4 B/px — the albedo 8 B/px under `LightingSpace::Linear` — fp16
/// light accumulation at 8 B/px, R8 shadow mask — ~29–33 B/px total), so
/// unbounded native-res rendering scales quadratically:
/// a 5K surface (~14.7 Mpx, ~425 MiB) still fits the budget, an 8K one
/// (~33 Mpx, ~950 MiB) does not. See [`clamp_target_size`].
const MAX_TARGET_AREA: u64 = 4096 * 4096;

/// Defensive size policy for [`Renderer::set_target_size`]. Today its only
/// callers pass either the fixed logical size or the device-validated
/// surface size, so this is normally the identity — but a future caller
/// (supersampling, an 8K surface) must degrade gracefully instead of
/// tripping wgpu's max-texture validation or allocating ~1 GiB of targets:
///
/// 1. zero-guard each axis to at least 1 px;
/// 2. above [`MAX_TARGET_AREA`], apply a proportional render scale-down
///    (aspect preserved; the letterbox blit upscales the smaller target to
///    the full surface, so the frame stays correct, just softer);
/// 3. when either axis exceeds the device's max 2D texture dimension
///    (a wgpu validation failure, not a soft error), scale both axes by
///    `max_dim / max(width, height)` so the aspect is preserved and the
///    screen channel keeps laying out over the requested size.
fn clamp_target_size(width: u32, height: u32, max_dim: u32) -> (u32, u32) {
    let (width, height) = (width.max(1), height.max(1));
    let max_dim = max_dim.max(1);
    let area = u64::from(width) * u64::from(height);
    // One combined scale, floored once, so the two limits never compound
    // rounding error into the aspect.
    let area_scale = ((MAX_TARGET_AREA as f64) / (area as f64)).sqrt();
    let dim_scale = f64::from(max_dim) / f64::from(width.max(height));
    let scale = area_scale.min(dim_scale);
    if scale >= 1.0 {
        return (width, height);
    }
    let axis = |len: u32| ((f64::from(len) * scale) as u32).clamp(1, max_dim);
    (axis(width), axis(height))
}

/// The screen channel's layout for a `target` the window's `requested` size
/// was scaled to: `(layout size in UI points, target pixels per point)`.
/// The blit letterboxes the target into the window by
/// `fit = min(requested / target)` per axis (`fit_fractional`), so a point
/// that covers `ui_scale / fit` target pixels lands on `ui_scale` window
/// pixels. Exactly `target / ui_scale` and `ui_scale` when the target is the
/// requested size.
fn ui_layout(requested: (u32, u32), target: (u32, u32), ui_scale: f32) -> (glam::Vec2, f32) {
    let fit = (requested.0 as f32 / target.0 as f32).min(requested.1 as f32 / target.1 as f32);
    let px_per_point = ui_scale / fit;
    let size = glam::Vec2::new(target.0 as f32, target.1 as f32) / px_per_point;
    (size, px_per_point)
}

impl Renderer {
    /// Build the offscreen targets (`logical_width × logical_height`), the
    /// sprite/light passes, the composite, and the blit pipeline targeting
    /// `ctx`'s swapchain format, lit in the default gamma space
    /// ([`LightingSpace::Gamma`], Godot parity). `clear_color_srgb` is the
    /// scene clear color in sRGB (e.g. PR-1's `[0.3, 0.3, 0.3]`).
    pub fn new(
        ctx: &Context,
        logical_width: u32,
        logical_height: u32,
        clear_color_srgb: [f32; 3],
    ) -> Self {
        Self::with_lighting(
            ctx,
            logical_width,
            logical_height,
            clear_color_srgb,
            LightingSpace::Gamma,
        )
    }

    /// [`new`](Self::new) with an explicit [`LightingSpace`]. The space is
    /// fixed for the renderer's lifetime (it selects pipelines and the
    /// albedo target format); `clear_color_srgb` is always given in sRGB
    /// and converted to the albedo target's space here.
    pub fn with_lighting(
        ctx: &Context,
        logical_width: u32,
        logical_height: u32,
        clear_color_srgb: [f32; 3],
        lighting_space: LightingSpace,
    ) -> Self {
        Self::build(
            ctx,
            ctx.surface_format,
            logical_width,
            logical_height,
            clear_color_srgb,
            lighting_space,
        )
    }

    /// A renderer with no window: it uploads, renders the offscreen scene
    /// ([`render_scene`](Self::render_scene)) and reads it back
    /// ([`read_scene`](Self::read_scene) / [`capture_scene`](Self::capture_scene))
    /// over a [`Gpu::headless`] device. Only [`render`](Self::render) /
    /// [`render_lit`](Self::render_lit), which blit to a swapchain, need a
    /// [`Context`].
    pub fn headless(
        gpu: &Gpu,
        logical_width: u32,
        logical_height: u32,
        clear_color_srgb: [f32; 3],
        lighting_space: LightingSpace,
    ) -> Self {
        // The blit pipeline is built but never run; any sRGB format will do.
        Self::build(
            gpu,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            logical_width,
            logical_height,
            clear_color_srgb,
            lighting_space,
        )
    }

    fn build(
        gpu: &Gpu,
        surface_format: wgpu::TextureFormat,
        logical_width: u32,
        logical_height: u32,
        clear_color_srgb: [f32; 3],
        lighting_space: LightingSpace,
    ) -> Self {
        let device = &gpu.device;

        let targets = scene_targets(device, logical_width, logical_height, lighting_space);
        let SceneTargets {
            albedo_view,
            normal_view,
            mask_view,
            composited_texture,
            composited_view,
        } = targets;

        let sprites = SpritePass::new(
            device,
            &gpu.queue,
            albedo_format(lighting_space),
            SCENE_FORMAT,
            lighting_space,
        );
        let lights = LightPass::new(
            device,
            &gpu.queue,
            &normal_view,
            &mask_view,
            logical_width,
            logical_height,
        );
        let composite =
            CompositePass::new(device, lighting_space, &albedo_view, lights.accum_view());

        // --- Letterbox blit.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blit.wgsl").into()),
        });

        // Nearest sampling keeps the logical image crisp (PR-1).
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit sampler (nearest)"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit bgl"),
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
        let blit_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit bind group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&composited_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(if surface_format.is_srgb() {
                    "fs_main"
                } else {
                    "fs_main_unorm"
                }),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // The albedo target's space: raw sRGB in Gamma (the target holds
        // gamma-space values, D-8), decoded to linear in Linear.
        let channel = |c: f32| match lighting_space {
            LightingSpace::Gamma => f64::from(c),
            LightingSpace::Linear => f64::from(srgb_to_linear(c)),
        };
        Self {
            logical_width,
            logical_height,
            requested_size: (logical_width, logical_height),
            max_texture_dim: device.limits().max_texture_dimension_2d,
            ui_scale: 1.0,
            lighting_space,
            clear_color: wgpu::Color {
                r: channel(clear_color_srgb[0]),
                g: channel(clear_color_srgb[1]),
                b: channel(clear_color_srgb[2]),
                a: 1.0,
            },
            albedo_view,
            normal_view,
            mask_view,
            composited_texture,
            composited_view,
            sprites,
            lights,
            composite,
            blit_pipeline,
            blit_bgl: bind_group_layout,
            blit_sampler: sampler,
            blit_bind_group,
        }
    }

    /// The color space the world channel is lit and composited in.
    pub fn lighting_space(&self) -> LightingSpace {
        self.lighting_space
    }

    /// A view of the composited scene target (lit scene + UI), for extra
    /// passes (dev overlay, R-11+) to render into between the composite and
    /// the blit.
    pub fn scene_view(&self) -> &wgpu::TextureView {
        &self.composited_view
    }

    /// The offscreen target size in pixels.
    pub fn target_size(&self) -> (u32, u32) {
        (self.logical_width, self.logical_height)
    }

    /// Resize the offscreen scene targets to a caller-selected resolution.
    /// No-op when the size is unchanged. Uploaded sprite
    /// pages, light cookies, and every pipeline survive — only the render
    /// targets and their bind groups are rebuilt.
    ///
    /// Requested dimensions pass through the target-size policy (device
    /// max-texture clamp + proportional scale-down above the pixel-area
    /// budget); the letterbox blit absorbs any difference, and the screen
    /// channel keeps laying out over the requested size
    /// ([`ui_size`](Self::ui_size)), so UI points still match the window.
    pub fn set_target_size(&mut self, gpu: &Gpu, width: u32, height: u32) {
        self.requested_size = (width.max(1), height.max(1));
        let (width, height) = clamp_target_size(width, height, self.max_texture_dim);
        if (width, height) == (self.logical_width, self.logical_height) {
            return;
        }
        let device = &gpu.device;
        let targets = scene_targets(device, width, height, self.lighting_space);
        self.lights.resize(
            device,
            &targets.normal_view,
            &targets.mask_view,
            width,
            height,
        );
        self.composite
            .rebind(device, &targets.albedo_view, self.lights.accum_view());
        self.blit_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit bind group"),
            layout: &self.blit_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&targets.composited_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                },
            ],
        });
        self.albedo_view = targets.albedo_view;
        self.normal_view = targets.normal_view;
        self.mask_view = targets.mask_view;
        self.composited_texture = targets.composited_texture;
        self.composited_view = targets.composited_view;
        self.logical_width = width;
        self.logical_height = height;
    }

    /// Set the physical-pixels-per-UI-point factor for the screen channel
    /// (D-26). At scale `s` the screen channel lays out over a
    /// `requested size / s` point space ([`ui_size`](Self::ui_size)); `1.0`
    /// (the default, and the game scenes' value) reproduces the pre-scale
    /// behavior bit-for-bit.
    pub fn set_ui_scale(&mut self, scale: f32) {
        self.ui_scale = scale.max(0.25);
    }

    /// The screen channel's layout size in UI points: the size requested of
    /// [`set_target_size`](Self::set_target_size) over the UI scale, so a
    /// window's logical size even when the target is scaled down. Lay the
    /// UI out over this, not [`target_size`](Self::target_size).
    #[must_use]
    pub fn ui_size(&self) -> glam::Vec2 {
        self.ui_layout().0
    }

    /// Target pixels per UI point: the UI scale, raised when a very large
    /// target is rendered scaled down. Set the text raster scale
    /// (`TextRenderer::set_pixel_scale`) to this so glyphs rasterize at the
    /// target's density.
    #[must_use]
    pub fn ui_pixel_scale(&self) -> f32 {
        self.ui_layout().1
    }

    fn ui_layout(&self) -> (glam::Vec2, f32) {
        ui_layout(
            self.requested_size,
            (self.logical_width, self.logical_height),
            self.ui_scale,
        )
    }

    /// Upload a decoded texture to the sprite pass under its asset `handle`
    /// (atlas-packed when small). Call once per texture after loading,
    /// before it appears in a [`DrawList`]. Uploading a handle that is
    /// already on the GPU replaces its pixels as [`Self::replace_texture`]
    /// does, so a texture changed under its handle (a glyph page from
    /// `TextRenderer::end_frame`) reaches the GPU without a new texture.
    /// An invalid texture (empty, larger than the device allows, or with an
    /// `rgba` length that does not match its dimensions) is refused with a
    /// [`TextureError`].
    pub fn upload_texture(
        &mut self,
        gpu: &Gpu,
        handle: Handle<Texture>,
        tex: &Texture,
    ) -> Result<(), TextureError> {
        self.sprites.upload(&gpu.device, &gpu.queue, handle, tex)
    }

    /// Replace a sprite texture's GPU pixels while keeping its asset `handle`
    /// valid for existing draw data; `Ok(false)` when the handle is unknown
    /// and a [`TextureError`], changing nothing, for an invalid texture.
    /// Follows [`SpritePass::replace`](super::sprite::SpritePass::replace):
    /// equal dimensions update in place, while changed dimensions relocate
    /// the texture and detach its normal map, which
    /// [`Self::upload_normal_map`] registers again.
    pub fn replace_texture(
        &mut self,
        gpu: &Gpu,
        handle: Handle<Texture>,
        tex: &Texture,
    ) -> Result<bool, TextureError> {
        self.sprites.replace(&gpu.device, &gpu.queue, handle, tex)
    }

    /// The shared 1×1 white texture, registered in `assets` and uploaded to
    /// the sprite pass, ready to appear in a [`DrawList`].
    ///
    /// Idempotent, and the same handle [`crate::assets::white_texture`] and
    /// `Ui::new` resolve — so a renderer-only consumer (an overlay, a screen
    /// fade) gets the flat-quad texture without hand-inserting one and
    /// without a second atlas entry. Pass the game's own texture cache: the
    /// renderer keys textures by handle, and a second cache's handles collide
    /// with it.
    pub fn white_texture(&mut self, gpu: &Gpu, assets: &mut Assets<Texture>) -> Handle<Texture> {
        let handle = crate::assets::white_texture(assets);
        if !self.sprites.has_texture(handle)
            && let Some(tex) = assets.get(handle)
        {
            self.sprites
                .upload(&gpu.device, &gpu.queue, handle, tex)
                .expect("the shared white texture is a valid 1x1 texture");
        }
        handle
    }

    /// Register `normal` as the companion normal map of the already-uploaded
    /// `diffuse` texture (R-6, PR-4). Sprites then opt in by setting their
    /// `normal` field to this handle. The normal map must match the
    /// diffuse's dimensions, or a [`TextureError`] refuses it.
    pub fn upload_normal_map(
        &mut self,
        gpu: &Gpu,
        diffuse: Handle<Texture>,
        normal: Handle<Texture>,
        tex: &Texture,
    ) -> Result<(), TextureError> {
        self.sprites
            .upload_normal(&gpu.device, &gpu.queue, diffuse, normal, tex)
    }

    /// Upload a light-cookie texture (PR-4 `assets/lights/*.png`) for use by
    /// [`PointLight`](crate::canvas::light::PointLight)s. Idempotent per
    /// handle; lights referencing an unregistered cookie are skipped. An
    /// invalid texture is refused with a [`TextureError`].
    pub fn upload_light_cookie(
        &mut self,
        gpu: &Gpu,
        handle: Handle<Texture>,
        tex: &Texture,
    ) -> Result<(), TextureError> {
        self.lights
            .upload_cookie(&gpu.device, &gpu.queue, handle, tex)
    }

    /// Render one frame **unlit**: identity lighting (white modulate, no
    /// lights), so the scene passes through unchanged — menus and demos
    /// without lighting call this. See [`render_lit`](Self::render_lit).
    pub fn render(&mut self, ctx: &Context, frame: &Frame, list: &mut DrawList, camera: &Camera) {
        self.render_lit(ctx, Some(frame), list, camera, &LightFrame::default());
    }

    /// Render one frame (R-6): the world channel into the albedo/normal/mask
    /// targets through `camera`, the light passes from `lighting`, the
    /// composite (`albedo × (modulate + lights)`), the screen channel (UI —
    /// unlit) on top, then the letterbox blit into the swapchain `frame`
    /// (bars are the swapchain clear, black). Sorts `list` by z (AR-6/PR-2).
    /// Submits to `ctx`'s queue; the caller presents.
    ///
    /// `frame` is `None` when no swapchain texture is available (e.g. the
    /// window is occluded): the offscreen scene still renders — so
    /// [`capture_scene`](Self::capture_scene) stays fresh — and only the
    /// final blit is skipped.
    pub fn render_lit(
        &mut self,
        ctx: &Context,
        frame: Option<&Frame>,
        list: &mut DrawList,
        camera: &Camera,
        lighting: &LightFrame,
    ) {
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            });
        self.encode_scene(ctx, &mut encoder, list, camera, lighting);

        // --- Blit pass: letterboxed copy into the swapchain (skipped when
        // no swapchain frame is available).
        let (win_w, win_h) = ctx.size();
        let letterbox = fit_fractional(win_w, win_h, self.logical_width, self.logical_height);
        if let Some(frame) = frame {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("letterbox blit pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &frame.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // The bars: cleared black; the viewport limits the blit.
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_viewport(
                letterbox.x,
                letterbox.y,
                letterbox.width,
                letterbox.height,
                0.0,
                1.0,
            );
            pass.set_pipeline(&self.blit_pipeline);
            pass.set_bind_group(0, &self.blit_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        ctx.queue.submit(Some(encoder.finish()));
    }

    /// Render the offscreen scene only — every pass of
    /// [`render_lit`](Self::render_lit) except the letterbox blit — and
    /// submit it. Needs no window: a [`headless`](Self::headless) renderer
    /// draws this way and reads the result with [`read_scene`](Self::read_scene).
    /// Sorts `list` by z.
    pub fn render_scene(
        &mut self,
        gpu: &Gpu,
        list: &mut DrawList,
        camera: &Camera,
        lighting: &LightFrame,
    ) {
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("scene encoder"),
            });
        self.encode_scene(gpu, &mut encoder, list, camera, lighting);
        gpu.queue.submit(Some(encoder.finish()));
    }

    /// Encode the scene, light, composite and screen passes into `encoder`.
    fn encode_scene(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        list: &mut DrawList,
        camera: &Camera,
        lighting: &LightFrame,
    ) {
        list.sort();

        let world_vp = camera.view_proj();
        // The screen channel lays out in UI points (`ui_size`); game scenes
        // run at scale 1.0 on an unscaled target where `x / 1.0 == x`
        // exactly — the matrix and scissors are bit-identical to the
        // fixed-logical path.
        let (ui_size, ui_scale) = self.ui_layout();
        let screen_vp = Camera::screen_view_proj_size(ui_size);
        // The world channel and lights are laid out in the camera's units
        // (pixels unless `Camera::with_units`); the screen channel is pixels.
        let world_units = camera.units();
        self.sprites.prepare(
            &gpu.device,
            &gpu.queue,
            list,
            world_vp,
            world_units,
            screen_vp,
            (self.logical_width, self.logical_height),
            ui_scale,
        );
        self.lights.prepare(
            &gpu.device,
            &gpu.queue,
            world_vp,
            world_units,
            lighting,
            (self.logical_width, self.logical_height),
        );
        // CanvasModulate uploads as given: sRGB under Gamma (Godot authors
        // it in sRGB and composites in gamma space, D-8 / R-13 parity),
        // linear under Linear.
        self.composite
            .set_modulate(&gpu.queue, lighting.canvas_modulate);

        // --- Scene pass (world channel, MRT): albedo + normal + mask.
        {
            fn attachment(
                view: &wgpu::TextureView,
                clear: wgpu::Color,
            ) -> wgpu::RenderPassColorAttachment<'_> {
                wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                }
            }
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene pass (MRT)"),
                color_attachments: &[
                    Some(attachment(&self.albedo_view, self.clear_color)),
                    // Flat +Z normal encoding under everything.
                    Some(attachment(
                        &self.normal_view,
                        wgpu::Color {
                            r: 0.5,
                            g: 0.5,
                            b: 1.0,
                            a: 0.0,
                        },
                    )),
                    // The background receives no light and has no normal map.
                    Some(attachment(&self.mask_view, wgpu::Color::TRANSPARENT)),
                ],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.sprites.draw_world(&mut pass);
        }

        // --- Light accumulation (+ per-light shadow masks).
        self.lights.run(encoder);

        // --- Composite: albedo × (modulate + light accumulation), encoded
        // to sRGB once here under Linear.
        self.composite.run(encoder, &self.composited_view);

        // --- Screen channel (UI/HUD) over the lit scene — bypasses lighting.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("screen sprite pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.composited_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.sprites.draw_screen(&mut pass);
        }
    }

    /// Read the composited scene target (lit scene + UI) back as tightly
    /// packed, fully opaque RGBA8 rows — the sRGB-encoded bytes the
    /// swapchain shows, in either lighting space. Reflects the last
    /// submitted frame; blocks until the GPU copy completes.
    pub fn read_scene(&self, gpu: &Gpu) -> Result<Vec<u8>, CaptureError> {
        let (w, h) = (self.logical_width, self.logical_height);
        let unpadded = 4 * w;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT; // 256
        let padded = unpadded.div_ceil(align) * align;

        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene capture buffer"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("scene capture encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.composited_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit(Some(encoder.finish()));

        // Map + block until the copy lands.
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| CaptureError::Readback(format!("poll: {e}")))?;
        rx.recv()
            .map_err(|_| CaptureError::Readback("map callback dropped".into()))?
            .map_err(|e| CaptureError::Readback(format!("map: {e}")))?;

        // Strip row padding; opaque alpha.
        let data = buffer
            .slice(..)
            .get_mapped_range()
            .expect("mapped readback buffer");
        let mut pixels = Vec::with_capacity((unpadded * h) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            for px in data[start..start + unpadded as usize].chunks_exact(4) {
                pixels.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
        drop(data);
        buffer.unmap();
        Ok(pixels)
    }

    /// [`read_scene`](Self::read_scene) written to `path` as a PNG (AR-9:
    /// the visual-parity screenshot harness, AC-PR-1).
    pub fn capture_scene(&self, gpu: &Gpu, path: &Path) -> Result<(), CaptureError> {
        let (w, h) = (self.logical_width, self.logical_height);
        let pixels = self.read_scene(gpu)?;
        image::save_buffer(path, &pixels, w, h, image::ExtendedColorType::Rgba8)
            .map_err(CaptureError::Encode)
    }

    /// The GPU allocations that must persist across frames (rendering.md
    /// 4): the sprite instance buffer and shadow vertex buffer grow only
    /// past their capacity, and texture pages only open for new uploads.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn resource_stats(&self) -> ResourceStats {
        ResourceStats {
            instance_capacity: self.sprites.instance_capacity(),
            sprite_pages: self.sprites.page_count(),
            shadow_capacity: self.lights.shadow_capacity(),
        }
    }
}

/// See [`Renderer::resource_stats`].
#[cfg(all(test, not(target_arch = "wasm32")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResourceStats {
    pub instance_capacity: u64,
    pub sprite_pages: usize,
    pub shadow_capacity: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Every size the current callers actually pass — the fixed logical
    /// target, common desktop surfaces, and 5K native — is inside both
    /// limits, so the clamp is the identity there (D-26: today the policy
    /// is defensive only).
    #[wasm_bindgen_test(unsupported = test)]
    fn clamp_target_size_passes_real_surfaces_through() {
        for (w, h) in [
            (960, 540),
            (1920, 1080),
            (2560, 1440),
            (3840, 2160),
            (5120, 2880), // 5K native: ~14.7 Mpx, still under the budget.
        ] {
            assert_eq!(clamp_target_size(w, h, 8192), (w, h), "{w}x{h}");
        }
    }

    /// Above the pixel-area budget the size scales down proportionally:
    /// area lands at (or just under) the budget and the aspect ratio is
    /// preserved — an 8K surface renders smaller and blits up rather than
    /// allocating ~1 GiB of offscreen targets.
    #[wasm_bindgen_test(unsupported = test)]
    fn clamp_target_size_scales_down_above_the_area_budget() {
        let (w, h) = clamp_target_size(7680, 4320, 16_384);
        let area = u64::from(w) * u64::from(h);
        assert!(area <= MAX_TARGET_AREA, "area over budget: {area}");
        assert!(
            area > MAX_TARGET_AREA * 95 / 100,
            "scaled far below the budget: {area}"
        );
        let aspect = f64::from(w) / f64::from(h);
        assert!(
            (aspect - 16.0 / 9.0).abs() < 0.01,
            "aspect not preserved: {aspect}"
        );
    }

    /// A target over the device's max texture dimension shrinks to it on
    /// its long axis even when the area budget allows the request (a
    /// 20000×100 request on an 8192-limit device would otherwise fail wgpu
    /// validation), and the short axis shrinks with it so the aspect holds.
    #[wasm_bindgen_test(unsupported = test)]
    fn clamp_target_size_respects_the_device_dimension_limit() {
        assert_eq!(clamp_target_size(20_000, 100, 8192), (8192, 40));
        assert_eq!(clamp_target_size(100, 20_000, 8192), (40, 8192));
    }

    /// #433: a triple-4K span (and its tall equivalent) over an 8192-limit
    /// device keeps the window's 16:3 aspect, so the screen channel still
    /// lays out over the window's logical size instead of letterboxing.
    #[wasm_bindgen_test(unsupported = test)]
    fn clamping_to_the_device_limit_keeps_the_window_aspect() {
        for requested in [(11_520, 2160), (2160, 11_520)] {
            let target = clamp_target_size(requested.0, requested.1, 8192);
            assert!(target.0 <= 8192 && target.1 <= 8192, "{target:?}");
            let aspect = |(w, h): (u32, u32)| f64::from(w) / f64::from(h);
            assert!(
                (aspect(target) / aspect(requested) - 1.0).abs() < 0.005,
                "{requested:?} -> {target:?}"
            );
            let (layout, _) = ui_layout(requested, target, 1.0);
            let window = Vec2::new(requested.0 as f32, requested.1 as f32);
            assert!(
                (layout - window).abs().max_element() < 1.0,
                "{requested:?} lays out over {layout}"
            );
        }
    }

    /// #326: an 8K window at UI scale 2 renders into a scaled-down target,
    /// yet the UI still lays out over the window's 3840×2160 points, and a
    /// widget drawn at a point is under the pointer at that point after the
    /// letterbox blit (pointer points are window pixels / UI scale).
    #[wasm_bindgen_test(unsupported = test)]
    fn scaled_down_target_keeps_the_window_ui_layout() {
        let requested = (7680, 4320);
        let target = clamp_target_size(requested.0, requested.1, 16384);
        assert!(target.0 < requested.0, "the target is scaled down");
        let ui_scale = 2.0;
        let (layout, scale) = ui_layout(requested, target, ui_scale);
        assert!(
            (layout - Vec2::new(3840.0, 2160.0)).abs().max_element() < 0.5,
            "{layout}"
        );

        let letterbox = fit_fractional(requested.0, requested.1, target.0, target.1);
        let blit = letterbox.width / target.0 as f32;
        for point in [
            Vec2::new(0.0, 0.0),
            Vec2::new(1000.0, 500.0),
            Vec2::new(3839.0, 2159.0),
        ] {
            let window = Vec2::new(letterbox.x, letterbox.y) + point * scale * blit;
            let pointer = window / ui_scale;
            assert!(
                (pointer - point).abs().max_element() < 0.5,
                "{point} -> {pointer}"
            );
        }
    }

    /// Degenerate inputs stay valid: zero axes become 1 px and a bogus
    /// zero device limit never produces a zero-sized texture.
    #[wasm_bindgen_test(unsupported = test)]
    fn clamp_target_size_never_returns_zero() {
        assert_eq!(clamp_target_size(0, 0, 8192), (1, 1));
        assert_eq!(clamp_target_size(0, 540, 8192), (1, 540));
        let (w, h) = clamp_target_size(960, 540, 0);
        assert!(w >= 1 && h >= 1);
    }

    /// Hand-derived linear composites (IEC 61966-2-1 encode):
    /// - albedo 0.5 under a white modulate → `1.055·0.5^(1/2.4) − 0.055`
    ///   = `1.055·0.749154 − 0.055` ≈ `0.735357`;
    /// - albedo 0.5 × (modulate 0.2 + accum 0.3) = 0.25 →
    ///   `1.055·0.561231 − 0.055` ≈ `0.537099`;
    /// - shadow-sp's Dark ambient red channel: `#8A` = 138/255 decodes to
    ///   linear `0.254131`, × brightness 0.003 = `0.000762` on a white
    ///   albedo stays on the linear toe → `× 12.92` ≈ `0.009850`;
    /// - an over-bright sum clamps to white before encoding (`1.055 − 0.055`
    ///   is one f32 ulp under 1, hence the tolerance); nothing goes below
    ///   black.
    #[wasm_bindgen_test(unsupported = test)]
    fn composite_linear_matches_hand_values() {
        let [r, _, _] = composite_linear([0.5; 3], [1.0; 3], [0.0; 3]);
        assert!((r - 0.735_357).abs() < 1e-4, "{r}");

        let [r, _, _] = composite_linear([0.5; 3], [0.2; 3], [0.3; 3]);
        assert!((r - 0.537_099).abs() < 1e-4, "{r}");

        let [r, _, _] = composite_linear([1.0; 3], [0.254_131 * 0.003; 3], [0.0; 3]);
        assert!((r - 0.009_850).abs() < 1e-5, "{r}");

        let white = composite_linear([1.0; 3], [1.0; 3], [5.0; 3]);
        assert!(
            white.iter().all(|c| (c - 1.0).abs() < 1e-6),
            "clamps to white: {white:?}"
        );
        assert_eq!(
            composite_linear([0.5; 3], [-1.0; 3], [0.0; 3]),
            [0.0; 3],
            "clamps to black"
        );
    }
}

/// Headless GPU tests: the real composite pipelines over 1×1 inputs.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod gpu_tests {
    use super::*;
    use crate::assets::Assets;
    use crate::canvas::draw::SpriteInstance;
    use crate::canvas::light::ACCUM_FORMAT;
    use crate::canvas::light::PointLight;
    use crate::canvas::sprite::MAX_ATLAS_DIM;
    use crate::canvas::test_gpu::{device, gpu, read_texture};
    use crate::canvas::{Overlay, Rect, WorldUnits};
    use glam::Vec2;
    use std::path::PathBuf;
    use wgpu::util::DeviceExt;

    fn flat(size: u32, rgba: [u8; 4]) -> Texture {
        Texture {
            width: size,
            height: size,
            rgba: rgba.repeat((size * size) as usize),
        }
    }

    /// rendering.md 4: pipelines, pages and buffers persist across frames.
    /// Identical frames leave every allocation alone; a frame past the
    /// instance capacity grows the buffer once and later, smaller frames
    /// never shrink it; a small upload packs into the open atlas page and
    /// only an oversized one opens another.
    #[test]
    fn allocations_persist_across_frames_and_grow_only_when_exceeded() {
        let Some(gpu) = gpu() else { return };
        let mut renderer = Renderer::headless(&gpu, 32, 32, [0.0; 3], LightingSpace::Gamma);
        let mut assets: Assets<Texture> = Assets::new();
        let white = renderer.white_texture(&gpu, &mut assets);
        let camera = Camera::new(32, 32);
        let frame = |sprites: usize| {
            let mut list = DrawList::new();
            for i in 0..sprites {
                list.push(SpriteInstance::new(white, Vec2::new((i % 32) as f32, 8.0)));
            }
            list
        };
        let lighting = LightFrame {
            lights: vec![PointLight {
                shadows: true,
                ..PointLight::analytic(Vec2::new(16.0, 16.0), 20.0, 1.0)
            }],
            occluders: vec![vec![
                Vec2::new(10.0, 20.0),
                Vec2::new(14.0, 20.0),
                Vec2::new(14.0, 24.0),
                Vec2::new(10.0, 24.0),
            ]],
            ..LightFrame::default()
        };

        renderer.render_scene(&gpu, &mut frame(10), &camera, &lighting);
        let baseline = renderer.resource_stats();
        assert_eq!(
            baseline.sprite_pages, 1,
            "the white pixel opens one atlas page"
        );
        for _ in 0..100 {
            renderer.render_scene(&gpu, &mut frame(10), &camera, &lighting);
        }
        assert_eq!(
            renderer.resource_stats(),
            baseline,
            "identical frames reallocated"
        );

        let over = usize::try_from(baseline.instance_capacity).unwrap() + 1;
        renderer.render_scene(&gpu, &mut frame(over), &camera, &lighting);
        let grown = renderer.resource_stats();
        assert!(
            grown.instance_capacity >= over as u64,
            "buffer too small for the frame"
        );
        assert!(grown.instance_capacity > baseline.instance_capacity);
        for sprites in [over, over, 3, 0, over] {
            renderer.render_scene(&gpu, &mut frame(sprites), &camera, &lighting);
        }
        assert_eq!(
            renderer.resource_stats(),
            grown,
            "buffer churned after growing"
        );

        let small = assets.insert(PathBuf::from("small.png"), flat(8, [255, 0, 0, 255]));
        renderer
            .upload_texture(&gpu, small, assets.get(small).unwrap())
            .unwrap();
        assert_eq!(renderer.resource_stats().sprite_pages, grown.sprite_pages);
        let big = assets.insert(
            PathBuf::from("big.png"),
            flat(MAX_ATLAS_DIM + 1, [0, 255, 0, 255]),
        );
        renderer
            .upload_texture(&gpu, big, assets.get(big).unwrap())
            .unwrap();
        assert_eq!(
            renderer.resource_stats().sprite_pages,
            grown.sprite_pages + 1
        );
    }

    /// #384: text that rasterizes a new glyph every frame keeps one asset and
    /// one GPU texture per glyph page, and each frame's new glyph still
    /// reaches the GPU.
    #[test]
    fn glyph_pages_update_in_place_across_frames() {
        use crate::canvas::text::{TextChannel, TextRenderer, TextStyle};
        let Some(gpu) = gpu() else { return };
        let mut renderer = Renderer::headless(&gpu, 64, 64, [0.0; 3], LightingSpace::Gamma);
        let mut assets: Assets<Texture> = Assets::new();
        let mut text = TextRenderer::new(include_bytes!(
            "../../tests/fixtures/IBMPlexSans-Regular.ttf"
        ))
        .expect("test font should parse");
        let camera = Camera::new(64, 64);
        let style = TextStyle::new(32.0, [1.0; 4]);
        let mut handles = std::collections::HashSet::new();
        let mut pages = None;
        for c in 'A'..='Z' {
            let mut list = DrawList::new();
            let glyph = c.to_string();
            text.draw(
                &glyph,
                Vec2::new(16.0, 8.0),
                &style,
                0.0,
                TextChannel::Screen,
            );
            let changed = text.end_frame(&mut assets, &mut list);
            assert_eq!(changed.len(), 1, "{c} is a new glyph");
            for page in changed {
                handles.insert(page);
                renderer
                    .upload_texture(&gpu, page, assets.get(page).unwrap())
                    .unwrap();
            }
            renderer.render_scene(&gpu, &mut list, &camera, &LightFrame::default());
            let sprite_pages = renderer.resource_stats().sprite_pages;
            assert_eq!(
                *pages.get_or_insert(sprite_pages),
                sprite_pages,
                "{c} opened another GPU texture"
            );
            let pixels = renderer.read_scene(&gpu).unwrap();
            assert!(
                pixels.chunks(4).any(|p| p[..3].iter().all(|&v| v > 200)),
                "{c} did not reach the GPU"
            );
        }
        assert_eq!(text.page_count(), 1);
        assert_eq!(handles.len(), 1, "the glyph page took more than one asset");
    }

    fn texture_1x1(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        data: &[u8],
    ) -> wgpu::Texture {
        device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("composite test input"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            data,
        )
    }

    /// Run `space`'s composite over a 1×1 albedo (`albedo` bytes in that
    /// space's albedo format), a 1×1 accumulation (`accum` as `Rgba16Float`
    /// bytes) and `modulate`; return the composited rgba8 pixel. `None`
    /// when the host has no adapter.
    fn composite_1x1(
        space: LightingSpace,
        albedo: &[u8],
        accum: &[u8],
        modulate: [f32; 4],
    ) -> Option<[u8; 4]> {
        let (device, queue) = device()?;
        let albedo = texture_1x1(&device, &queue, albedo_format(space), albedo)
            .create_view(&wgpu::TextureViewDescriptor::default());
        let accum = texture_1x1(&device, &queue, ACCUM_FORMAT, accum)
            .create_view(&wgpu::TextureViewDescriptor::default());
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("composite test target"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SCENE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let composite = CompositePass::new(&device, space, &albedo, &accum);
        composite.set_modulate(&queue, modulate);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        composite.run(
            &mut encoder,
            &target.create_view(&wgpu::TextureViewDescriptor::default()),
        );
        queue.submit(Some(encoder.finish()));
        let bytes = read_texture(&device, &queue, &target, 1, 1, 4);
        Some([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// binary16 1.0 (`0x3C00`), 0.5 (`0x3800`), 0.3 (`0x34CD`: exponent
    /// 2⁻² with mantissa 205/1024 → 0.300049) and 0, little-endian.
    const F16_ONE: [u8; 2] = 0x3C00u16.to_le_bytes();
    const F16_HALF: [u8; 2] = 0x3800u16.to_le_bytes();
    const F16_0_3: [u8; 2] = 0x34CDu16.to_le_bytes();
    const F16_ZERO: [u8; 2] = [0, 0];

    fn rgba16f(r: [u8; 2], g: [u8; 2], b: [u8; 2], a: [u8; 2]) -> Vec<u8> {
        [r, g, b, a].concat()
    }

    /// The linear composite encodes once: a linear-0.5 albedo under a white
    /// modulate reads back as `linear_to_srgb(0.5) × 255 = 187.5` (187 or
    /// 188), and adding light in linear (albedo 0.5 × (0.2 + 0.3) = 0.25)
    /// reads `0.5371 × 255 ≈ 137`.
    #[test]
    fn linear_composite_encodes_to_srgb_once() {
        let Some(px) = composite_1x1(
            LightingSpace::Linear,
            &rgba16f(F16_HALF, F16_HALF, F16_HALF, F16_ONE),
            &rgba16f(F16_ZERO, F16_ZERO, F16_ZERO, F16_ZERO),
            [1.0, 1.0, 1.0, 1.0],
        ) else {
            return;
        };
        for c in &px[..3] {
            assert!((f32::from(*c) - 187.5).abs() <= 1.0, "{px:?}");
        }
        assert_eq!(px[3], 255);

        let Some(px) = composite_1x1(
            LightingSpace::Linear,
            &rgba16f(F16_HALF, F16_HALF, F16_HALF, F16_ONE),
            &rgba16f(F16_0_3, F16_0_3, F16_0_3, F16_ZERO),
            [0.2, 0.2, 0.2, 1.0],
        ) else {
            return;
        };
        for c in &px[..3] {
            assert!((f32::from(*c) - 137.0).abs() <= 1.0, "{px:?}");
        }
    }

    /// #324: a world-channel overlay fill under a 32 px/unit, y-up camera
    /// covers its rect in world units. The camera centers (1, 1) in a 64 px
    /// view, so world `(x, y)` lands at pixel `(32 + 32(x - 1), 32 - 32(y - 1))`
    /// and the rect `x ∈ [0.25, 1.25], y ∈ [0.5, 1.25]` covers columns
    /// 8..40 and rows 24..48.
    #[test]
    fn world_overlay_fill_spans_world_units() {
        let Some(gpu) = gpu() else { return };
        let mut renderer = Renderer::headless(&gpu, 64, 64, [0.0; 3], LightingSpace::Gamma);
        let mut assets: Assets<Texture> = Assets::new();
        let white = renderer.white_texture(&gpu, &mut assets);
        let units = WorldUnits {
            pixels_per_unit: 32.0,
            y_up: true,
        };
        let mut camera = Camera::new(64, 64).with_units(units);
        camera.center = Vec2::ONE;
        let mut list = DrawList::new();
        Overlay {
            color: [1.0, 0.0, 0.0, 1.0],
            units,
            ..Overlay::new(white)
        }
        .fill_rect(&mut list.world, Rect::new(0.25, 0.5, 1.0, 0.75));
        renderer.render_scene(&gpu, &mut list, &camera, &LightFrame::default());
        let pixels = renderer.read_scene(&gpu).expect("scene readback");
        let red: Vec<(usize, usize)> = pixels
            .chunks_exact(4)
            .enumerate()
            .filter(|(_, p)| p[0] > 128)
            .map(|(i, _)| (i % 64, i / 64))
            .collect();
        let columns = red.iter().map(|p| p.0);
        let rows = red.iter().map(|p| p.1);
        assert_eq!(red.len(), 32 * 24, "covered pixels");
        assert_eq!((columns.clone().min(), columns.max()), (Some(8), Some(39)));
        assert_eq!((rows.clone().min(), rows.max()), (Some(24), Some(47)));
    }

    /// The gamma composite stores the multiply-add unchanged: an 8-bit 128
    /// albedo under a white modulate reads back 128 — the same inputs the
    /// linear mode maps to 187/188 — and adding gamma-space light
    /// (128/255 × (0.2 + 0.3) = 0.251 → 64) is a plain product.
    #[test]
    fn gamma_composite_stores_the_product_unencoded() {
        let Some(px) = composite_1x1(
            LightingSpace::Gamma,
            &[128, 128, 128, 255],
            &rgba16f(F16_ZERO, F16_ZERO, F16_ZERO, F16_ZERO),
            [1.0, 1.0, 1.0, 1.0],
        ) else {
            return;
        };
        for c in &px[..3] {
            assert!((f32::from(*c) - 128.0).abs() <= 1.0, "{px:?}");
        }
        assert_eq!(px[3], 255);

        let Some(px) = composite_1x1(
            LightingSpace::Gamma,
            &[128, 128, 128, 255],
            &rgba16f(F16_0_3, F16_0_3, F16_0_3, F16_ZERO),
            [0.2, 0.2, 0.2, 1.0],
        ) else {
            return;
        };
        for c in &px[..3] {
            assert!((f32::from(*c) - 64.0).abs() <= 1.0, "{px:?}");
        }
    }
}
