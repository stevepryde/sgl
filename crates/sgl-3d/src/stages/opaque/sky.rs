//! The authored panoramic sky behind opaque geometry, under the unlit group 0.
use crate::shading::{self, gbuffer};

pub(crate) static SKY: shading::Module = shading::Module {
    name: "sky",
    source: include_str!("sky.wgsl"),
    deps: &[
        &shading::BIND_UNLIT,
        &shading::ENVIRONMENT,
        &shading::FULLSCREEN,
        &shading::GBUFFER,
    ],
};
/// The entry points the sky's pipeline is created with.
pub(crate) const SKY_VS_ENTRY: &str = "sky_vs";
pub(crate) const SKY_FS_ENTRY: &str = "sky_fs";

pub(crate) struct Sky {
    pipeline: wgpu::RenderPipeline,
}

impl Sky {
    pub fn new(device: &wgpu::Device, unlit: &wgpu::BindGroupLayout) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sky and additive glow"),
            source: wgpu::ShaderSource::Wgsl(shading::compose(&[&SKY]).into()),
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
                label: Some("authored panoramic sky"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(SKY_VS_ENTRY),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(SKY_FS_ENTRY),
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

    /// Draws the panoramic background into colour and motion targets, under
    /// `unlit` group 0.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, unlit: &wgpu::BindGroup) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, unlit, &[]);
        pass.draw(0..3, 0..1);
    }
}
