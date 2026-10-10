//! The local-light shadow atlas: every shadowed point and spot light's faces
//! in one 2D depth atlas, with each face's static casters cached.
//!
//! Lights that cast a shadow and reach the camera's view are placed in the
//! atlas by screen coverage (`atlas`, Godot's shadow atlas); lights it has
//! no room for are lit without a shadow. A point light has six cube faces,
//! laid out as Wicked Engine lays them; a spot light one face, or the cube
//! faces its cone reaches when it is wider than a face (`shape`).
//!
//! Each face keeps a static layer in a second atlas of the same layout: its
//! static instances, drawn once (`cache`). A frame draws only the faces
//! whose content changed (`plan`). When only a face's moving casters did,
//! it copies the face's static layer and draws the moving casters over it;
//! when the layer is stale, the layer is drawn first; when the face's light
//! moved, the face draws every caster at once. A frame in which nothing a
//! face shows changed draws nothing.
//!
//! Each light's shadow record (`LocalShadowRecord`) is written only when it
//! changed: a light placed, re-placed, moved or left without a shadow. As
//! Bevy extracts a light to its render world only when what it shows changed
//! (`extract_lights`'s `Changed` filters, crates/bevy_pbr/src/render/light.rs
//! lines 333-436 at revision 9d120361303727a66b62f31f0d053793af62417a), the
//! stage compares each record with the one its buffer holds. A queued write
//! lands at the next submission whether or not the frame that queued it was
//! submitted or finished, so what the buffer holds follows the writes
//! queued, not the finished frames as the atlas's cache does.
//!
//! Reads: the scene's lights, instances, materials and static edits, the
//! camera. Writes: its faces' draw instances into the frame's
//! (`FrameViews::instances`), the frame atlas and the static atlas, and the
//! lights' shadow records, which group 0 binds.
//! Honours: the effective local lights.
//! Timing groups: `local shadow layers`, `local shadows`.
pub(crate) mod atlas;
mod cache;
mod plan;
mod shape;

use crate::Scene;
use crate::shading::lights::LocalShadowRecord;
use crate::timing::GpuTiming;
use crate::view::bindings::FrameBindings;
use crate::view::draw_list::DrawInstances;
use crate::view::frame::FrameContext;
use crate::view::pipelines::{GeometryPass, GeometryPipelines};
use glam::{Mat4, Vec3};
pub(crate) use plan::ShadowFrame;
use plan::{Face, Work};

/// The last rendered frame's local-light shadows
/// (`Renderer::local_shadow_stats`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalShadowStats {
    /// Lights that cast a shadow, reach the camera's view and have one.
    pub shadowed: usize,
    /// Lights that cast a shadow and reach the camera's view, but for which
    /// the atlas had no room: they are lit without a shadow.
    pub unshadowed: usize,
    /// Faces whose static layer was drawn.
    pub layers_drawn: usize,
    /// Faces drawn into the frame's atlas.
    pub faces_drawn: usize,
    /// Draws encoded into either atlas, clears and copies included.
    pub draws: usize,
}

/// A face view's uniform and the shadow group 0 that binds it.
struct FaceView {
    buffer: wgpu::Buffer,
    group: wgpu::BindGroup,
}

pub(crate) struct Local {
    /// The frame's atlas.
    frame: Atlas,
    /// The static layers.
    layers: Atlas,
    /// `LocalShadowRecord`s at each light's index, which group 0 binds.
    pub records: wgpu::Buffer,
    /// What `records` holds once the writes queued so far land.
    written: Vec<LocalShadowRecord>,
    clear: wgpu::RenderPipeline,
    copy: wgpu::RenderPipeline,
    copy_group: wgpu::BindGroup,
    plan: plan::Plan,
    /// The view of each face drawn, by its place in the plan's faces.
    views: Vec<FaceView>,
}

/// One of the stage's two atlases of one layout: the view its passes draw
/// into and the one-layer array group 0 samples, as the shared shadow
/// filters take.
struct Atlas {
    target: wgpu::TextureView,
    sampled: wgpu::TextureView,
}

impl Atlas {
    /// An atlas of `size` texels a side.
    fn new(device: &wgpu::Device, label: &str, size: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::shading::gbuffer::DEPTH,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        Self {
            target: texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                ..Default::default()
            }),
            sampled: texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            }),
        }
    }
}

/// The local shadow stage's own shaders: clearing a face and copying its
/// static layer.
pub(crate) static COPY: crate::shading::Module = crate::shading::Module {
    name: "local_shadow_copy",
    source: include_str!("local/copy.wgsl"),
    deps: &[&crate::shading::FULLSCREEN_VS],
};
/// The entry point the copy's pipeline is created with, beside
/// `shading::FULLSCREEN_VS_ENTRY`, which the clear's alone has.
pub(crate) const COPY_FS_ENTRY: &str = "copy_fs";

fn records_buffer(device: &wgpu::Device, count: usize) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some("local light shadow records"),
            size: (count.max(1) * std::mem::size_of::<LocalShadowRecord>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
    )
}

