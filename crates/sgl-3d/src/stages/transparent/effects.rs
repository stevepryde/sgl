//! Caller-authored additive glow (`Glow` vertices in the scene's transient
//! geometry) with metric soft intersections against the opaque depth.
use crate::content::transient::Glow;
use crate::scene::transient::Transient;
use crate::shading::gbuffer;
use crate::shading::vertex::{VertexLayout, vertex_layout};
use crate::view::targets::mask_targets;

/// Additive glow, under the unlit group 0.
pub(crate) static GLOW: crate::shading::Module = crate::shading::Module {
    name: "glow",
    source: include_str!("glow.wgsl"),
    deps: &[&crate::shading::BIND_UNLIT, &crate::shading::FRAME_FOG],
};
/// The glow's vertex buffer, read by `glow_vs`.
pub(crate) const GLOW_LAYOUT: VertexLayout =
    vertex_layout!(Glow, [position, uv, color, kind, other, soft_distance]);

pub(crate) struct Effects {
    soft_glow: wgpu::RenderPipeline,
    /// `soft_glow` also writing FSR2's masks (`mask_targets`).
    soft_glow_fsr2_masked: wgpu::RenderPipeline,
    depth_layout: wgpu::BindGroupLayout,
}

impl Effects {
    pub fn new(device: &wgpu::Device, unlit: &wgpu::BindGroupLayout) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("additive glow"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&GLOW]).into()),
        });
        let depth_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("soft effect depth"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let soft_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("soft additive geometry"),
            bind_group_layouts: &[Some(unlit), Some(&depth_layout)],
            immediate_size: 0,
        });
        let buffers = [GLOW_LAYOUT.buffer];
        let glow_targets = [Some(wgpu::ColorTargetState {
            format: gbuffer::COLOR,
            blend: Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::OVER,
            }),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let [reactive, composition] = mask_targets();
        let masked_targets = [glow_targets[0].clone(), reactive, composition];
        let make = |masked: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("additive transient geometry"),
                layout: Some(&soft_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("glow_vs"),
                    compilation_options: Default::default(),
                    buffers: &buffers,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(if masked {
                        "glow_soft_fsr2_masked_fs"
                    } else {
                        "glow_soft_fs"
                    }),
                    compilation_options: Default::default(),
                    targets: if masked {
                        &masked_targets
                    } else {
                        &glow_targets
                    },
                }),
                primitive: wgpu::PrimitiveState {
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: gbuffer::DEPTH,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            soft_glow: make(false),
            soft_glow_fsr2_masked: make(true),
            depth_layout,
        }
    }

    /// The opaque depth soft intersections read, as group 1. Recreate it
    /// when the depth target is.
    pub fn depth_group(&self, device: &wgpu::Device, depth: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("soft effect primary depth"),
            layout: &self.depth_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(depth),
            }],
        })
    }

    /// Draws the glow with soft intersections, into a pass whose colour
    /// attachment is followed by FSR2's masks when `fsr2_masked`. The
    /// sampled depth attachment must be read-only (`depth_ops: None`).
    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        unlit: &wgpu::BindGroup,
        depth: &wgpu::BindGroup,
        transient: &Transient,
        fsr2_masked: bool,
    ) {
        // Nothing to draw; WebGPU warns of an empty draw.
        if transient.glow_count == 0 {
            return;
        }
        pass.set_pipeline(if fsr2_masked {
            &self.soft_glow_fsr2_masked
        } else {
            &self.soft_glow
        });
        pass.set_bind_group(0, unlit, &[]);
        pass.set_bind_group(1, depth, &[]);
        pass.set_vertex_buffer(0, transient.glow.slice(..));
        pass.draw(0..transient.glow_count, 0..1);
    }
}
