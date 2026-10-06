//! Caller-positioned transparent mist (the mist positions are the scene's
//! transient geometry), fogged where it stands.
use crate::scene::transient::Transient;
use crate::shading::gbuffer::COLOR as HDR;
use crate::shading::vertex::{Attribute, VertexLayout};
use crate::view::targets::mask_targets;

static MIST_NOISE: crate::shading::Module = crate::shading::Module {
    name: "mist_noise",
    source: include_str!("mist_noise.wgsl"),
    deps: &[],
};
/// The mist, under the unlit group 0.
pub(crate) static MIST: crate::shading::Module = crate::shading::Module {
    name: "mist",
    source: include_str!("mist.wgsl"),
    deps: &[
        &crate::shading::BIND_UNLIT,
        &crate::shading::FRAME_FOG,
        &MIST_NOISE,
    ],
};
/// The entry points the mist's pipelines are created with.
pub(crate) const MIST_VS_ENTRY: &str = "mist_vs";
pub(crate) const MIST_FS_ENTRY: &str = "mist_fs";
pub(crate) const MIST_FSR2_MASKED_FS_ENTRY: &str = "mist_fsr2_masked_fs";
/// The mist's instance buffer, one position (`Transient::mist_positions`)
/// per quad, read by `mist_vs`.
pub(crate) const MIST_LAYOUT: VertexLayout = VertexLayout {
    buffer: wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<[f32; 3]>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &[wgpu::VertexAttribute {
            format: <[f32; 3] as Attribute>::FORMAT,
            offset: 0,
            shader_location: 0,
        }],
    },
    #[cfg(test)]
    fields: &["center"],
};

pub(crate) struct Mist {
    mist: wgpu::RenderPipeline,
    /// `mist` also writing FSR2's masks (`mask_targets`).
    mist_fsr2_masked: wgpu::RenderPipeline,
}
impl Mist {
    pub fn new(device: &wgpu::Device, frame: &wgpu::BindGroupLayout) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mist"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&MIST]).into()),
        });
        let [reactive, composition] = mask_targets();
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mist"),
            bind_group_layouts: &[Some(frame)],
            immediate_size: 0,
        });
        let pipeline = |masked: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("ground mist"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(MIST_VS_ENTRY),
                    compilation_options: Default::default(),
                    buffers: &[Some(MIST_LAYOUT.buffer)],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(if masked {
                        MIST_FSR2_MASKED_FS_ENTRY
                    } else {
                        MIST_FS_ENTRY
                    }),
                    compilation_options: Default::default(),
                    targets: &[
                        Some(wgpu::ColorTargetState {
                            format: HDR,
                            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                            write_mask: wgpu::ColorWrites::ALL,
                        }),
                        reactive.clone(),
                        composition.clone(),
                    ][..if masked { 3 } else { 1 }],
                }),
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: crate::shading::gbuffer::DEPTH,
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
            mist: pipeline(false),
            mist_fsr2_masked: pipeline(true),
        }
    }
    /// The mist, sorted back to front, blended into `destination` and tested
    /// against `depth` without writing it, in timing group `group`.
    /// `fsr2_masks`, already cleared this frame, also receive it.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &wgpu::BindGroup,
        transient: &Transient,
        destination: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        fsr2_masks: Option<&[wgpu::TextureView; 2]>,
        timing: Option<&crate::timing::GpuTiming>,
        group: &'static str,
    ) {
        if !transient.mist_positions.is_empty() {
            let load = |view| {
                Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })
            };
            let attachments = match fsr2_masks {
                Some([reactive, composition]) => {
                    vec![load(destination), load(reactive), load(composition)]
                }
                None => vec![load(destination)],
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ground mist"),
                color_attachments: &attachments,
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: timing.and_then(|t| t.render_pass(group)),
                ..Default::default()
            });
            pass.set_pipeline(if fsr2_masks.is_some() {
                &self.mist_fsr2_masked
            } else {
                &self.mist
            });
            pass.set_bind_group(0, frame, &[]);
            pass.set_vertex_buffer(0, transient.mist.slice(..));
            pass.draw(0..6, 0..transient.mist_positions.len() as u32);
        }
    }
}
