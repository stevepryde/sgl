//! Caller-positioned transparent mist (the mist positions are the scene's
//! transient geometry), fogged where it stands.
use crate::scene::transient::Transient;
use crate::shading::gbuffer::COLOR as HDR;
use crate::shading::vertex::{Attribute, VertexLayout};
use crate::view::targets::mask_targets;
use glam::Vec3;

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
        &crate::shading::TEMPORAL_MASKS,
        &MIST_NOISE,
    ],
};
/// The entry points the mist's pipelines are created with.
pub(crate) const MIST_VS_ENTRY: &str = "mist_vs";
pub(crate) const MIST_FS_ENTRY: &str = "mist_fs";
pub(crate) const MIST_MASKED_FS_ENTRY: &str = "mist_masked_fs";
/// The mist's instance buffer, one position (`Transient::mist_positions`)
/// per quad, back to front from the view's eye, read by `mist_vs`.
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
    /// `mist` also writing the temporal masks (`mask_targets`).
    mist_masked: wgpu::RenderPipeline,
    /// The scene's mist positions as the view's last `order` sorted them,
    /// and the buffer it uploaded them to, which only this view writes.
    positions: Vec<[f32; 3]>,
    buffer: wgpu::Buffer,
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
                        MIST_MASKED_FS_ENTRY
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
            mist_masked: pipeline(true),
            positions: Vec::new(),
            buffer: buffer(device, 1),
        }
    }

    /// Orders the scene's mist back to front from the view's `eye`, as
    /// Three.js orders transparent objects for each camera it renders, and
    /// uploads it for the frame's draws, growing the buffer as needed.
    pub fn order(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        transient: &Transient,
        eye: Vec3,
    ) {
        self.positions.clear();
        self.positions.extend_from_slice(&transient.mist_positions);
        self.positions.sort_by(|a, b| {
            Vec3::from_array(*b)
                .distance_squared(eye)
                .total_cmp(&Vec3::from_array(*a).distance_squared(eye))
        });
        let bytes: &[u8] = bytemuck::cast_slice(&self.positions);
        if bytes.len() as u64 > self.buffer.size() {
            self.buffer = buffer(device, self.positions.len() as u64);
        }
        if !bytes.is_empty() {
            crate::counters::write_buffer(queue, &self.buffer, 0, bytes);
        }
    }

    /// The mist as `order` left it, blended into `destination` and tested
    /// against `depth` without writing it, in timing group `group`.
    /// `temporal_masks`, already cleared this frame, also receive it.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &wgpu::BindGroup,
        destination: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        temporal_masks: Option<&[wgpu::TextureView; 2]>,
        timing: Option<&crate::timing::GpuTiming>,
        group: &'static str,
    ) {
        if !self.positions.is_empty() {
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
            let attachments = match temporal_masks {
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
            pass.set_pipeline(if temporal_masks.is_some() {
                &self.mist_masked
            } else {
                &self.mist
            });
            pass.set_bind_group(0, frame, &[]);
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            pass.draw(0..6, 0..self.positions.len() as u32);
        }
    }
}

fn buffer(device: &wgpu::Device, positions: u64) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some("view's mist positions"),
            size: positions * std::mem::size_of::<[f32; 3]>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
    )
}
