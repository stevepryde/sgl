//! Deform, prepare's GPU step: morphs and skins each deforming instance
//! whose deformation changed since the last submitted frame, once per frame
//! in one compute pass, into its deformed vertices in the scene source,
//! which every geometry pass reads (the architecture's "Deformation").
//!
//! Reads: the scene's deformations for the frame (`Scene::deformations`)
//! and the scene source. Writes: the deforming instances' deformed vertices
//! in the scene source. Honours: nothing. Timing group: "deform".
use crate::shading::deformation::DeformDispatch;
use crate::view::frame::FrameContext;

pub(crate) static DEFORM: crate::shading::Module = crate::shading::Module {
    name: "deform",
    source: include_str!("deform.wgsl"),
    deps: &[&crate::shading::SCENE_SOURCE, &crate::shading::DEFORMATION],
};

/// The bytes between dispatch records: the device's uniform offset
/// alignment.
fn stride(device: &wgpu::Device) -> u64 {
    u64::from(device.limits().min_uniform_buffer_offset_alignment)
        .max(std::mem::size_of::<DeformDispatch>() as u64)
}

pub(crate) struct Deform {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    /// The frame's dispatch records, one per `stride`.
    records: wgpu::Buffer,
    stride: u64,
    /// The bind group over the scene source it was made for and `records`.
    group: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    staging: Vec<u8>,
}

impl Deform {
    pub fn new(device: &wgpu::Device) -> Self {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: binding == 1,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("deform"),
            entries: &[
                entry(0, wgpu::BufferBindingType::Storage { read_only: false }),
                entry(1, wgpu::BufferBindingType::Uniform),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("deform"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&DEFORM]).into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("deform"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let stride = stride(device);
        Self {
            pipeline: device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("deform"),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some("deform"),
                compilation_options: Default::default(),
                cache: None,
            }),
            layout,
            records: records(device, stride),
            stride,
            group: None,
            staging: Vec::new(),
        }
    }

    /// Writes the frame's deformations, before any pass draws scene
    /// geometry.
    pub fn encode(&mut self, ctx: &mut FrameContext<'_>) {
        self.dispatch(ctx.device, ctx.queue, ctx.encoder, ctx.scene, ctx.timing);
    }

    /// Writes `scene`'s deformations for the frame it prepared.
    pub(crate) fn dispatch(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene: &crate::Scene,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let work = &scene.deformations;
        if work.is_empty() {
            return;
        }
        let size = work.len() as u64 * self.stride;
        if self.records.size() < size {
            self.records = records(device, size.next_power_of_two());
            self.group = None;
        }
        self.staging.clear();
        self.staging.resize(size as usize, 0);
        for (record, chunk) in work
            .iter()
            .zip(self.staging.chunks_mut(self.stride as usize))
        {
            chunk[..std::mem::size_of::<DeformDispatch>()]
                .copy_from_slice(bytemuck::bytes_of(record));
        }
        crate::counters::write_buffer(queue, &self.records, 0, &self.staging);
        let source = scene.rays.source();
        if self.group.as_ref().is_none_or(|(bound, _)| bound != source) {
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("deform"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: source.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.records,
                            offset: 0,
                            size: wgpu::BufferSize::new(
                                std::mem::size_of::<DeformDispatch>() as u64
                            ),
                        }),
                    },
                ],
            });
            self.group = Some((source.clone(), group));
        }
        let (_, group) = self.group.as_ref().unwrap();
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("deform"),
            timestamp_writes: timing.and_then(|t| t.compute_pass("deform")),
        });
        pass.set_pipeline(&self.pipeline);
        for (index, record) in work.iter().enumerate() {
            pass.set_bind_group(0, group, &[(index as u64 * self.stride) as u32]);
            pass.dispatch_workgroups(record.vertex_count.div_ceil(64), 1, 1);
        }
    }
}

fn records(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some("deform dispatches"),
            size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
    )
}
