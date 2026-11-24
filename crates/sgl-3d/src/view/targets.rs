//! The frame's sizes and the targets stages share, each defined once, which
//! the renderer creates and lends. A stage's own targets (bloom, AO,
//! reflection history) are its own.
use crate::shading::gbuffer;

/// The frame's sizes. Every pass up to antialiasing renders at `render`;
/// antialiasing writes `scene` (they differ only while FSR upscales), which
/// depth of field, motion blur, bloom, SMAA and tone mapping use;
/// presentation writes `output`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sizes {
    pub render: [u32; 2],
    pub scene: [u32; 2],
    pub output: [u32; 2],
}

/// The format of the temporal masks, the reactive and
/// transparency-and-composition masks, as AMD's FSR sample allocates them
/// (`samples/fsrapi/config/fsrapiconfig.json`).
pub(crate) const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

/// The reactive and transparency-and-composition mask targets a transparent
/// draw adds after its colour, as AMD's FSR sample adds them to its
/// translucency pass (`samples/fsrapi/fsrapirendermodule.cpp:93–101`, MIT,
/// see `LICENSE-amd-fidelityfx.txt`): each draw's transparency and
/// composition `m` accumulates in red as `m (1 - mask) + mask`; its
/// reactivity, which it outputs through `temporal_reactive`
/// (`shading/temporal_masks.wgsl`) as `r` in red and `r / 0.9` in alpha,
/// as `r + mask (1 - r / 0.9)`, the same union in units of AMD's
/// recommended cap, which it never exceeds.
pub(crate) fn mask_targets() -> [Option<wgpu::ColorTargetState>; 2] {
    let alpha = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::Zero,
        operation: wgpu::BlendOperation::Add,
    };
    let reactive = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha,
    };
    let composition = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::OneMinusDst,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
        alpha,
    };
    [reactive, composition].map(|blend| {
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
    /// RGBA16F: the ambient light within `color` before occlusion, its
    /// diffuse share and multiple scattering together, `color`'s alpha
    /// holding the multiple scattering's share, which source completion
    /// occludes (shading/gbuffer.wgsl).
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
    /// R8: the temporal masks, AMD's reactive mask and its transparency and
    /// composition mask, made in the first frame that binds them and kept
    /// while the view's antialiasing reads masks (`set_temporal_masks`).
    temporal_masks: Option<[wgpu::TextureView; 2]>,
    /// Whether this frame binds `temporal_masks`.
    masks_bound: bool,
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
            ambient: gbuffer_target("jittered ambient light", gbuffer::AMBIENT),
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
            temporal_masks: None,
            masks_bound: false,
            surface: surface.then(|| SurfaceTargets::new(device, size)),
        }
    }

    /// Binds the temporal masks in this frame where `bind`
    /// (`Effective::temporal_masks`), making them if absent. Unbound, they
    /// are kept while `keep` (TAA or FSR runs), so content that marks them
    /// now and then makes no targets each time, and released otherwise: two
    /// bytes a render pixel (the register's F-15).
    pub fn set_temporal_masks(&mut self, device: &wgpu::Device, bind: bool, keep: bool) {
        if bind {
            self.temporal_masks.get_or_insert_with(|| {
                let size = self.depth.texture().size();
                ["reactive mask", "transparency and composition mask"]
                    .map(|label| target(device, label, [size.width, size.height], MASK_FORMAT))
            });
        } else if !keep {
            self.temporal_masks = None;
        }
        self.masks_bound = bind;
    }

    /// The temporal masks this frame binds, which its draws onto the
    /// composite write and its antialiasing reads.
    pub fn temporal_masks(&self) -> Option<&[wgpu::TextureView; 2]> {
        self.temporal_masks.as_ref().filter(|_| self.masks_bound)
    }

    /// The temporal masks held, bound in this frame or not.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn held_temporal_masks(&self) -> Option<&[wgpu::TextureView; 2]> {
        self.temporal_masks.as_ref()
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
/// writes; its view is `ViewData::probe_face`.
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

/// What `held` holds, made by `make` where it holds nothing or a texture
/// (`texture` of it) of another size than `size`: a stage's snapshot of the
/// frame at the render size, allocated in the first frame that needs it and
/// again after a resize (heat distortion's, the transmission copy).
pub(crate) fn hold<T>(
    held: &mut Option<T>,
    texture: impl Fn(&T) -> &wgpu::Texture,
    size: wgpu::Extent3d,
    make: impl FnOnce() -> T,
) -> &T {
    if held
        .as_ref()
        .is_none_or(|kept| texture(kept).size() != size)
    {
        *held = Some(make());
    }
    held.as_ref().unwrap()
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    // Plausible defects: the masks made before a frame binds them, made
    // again for each frame that binds them after one that did not (two
    // targets created and dropped per spawn), still bound in a frame that
    // binds none, or held once neither TAA nor FSR reads them. The oracle
    // is the register's F-15: made at the first bind, R8 at the render
    // size, kept unbound while kept, released otherwise.
    #[test]
    fn temporal_masks_are_kept_once_made_and_bound_per_frame() {
        let Some((device, _queue)) = crate::test_support::device() else {
            return;
        };
        for size in [[8, 6], [13, 9]] {
            let mut targets = SharedTargets::new(&device, size, false);
            targets.set_temporal_masks(&device, false, true);
            assert!(
                targets.held_temporal_masks().is_none(),
                "nothing bound them yet"
            );
            targets.set_temporal_masks(&device, true, true);
            let made = targets.temporal_masks().unwrap().clone();
            for mask in &made {
                assert_eq!(mask.texture().format(), wgpu::TextureFormat::R8Unorm);
                assert_eq!([mask.texture().width(), mask.texture().height()], size);
            }
            targets.set_temporal_masks(&device, false, true);
            assert!(targets.temporal_masks().is_none(), "unbound");
            assert!(targets.held_temporal_masks().is_some(), "kept");
            targets.set_temporal_masks(&device, true, true);
            assert_eq!(
                &made,
                targets.temporal_masks().unwrap(),
                "bound again, the kept masks"
            );
            targets.set_temporal_masks(&device, false, false);
            assert!(targets.held_temporal_masks().is_none(), "released");
            targets.set_temporal_masks(&device, true, true);
            assert_ne!(
                &made,
                targets.temporal_masks().unwrap(),
                "made again after a release"
            );
        }
    }
}
