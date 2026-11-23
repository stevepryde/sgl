//! The transparent stage's instance footprints (the register's M-23): in
//! the temporal composition pass, the view's footprint draws
//! (`view::footprints`), marking transparency and composition 1 where
//! changed instances stood, at or in front of the opaque depth, and where
//! they stand, whatever lies in front of them. Two pipelines of one
//! program, in a `Compiled` slot (AR-13), made in the first frame of a
//! view whose antialiasing reads the masks, planned or not, so the first
//! instance edit creates nothing.
use crate::shading::gbuffer::DEPTH;
use crate::shading::vertex::{DRAW_INSTANCE_LAYOUT, DRAW_INSTANCE_SLOT};
use crate::view::FrameViews;
use crate::view::compile::Compiled;
use crate::view::footprints::FootprintDraw;
use crate::view::frame::FrameContext;
use crate::view::targets::composition_targets;

/// The footprint program, under the unlit group 0 and the scene's group 1.
pub(crate) static FOOTPRINTS: crate::shading::Module = crate::shading::Module {
    name: "footprints",
    source: include_str!("footprints.wgsl"),
    deps: &[&crate::shading::BIND_UNLIT, &crate::shading::VERTEX_PULL],
};

/// The footprint pipelines.
pub(crate) struct Programs {
    /// Where instances stood: at or in front of the opaque depth, where
    /// what a frame drew of them can still show.
    stood: wgpu::RenderPipeline,
    /// Where instances stand: everywhere they project, since the camera's
    /// cull may draw a coarser level whose surface lies on either side of
    /// the base level's.
    stands: wgpu::RenderPipeline,
}

impl Programs {
    fn new(
        device: &wgpu::Device,
        unlit: &wgpu::BindGroupLayout,
        scene: &wgpu::BindGroupLayout,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("instance footprints"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&FOOTPRINTS]).into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("instance footprints"),
            bind_group_layouts: &[Some(unlit), Some(scene)],
            immediate_size: 0,
        });
        let targets = composition_targets();
        let pipeline = |label, depth_compare| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("footprint_vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(DRAW_INSTANCE_LAYOUT.buffer.clone())],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("footprint_fs"),
                    compilation_options: Default::default(),
                    targets: &targets,
                }),
                // Both faces: what a frame drew of a single-sided mesh lies
                // within them.
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(depth_compare),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            stood: pipeline("where instances stood", wgpu::CompareFunction::GreaterEqual),
            stands: pipeline("where instances stand", wgpu::CompareFunction::Always),
        }
    }
}

#[derive(Default)]
pub(crate) struct Footprints {
    programs: Compiled<Programs>,
}

impl Footprints {
    /// The programs, where the frame has them: none in a planning pass
    /// whose job is still queued, which leaves the draws out.
    pub fn programs(&mut self, ctx: &FrameContext<'_>) -> Option<&Programs> {
        let device = ctx.device.clone();
        let unlit = ctx.bindings.unlit.clone();
        let scene = ctx.bindings.scene.clone();
        self.programs
            .get(ctx.pipelines.compiler(), "instance footprints", move || {
                move || Programs::new(&device, &unlit, &scene)
            })
    }

    /// Draws `views`' footprints of `scene` in `pass`, the temporal
    /// composition pass, with `programs` under `camera_unlit`.
    pub fn draw(
        programs: &Programs,
        (scene, views, camera_unlit): (&crate::Scene, &FrameViews, &wgpu::BindGroup),
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        let list = &views.footprints;
        pass.set_bind_group(0, camera_unlit, &[]);
        pass.set_vertex_buffer(DRAW_INSTANCE_SLOT, views.instances.buffer().slice(..));
        let draw = |pass: &mut wgpu::RenderPass<'_>, draws: &[FootprintDraw]| {
            for draw in draws {
                pass.draw(0..draw.vertices, draw.instance..draw.instance + 1);
            }
        };
        if let Some(group) = scene.footprints.group().filter(|_| !list.stood.is_empty()) {
            pass.set_pipeline(&programs.stood);
            pass.set_bind_group(1, group, &[]);
            draw(pass, &list.stood);
        }
        if !list.stands.is_empty() {
            pass.set_pipeline(&programs.stands);
            pass.set_bind_group(1, &scene.scene_group, &[]);
            draw(pass, &list.stands);
        }
    }
}