/// A depth-only pipeline over `layout` that writes every texel of the
/// viewport, with `fragment` or the full-screen triangle's depth.
fn face_pipeline(
    device: &wgpu::Device,
    label: &str,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    fragment: Option<&str>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(crate::shading::FULLSCREEN_VS_ENTRY),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: fragment.map(|entry_point| wgpu::FragmentState {
            module,
            entry_point: Some(entry_point),
            compilation_options: Default::default(),
            targets: &[],
        }),
        primitive: Default::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: crate::shading::gbuffer::DEPTH,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// The copy's group: the static layers it copies faces from.
fn copy_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    layers: &Atlas,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("local light shadow static layers"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&layers.target),
        }],
    })
}

impl Local {
    /// Atlases of `size` texels a side.
    pub fn new(device: &wgpu::Device, size: u32) -> Self {
        let layers = Atlas::new(device, "local light shadow static layers", size);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("local light shadow copy"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&COPY]).into()),
        });
        let copy_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("local light shadow static layers"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline_layout = |label, groups: &[Option<&wgpu::BindGroupLayout>]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: groups,
                immediate_size: 0,
            })
        };
        let clear = face_pipeline(
            device,
            "local light shadow face clear",
            &module,
            &pipeline_layout("local light shadow face clear", &[]),
            None,
        );
        let copy = face_pipeline(
            device,
            "local light shadow layer copy",
            &module,
            &pipeline_layout("local light shadow layer copy", &[Some(&copy_layout)]),
            Some(COPY_FS_ENTRY),
        );
        Self {
            frame: Atlas::new(device, "local light shadow atlas", size),
            copy_group: copy_group(device, &copy_layout, &layers),
            layers,
            records: records_buffer(device, 1),
            written: vec![LocalShadowRecord::NONE],
            clear,
            copy,
            plan: plan::Plan::new(size),
            views: Vec::new(),
        }
    }

    /// Reallocates both atlases at `size` texels a side unless they are
    /// that size, forgetting every placement and static layer, so the next
    /// frame places and draws every shadow anew.
    pub fn resize(&mut self, device: &wgpu::Device, size: u32) {
        if self.frame.target.texture().width() == size {
            return;
        }
        self.frame = Atlas::new(device, "local light shadow atlas", size);
        self.layers = Atlas::new(device, "local light shadow static layers", size);
        self.copy_group = copy_group(device, &self.copy.get_bind_group_layout(0), &self.layers);
        self.plan = plan::Plan::new(size);
    }

    /// The frame's atlas, as group 0 samples it.
    pub fn atlas(&self) -> &wgpu::TextureView {
        &self.frame.sampled
    }

    /// The static layers, as group 0 samples them.
    pub fn layers(&self) -> &wgpu::TextureView {
        &self.layers.sampled
    }

    /// The last prepared frame's statistics.
    pub fn stats(&self) -> LocalShadowStats {
        self.plan.stats
    }

    /// The last frame's shadowed lights in the atlas's ranking, largest
    /// screen coverage first: the lights the ray-traced shadow stage gives
    /// its slots to.
    pub fn ranking(&self) -> &[crate::content::identity::LightId] {
        self.plan.ranking()
    }

    /// Places the shadows of `scene`'s casting lights that reach the view of
    /// a camera with `view` and `projection`, when `enabled`, plans the faces
    /// whose content changed for `frame`, with their draws' instances in
    /// `drawn`, and uploads their views and the lights' shadow records.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bindings: &FrameBindings,
        (scene, drawn): (&Scene, &mut DrawInstances),
        camera: (Mat4, Mat4),
        frame: ShadowFrame,
        enabled: bool,
    ) {
        self.plan.prepare(drawn, scene, camera, frame, enabled);
        self.upload_views(device, queue, bindings);
        let records = self.plan.records();
        if self.written.len() < records.len() {
            // A new buffer holds zeros: every light without a shadow.
            self.records = records_buffer(device, records.len());
            self.written = vec![LocalShadowRecord::NONE; records.len()];
        }
        write_changed(queue, &self.records, &mut self.written, records);
    }

    /// Places the static layers a probe capture at `center` of `scene`
    /// samples for `frame`, when `enabled`, with their draws'
    /// instances in `drawn`, and returns the shadow records that place them,
    /// which the capture's lit groups bind. `encode_capture` draws them, and
    /// `finish_capture` commits them once the capture is submitted.
    #[allow(clippy::too_many_arguments)]
    pub fn plan_capture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bindings: &FrameBindings,
        (scene, drawn): (&Scene, &mut DrawInstances),
        center: Vec3,
        frame: ShadowFrame,
        enabled: bool,
    ) -> wgpu::Buffer {
        self.plan
            .prepare_capture(drawn, scene, center, frame, enabled);
        self.upload_views(device, queue, bindings);
        crate::scene::buffer(
            device,
            "probe capture local light shadow records",
            bytemuck::cast_slice(self.plan.records()),
            wgpu::BufferUsages::STORAGE,
        )
    }

    /// Draws the planned capture's static layers into `encoder`, with the
    /// uploaded draw instances it was planned with.
    pub fn encode_capture(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        (scene, pipelines): (&Scene, &GeometryPipelines),
        drawn: &DrawInstances,
    ) {
        self.encode_layers(encoder, None, (scene, pipelines, drawn));
    }

    /// Commits the last capture, once submitted.
    pub fn finish_capture(&mut self) {
        self.plan.finish();
    }

    /// Uploads the planned faces' views.
    fn upload_views(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bindings: &FrameBindings,
    ) {
        for (index, face) in self.plan.faces().iter().enumerate() {
            let uniform = bytemuck::bytes_of(&face.view.uniform);
            if let Some(view) = self.views.get(index) {
                crate::counters::write_buffer(queue, &view.buffer, 0, uniform);
            } else {
                let buffer = crate::scene::buffer(
                    device,
                    "local light shadow face",
                    uniform,
                    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                );
                let group = bindings.shadow_group(device, &buffer, &bindings.frame);
                self.views.push(FaceView { buffer, group });
            }
        }
    }

    /// Draws the faces whose content changed: static layers first, then the
    /// frame's faces.
    pub fn encode(&mut self, ctx: &mut FrameContext<'_>) {
        let (scene, pipelines, drawn) = (ctx.scene, ctx.pipelines, &ctx.views.instances);
        let mut draws = self.encode_layers(ctx.encoder, ctx.timing, (scene, pipelines, drawn));
        let faces = self.plan.faces();
        if !faces.is_empty() {
            let mut pass = begin(ctx.encoder, ctx.timing, &self.frame.target, "local shadows");
            for (index, face) in faces.iter().enumerate() {
                limit(&mut pass, face);
                let casters = if face.work == Work::Full {
                    pass.set_pipeline(&self.clear);
                    &face.casters
                } else {
                    pass.set_pipeline(&self.copy);
                    pass.set_bind_group(0, &self.copy_group, &[]);
                    &face.moving
                };
                pass.draw(0..3, 0..1);
                pass.set_bind_group(0, &self.views[index].group, &[]);
                draws += 1 + casters.draw(
                    scene,
                    pipelines,
                    drawn,
                    &mut pass,
                    GeometryPass::LocalShadow,
                );
            }
        }
        self.plan.stats.draws = draws;
    }

    /// Draws the planned faces' static layers, and returns how many draws
    /// it encoded.
    fn encode_layers(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        timing: Option<&GpuTiming>,
        (scene, pipelines, drawn): (&Scene, &GeometryPipelines, &DrawInstances),
    ) -> usize {
        let mut layers = self
            .plan
            .faces()
            .iter()
            .enumerate()
            .filter(|(_, face)| face.work == Work::Layer)
            .peekable();
        if layers.peek().is_none() {
            return 0;
        }
        let mut draws = 0;
        let mut pass = begin(encoder, timing, &self.layers.target, "local shadow layers");
        for (index, face) in layers {
            limit(&mut pass, face);
            pass.set_pipeline(&self.clear);
            pass.draw(0..3, 0..1);
            pass.set_bind_group(0, &self.views[index].group, &[]);
            draws += 1 + face.casters.draw(
                scene,
                pipelines,
                drawn,
                &mut pass,
                GeometryPass::LocalShadow,
            );
        }
        draws
    }

    /// Commits the last prepared frame, once submitted.
    pub fn finish_frame(&mut self) {
        self.plan.finish();
    }
}

