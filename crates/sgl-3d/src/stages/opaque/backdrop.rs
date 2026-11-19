//! The backdrop behind opaque geometry, under the unlit group 0: the frame
//! environment's panorama, one colour, or the procedural sky (the sky
//! stage's sky-view table, bound where a panorama is, and the lights'
//! discs). Its timing group is `sky`.
use crate::shading::{self, gbuffer};

pub(crate) static BACKDROP: shading::Module = shading::Module {
    name: "backdrop",
    source: include_str!("backdrop.wgsl"),
    deps: &[
        &shading::BIND_UNLIT,
        &shading::ENVIRONMENT,
        &shading::FULLSCREEN,
        &shading::GBUFFER,
        &shading::SCENE_COLOR,
        &shading::sky::SKY,
    ],
};
/// The entry points the backdrop's pipeline is created with.
pub(crate) const BACKDROP_VS_ENTRY: &str = "backdrop_vs";
pub(crate) const BACKDROP_FS_ENTRY: &str = "backdrop_fs";

pub(crate) struct BackdropDraw {
    pipeline: wgpu::RenderPipeline,
}

impl BackdropDraw {
    pub fn new(device: &wgpu::Device, unlit: &wgpu::BindGroupLayout) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("backdrop"),
            source: wgpu::ShaderSource::Wgsl(shading::compose(&[&BACKDROP]).into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(unlit)],
            immediate_size: 0,
        });
        let targets = [gbuffer::COLOR, gbuffer::MOTION].map(|format| {
            Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })
        });
        Self {
            pipeline: device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("backdrop"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(BACKDROP_VS_ENTRY),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(BACKDROP_FS_ENTRY),
                    compilation_options: Default::default(),
                    targets: &targets,
                }),
                primitive: wgpu::PrimitiveState {
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: gbuffer::DEPTH,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            }),
        }
    }

    /// Draws the backdrop into colour and motion targets, under `unlit`
    /// group 0.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, unlit: &wgpu::BindGroup) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, unlit, &[]);
        pass.draw(0..3, 0..1);
    }
}
