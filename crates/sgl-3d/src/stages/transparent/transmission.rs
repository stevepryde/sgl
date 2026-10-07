//! The transmission copy: the composed frame with its mip chain, which the
//! transparent stage's draw onto it samples for the light transmitted
//! through a transmissive surface (`shading/transmission.wgsl`). It is made
//! in a frame whose blended list shows a transmissive material, on a device
//! of the Extended binding tier, before the blended draws onto the composed
//! frame, as three.js r185's WebGPU renderer copies its framebuffer, with
//! mips, before its transparent objects (src/nodes/display/
//! ViewportTextureNode.js 159-200, `viewportOpaqueMipTexture` 239-256;
//! commit 2431a09f, MIT, see `stages/post/smaa/LICENSE-three.txt`): the
//! render size in the composed frame's format (Rgba16Float), every level to
//! one texel. It is allocated in the first frame that needs it, and again
//! after a resize, as heat distortion's snapshot is, and keeps no history.
use crate::shading::gbuffer::COLOR;
use crate::view::targets::{attachment, hold};

pub(crate) static COPY: crate::shading::Module = crate::shading::Module {
    name: "transmission_copy",
    source: include_str!("transmission.wgsl"),
    deps: &[&crate::shading::FULLSCREEN_VS],
};
/// The entry points the copy's pipelines are created with, beside
/// `shading::FULLSCREEN_VS_ENTRY`: the first level's copy and each further
/// level's downsample.
pub(crate) const COPY_FS_ENTRY: &str = "transmission_copy_fs";
pub(crate) const DOWNSAMPLE_FS_ENTRY: &str = "transmission_downsample_fs";

pub(crate) struct Transmission {
    layout: wgpu::BindGroupLayout,
    copy: wgpu::RenderPipeline,
    downsample: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    levels: Option<Levels>,
}

/// The copy's texture, every level of it, and for each level but the first
/// its attachment and the group that binds the level above.
struct Levels {
    view: wgpu::TextureView,
    first: wgpu::TextureView,
    further: Vec<(wgpu::TextureView, wgpu::BindGroup)>,
}

impl Transmission {
    pub fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("transmission copy source"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("transmission copy"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&COPY]).into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("transmission copy"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |label, fragment| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(crate::shading::FULLSCREEN_VS_ENTRY),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(COLOR.into())],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            copy: pipeline("transmission copy", COPY_FS_ENTRY),
            downsample: pipeline("transmission copy downsample", DOWNSAMPLE_FS_ENTRY),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("transmission copy downsample"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            layout,
            levels: None,
        }
    }

    /// The copy, every level of it, once a frame has made it.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn copy(&self) -> Option<&wgpu::TextureView> {
        self.levels.as_ref().map(|levels| &levels.view)
    }

    /// Copies `composed` (the composed frame) into the copy's first level and
    /// builds its further levels, allocating the copy at its size first where
    /// it has none of that size. Returns the copy, every level of it.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        composed: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) -> &wgpu::TextureView {
        let size = composed.texture().size();
        let (layout, sampler) = (&self.layout, &self.sampler);
        let levels = hold(&mut self.levels, Levels::texture, size, || {
            Levels::new(device, layout, sampler, size)
        });
        // The composed frame is bound afresh each frame, as heat's
        // snapshot's source is, since the targets may replace it.
        let source = group(device, layout, sampler, composed);
        let passes = std::iter::once((&self.copy, &levels.first, &source)).chain(
            levels
                .further
                .iter()
                .map(|(target, group)| (&self.downsample, target, group)),
        );
        for (pipeline, target, group) in passes {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("transmission copy"),
                color_attachments: &[attachment(target)],
                timestamp_writes: timing.and_then(|t| t.render_pass("transmission copy")),
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
        &levels.view
    }
}

impl Levels {
    /// The copy's texture.
    fn texture(&self) -> &wgpu::Texture {
        self.view.texture()
    }

    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        size: wgpu::Extent3d,
    ) -> Self {
        let count = size.max_mips(wgpu::TextureDimension::D2);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("transmission copy"),
            size,
            mip_level_count: count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let level = |level| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        Self {
            view: texture.create_view(&Default::default()),
            first: level(0),
            further: (1..count)
                .map(|index| {
                    (
                        level(index),
                        group(device, layout, sampler, &level(index - 1)),
                    )
                })
                .collect(),
        }
    }
}

/// The group that binds `source`, the level a pass reads.
fn group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    source: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("transmission copy source"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(source),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}
