//! The frame's sizes and the targets stages share, each defined once, which
//! the renderer creates and lends. A stage's own targets (bloom, AO,
//! reflection history) are its own.
use crate::shading::gbuffer;

/// The frame's sizes. Every pass up to antialiasing renders at `render`;
/// antialiasing writes `scene` (they differ only while FSR2 upscales), which
/// motion blur, bloom, SMAA and tone mapping use; presentation writes
/// `output`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sizes {
    pub render: [u32; 2],
    pub scene: [u32; 2],
    pub output: [u32; 2],
}

/// The format of FSR2's reactive and transparency-and-composition masks, as
/// AMD's FSR sample allocates them (`samples/fsrapi/config/fsrapiconfig.json`).
pub(crate) const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

/// The reactive and transparency-and-composition mask targets a transparent
/// draw adds after its colour, as AMD's FSR sample adds them to its
/// translucency pass (`samples/fsrapi/fsrapirendermodule.cpp:93–101`, MIT,
/// see `LICENSE-amd-fidelityfx.txt`): each
/// draw's value `m` accumulates in red as `m (1 - mask) + mask`.
pub(crate) fn mask_targets() -> [Option<wgpu::ColorTargetState>; 2] {
    let blend = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::OneMinusDst,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::Zero,
            operation: wgpu::BlendOperation::Add,
        },
    };
    [0, 1].map(|_| {
        Some(wgpu::ColorTargetState {
            format: MASK_FORMAT,
            blend: Some(blend),
            write_mask: wgpu::ColorWrites::RED,
        })
    })
}

/// The reactive and transparency-and-composition mask targets of the draw
/// that marks moving opaque surfaces, as AMD's FSR sample's animated
/// textures draw them (`framework/rendermodules/animatedtextures/`
/// `animatedtexturesrendermodule.cpp` 81–90, MIT, see
/// `LICENSE-amd-fidelityfx.txt`): the reactive mask kept as it is, the
/// transparency and composition mask written in red, unblended.
pub(crate) fn composition_targets() -> [Option<wgpu::ColorTargetState>; 2] {
    [wgpu::ColorWrites::empty(), wgpu::ColorWrites::RED].map(|write_mask| {
        Some(wgpu::ColorTargetState {
            format: MASK_FORMAT,
            blend: None,
            write_mask,
        })
    })
}

/// The targets opaque writes and later stages read, at the render size.
pub(crate) struct SharedTargets {
    pub color: wgpu::TextureView,
    /// RGBA16F: the ambient diffuse within `color` before occlusion, which
    /// source completion occludes (shading/gbuffer.wgsl).
    pub ambient: wgpu::TextureView,
    pub source_id: wgpu::TextureView,
    pub depth: wgpu::TextureView,
    /// RGBA16F: signed octahedral world-space base normal in RG and coat normal in BA.
    pub normal: wgpu::TextureView,
    /// RGBA16F: coat roughness, base roughness, coat strength, base F90.
    pub material: wgpu::TextureView,
    /// Resolved world anisotropy tangent, signed octahedral in RG, its
    /// strength in B and the environment scale in A.
    pub anisotropy: wgpu::TextureView,
    pub f0: wgpu::TextureView,
    pub motion: wgpu::TextureView,
    /// The beauty reflections complete.
    pub composite: wgpu::TextureView,
    /// R8: FSR2's reactive mask and its transparency and composition mask,
    /// written by the composite's transparent draws while FSR2 runs.
    pub fsr2_masks: [wgpu::TextureView; 2],
    /// The surface's own targets, from the first frame of a scene that
    /// holds a blended receiver of screen-space reflections.
    pub surface: Option<SurfaceTargets>,
}

/// The targets of the Surface contract (specs/sgl3d-architecture.md) that
/// the receiver pass draws, at the render size.
pub(crate) struct SurfaceTargets {
    /// The surface depth: a copy of the opaque depth with the receivers
    /// drawn over it, the nearest winning.
    pub depth: wgpu::TextureView,
    /// RGBA16F: the receiver layer, the traced lobe's normal and perceptual
    /// roughness at the pixels a receiver covers (shading/gbuffer.wgsl).
    pub receivers: wgpu::TextureView,
}

impl SurfaceTargets {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        Self {
            depth: target(device, "surface depth", size, gbuffer::DEPTH),
            receivers: target(device, "receiver layer", size, gbuffer::RECEIVER),
        }
    }
}