/// Writes the `records` that differ from what `buffer` holds, as `written`
/// records it, a run of adjacent ones at a time, and records them.
fn write_changed(
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    written: &mut [LocalShadowRecord],
    records: &[LocalShadowRecord],
) {
    let changed = |written: &[LocalShadowRecord], index: usize| {
        bytemuck::bytes_of(&records[index]) != bytemuck::bytes_of(&written[index])
    };
    let mut index = 0;
    while index < records.len() {
        if !changed(written, index) {
            index += 1;
            continue;
        }
        let start = index;
        while index < records.len() && changed(written, index) {
            index += 1;
        }
        written[start..index].copy_from_slice(&records[start..index]);
        crate::counters::write_buffer(
            queue,
            buffer,
            (start * std::mem::size_of::<LocalShadowRecord>()) as u64,
            bytemuck::cast_slice(&records[start..index]),
        );
    }
}

/// A render pass over `target` that keeps what it holds.
fn begin<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    timing: Option<&GpuTiming>,
    target: &wgpu::TextureView,
    label: &'static str,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: target,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: timing.and_then(|timing| timing.render_pass(label)),
        ..Default::default()
    })
}

/// Limits `pass` to `face`'s slot.
fn limit(pass: &mut wgpu::RenderPass<'_>, face: &Face) {
    let [x, y] = face.origin;
    let size = face.size as f32;
    pass.set_viewport(x as f32, y as f32, size, size, 0., 1.);
    pass.set_scissor_rect(x, y, face.size, face.size);
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
