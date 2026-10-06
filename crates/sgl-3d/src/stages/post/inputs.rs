//! What every post pass binds as group 0: a scene view, a bloom view, the
//! linear sampler and the bloom settings uniform (`bloom::BloomSettings`).
//! One bind group per pair of views, kept until an input is replaced; and the
//! full-screen pipeline and pass the post passes share.
use crate::view::targets::attachment;
use std::cell::RefCell;

/// Group 0 of every post pass and its full-screen vertex.
pub(crate) static INPUTS: crate::shading::Module = crate::shading::Module {
    name: "post_inputs",
    source: include_str!("inputs.wgsl"),
    deps: &[&crate::shading::FULLSCREEN],
};
/// The full-screen vertex entry point every post pipeline is created with.
pub(crate) const VS_ENTRY: &str = "vs";

struct Binding {
    scene: wgpu::TextureView,
    bloom: wgpu::TextureView,
    group: wgpu::BindGroup,
}

pub(super) struct Inputs {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// `bloom::BloomSettings`.
    pub(super) settings: wgpu::Buffer,
    groups: RefCell<Vec<Binding>>,
}

impl Inputs {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            layout: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("HDR presentation inputs"),
                entries: &[
                    sampled(0, true),
                    sampled(1, true),
                    sampler_entry(2),
                    uniform_entry(3),
                ],
            }),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            settings: crate::counters::buffer_init(
                device,
                &wgpu::util::BufferInitDescriptor {
                    label: Some("bloom settings"),
                    contents: bytemuck::bytes_of(&super::bloom::BloomSettings::default()),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                },
            ),
            groups: RefCell::new(Vec::new()),
        }
    }

    pub fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    /// Drops the cached groups, which may hold a replaced input's views.
    pub fn forget(&mut self) {
        self.groups.get_mut().clear();
    }

    /// The group binding `scene` and `bloom`, made once per pair.
    pub fn group(
        &self,
        device: &wgpu::Device,
        scene: &wgpu::TextureView,
        bloom: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        if let Some(binding) = self
            .groups
            .borrow()
            .iter()
            .find(|binding| binding.scene == *scene && binding.bloom == *bloom)
        {
            return binding.group.clone();
        }
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("HDR pass"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(scene),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(bloom),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.settings.as_entire_binding(),
                },
            ],
        });
        self.groups.borrow_mut().push(Binding {
            scene: scene.clone(),
            bloom: bloom.clone(),
            group: group.clone(),
        });
        group
    }
}

/// A fragment-stage 2D float texture entry.
pub(super) fn sampled(binding: u32, filterable: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

/// A fragment-stage uniform buffer entry.
pub(super) fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// A full-screen pipeline of `shader`'s `fragment` over `layouts`, writing
/// `formats` with `blend`.
pub(super) fn pipeline(
    device: &wgpu::Device,
    layouts: &[Option<&wgpu::BindGroupLayout>],
    shader: &wgpu::ShaderModule,
    fragment: &str,
    formats: &[wgpu::TextureFormat],
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(fragment),
        bind_group_layouts: layouts,
        immediate_size: 0,
    });
    let targets: Vec<_> = formats
        .iter()
        .map(|format| {
            Some(wgpu::ColorTargetState {
                format: *format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })
        })
        .collect();
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fragment),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(VS_ENTRY),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment),
            compilation_options: Default::default(),
            targets: &targets,
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// One full-screen pass of `pipeline` with `group` into `target`.
pub(super) fn draw(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::RenderPipeline,
    group: &wgpu::BindGroup,
    target: &wgpu::TextureView,
    label: &str,
    timestamp_writes: Option<wgpu::RenderPassTimestampWrites<'_>>,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        timestamp_writes,
        color_attachments: &[attachment(target)],
        ..Default::default()
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, group, &[]);
    pass.draw(0..3, 0..1);
}
