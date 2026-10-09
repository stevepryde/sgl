//! The volume layers (D-41; specs/sgl3d-architecture.md, Programmable
//! surfaces): three depth targets of the render size in the opaque depth's
//! format, each a copy of the opaque depth over which the transparent stage
//! draws the camera's blended batches whose material's shader reads its
//! volume path (`scene_volume_path`), tested strictly nearer and written:
//! the nearest front faces (the entry layer), the nearest back faces (the
//! exit layer), and the nearest back faces behind the exit layer's (the
//! second exit layer, one depth peel, as Everitt's order-independent
//! transparency peels, its pass reading the exit layer). The camera's
//! blended draws read them through `scene_volume_path`
//! (shader_scene_depth.wgsl). The exit layer is the back-face depth of
//! Wyman's image-space refraction (SIGGRAPH 2005), measured along the view
//! ray. They are allocated in the first frame that draws them and again
//! after a resize, as the transmission copy is, freed while the effective
//! configuration has no volume paths, and keep no history.
use crate::shading::bind::{self, BlendedTrace};
use crate::shading::gbuffer;
use crate::view::cached_group::CachedGroup;
use crate::view::frame::FrameContext;
use crate::view::pipelines::GeometryPass;
use crate::view::targets::hold;

pub(crate) struct Volumes {
    layers: Option<Layers>,
    /// The second exit pass's group 3, of the blended pipelines' layout: the
    /// exit layer, and stand-ins for what it does not read.
    group: CachedGroup,
    /// Its `BlendedTrace`, which nothing it draws reads: zero.
    trace: wgpu::Buffer,
    /// Whether the layers hold this frame's.
    held: bool,
}

/// The entry, exit and second exit layers.
pub(crate) struct Layers {
    pub entry: wgpu::TextureView,
    pub exit: wgpu::TextureView,
    pub second_exit: wgpu::TextureView,
}

impl Layers {
    fn new(device: &wgpu::Device, size: wgpu::Extent3d) -> Self {
        let layer = |label| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: gbuffer::DEPTH,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        Self {
            entry: layer("volume entry layer"),
            exit: layer("volume exit layer"),
            second_exit: layer("volume second exit layer"),
        }
    }

    fn texture(&self) -> &wgpu::Texture {
        self.entry.texture()
    }
}

impl Volumes {
    /// The layers' state over the blended pipelines' group 3 `layout`.
    pub fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> Self {
        Self {
            layers: None,
            group: CachedGroup::new(layout.clone()),
            trace: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("second exit volume layer trace"),
                    size: size_of::<BlendedTrace>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM,
                    mapped_at_creation: false,
                },
            ),
            held: false,
        }
    }

    /// Frees the layers, as while the effective configuration has no
    /// volume paths. Returns whether it held any, whose views the groups
    /// that bound them still keep.
    pub fn release(&mut self) -> bool {
        self.held = false;
        self.group.forget();
        self.layers.take().is_some()
    }

    /// The layers, where they hold this frame's.
    pub fn held(&self) -> Option<&Layers> {
        self.layers.as_ref().filter(|_| self.held)
    }

    /// When the effective configuration has volume paths and the camera's
    /// blended list holds a material whose shader reads its volume path:
    /// for each layer, copies the opaque depth into it and draws the list's
    /// batches of those materials over it with the frame's jittered view,
    /// the second exit's with its group 3, whose stand-in for the
    /// screen-space result and transmission copy is `blank`. Returns
    /// whether the layers hold this frame's.
    pub fn encode(&mut self, ctx: &mut FrameContext<'_>, blank: &wgpu::TextureView) -> bool {
        self.held = false;
        if !ctx.effective.volume_paths || !ctx.views.blended.holds_volumes(ctx.scene) {
            return false;
        }
        let depth = &ctx.targets.depth;
        let size = depth.texture().size();
        let device = ctx.device;
        let layers = hold(&mut self.layers, Layers::texture, size, || {
            Layers::new(device, size)
        });
        let texture = wgpu::BindingResource::TextureView;
        let group = self.group.get(
            device,
            "second exit volume layer",
            &[
                (bind::blended::REFLECTIONS, texture(blank)),
                (bind::blended::SURFACE_DEPTH, texture(depth)),
                (bind::blended::TRACE, self.trace.as_entire_binding()),
                (bind::blended::TRANSMISSION, texture(blank)),
                (bind::blended::SCENE_DEPTH, texture(depth)),
                (bind::blended::VOLUME_ENTRY, texture(depth)),
                (bind::blended::VOLUME_EXIT, texture(&layers.exit)),
                (bind::blended::VOLUME_SECOND_EXIT, texture(depth)),
            ],
        );
        for (layer, kind) in [
            (&layers.entry, GeometryPass::VolumeEntry),
            (&layers.exit, GeometryPass::VolumeExit),
            (&layers.second_exit, GeometryPass::VolumeSecondExit),
        ] {
            ctx.encoder.copy_texture_to_texture(
                depth.texture().as_image_copy(),
                layer.texture().as_image_copy(),
                size,
            );
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("volume layer"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: layer,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: ctx.timing.and_then(|t| t.render_pass("volume layers")),
                ..Default::default()
            });
            pass.set_bind_group(0, ctx.bindings.camera_lit(), &[]);
            if kind == GeometryPass::VolumeSecondExit {
                pass.set_bind_group(3, group, &[]);
            }
            ctx.views.blended.draw(
                ctx.scene,
                ctx.pipelines,
                &ctx.views.instances,
                &mut pass,
                kind,
            );
        }
        self.held = true;
        true
    }
}