/// The nearest reflective surface at each pixel, opaque or receiver, which
/// the screen-space method, world-space rays, composition and the temporal
/// consumers read (the Surface contract): the surface depth and the receiver
/// layer, with the G-buffer's motion. A pixel is under a receiver where the
/// surface depth is nearer than the opaque depth.
#[derive(Clone, Copy)]
pub(crate) struct Surface<'a> {
    pub depth: &'a wgpu::TextureView,
    /// The receiver layer; a frame that drew no receiver lends a stand-in,
    /// which nothing reads where the depths are equal.
    pub receivers: &'a wgpu::TextureView,
}

impl SharedTargets {
    /// The targets at `size`, with the surface's own while `surface`.
    pub fn new(device: &wgpu::Device, size: [u32; 2], surface: bool) -> Self {
        let gbuffer_target = |label, format| target(device, label, size, format);
        let depth = gbuffer_target("stable depth", gbuffer::DEPTH);
        Self {
            color: gbuffer_target("jittered HDR scene", gbuffer::COLOR),
            ambient: gbuffer_target("jittered ambient diffuse", gbuffer::AMBIENT),
            source_id: gbuffer_target("stable lit primitive identity", gbuffer::SOURCE_ID),
            depth,
            normal: gbuffer_target("stable world normals", gbuffer::NORMAL),
            material: gbuffer_target(
                "stable coat and base roughness, coat strength and F90",
                gbuffer::MATERIAL,
            ),
            anisotropy: gbuffer_target(
                "stable world anisotropy tangent and strength and environment scale",
                gbuffer::ANISOTROPY,
            ),
            f0: gbuffer_target(
                "stable material F0, lit and baked-light flags and material occlusion",
                gbuffer::F0,
            ),
            motion: gbuffer_target("stable rigid motion", gbuffer::MOTION),
            composite: target_with_usage(
                device,
                "HDR composition",
                size,
                gbuffer::COLOR,
                wgpu::TextureUsages::STORAGE_BINDING,
            ),
            fsr2_masks: [
                "FSR2 reactive mask",
                "FSR2 transparency and composition mask",
            ]
            .map(|label| target(device, label, size, MASK_FORMAT)),
            surface: surface.then(|| SurfaceTargets::new(device, size)),
        }
    }

    /// Allocates the surface's own targets, once a scene holds a receiver.
    pub fn hold_surface(&mut self, device: &wgpu::Device) {
        if self.surface.is_none() {
            let size = self.depth.texture().size();
            self.surface = Some(SurfaceTargets::new(device, [size.width, size.height]));
        }
    }

    /// The surface of a frame: its own targets' when the receiver pass
    /// `drew` receivers over them, else the opaque depth lent as the
    /// surface depth.
    pub fn surface(&self, drew: bool) -> Surface<'_> {
        match &self.surface {
            Some(surface) if drew => Surface {
                depth: &surface.depth,
                receivers: &surface.receivers,
            },
            _ => Surface {
                depth: &self.depth,
                receivers: &self.normal,
            },
        }
    }
}

/// One probe-capture face's attachments, which the capture's opaque pass
/// writes; its view is `View::probe_face`.
pub(crate) struct CaptureFace {
    pub color: wgpu::TextureView,
    pub motion: wgpu::TextureView,
    pub depth: wgpu::TextureView,
}

/// A 2D render target of `size` (at least one texel) that passes may also
/// sample and copy.
pub(crate) fn target(
    device: &wgpu::Device,
    label: &str,
    size: [u32; 2],
    format: wgpu::TextureFormat,
) -> wgpu::TextureView {
    target_with_usage(device, label, size, format, wgpu::TextureUsages::empty())
}

pub(crate) fn target_with_usage(
    device: &wgpu::Device,
    label: &str,
    size: [u32; 2],
    format: wgpu::TextureFormat,
    extra_usage: wgpu::TextureUsages,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size[0].max(1),
                height: size[1].max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | extra_usage,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

/// `view` as a colour attachment cleared to transparent black and stored.
pub(crate) fn attachment(view: &wgpu::TextureView) -> Option<wgpu::RenderPassColorAttachment<'_>> {
    Some(wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            store: wgpu::StoreOp::Store,
        },
    })
}
