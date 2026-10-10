//! Deterministic SMAA 1x at SMAA 2.8's quality presets; Medium matches the
//! browser's Three.js SMAANode. Input is tone-mapped, display-linear colour,
//! whose sRGB encoding edges are detected on; HUD follows output. See README.md and the adjacent upstream
//! licenses for shader/atlas provenance.
use crate::settings::SmaaQuality;
use std::cell::RefCell;

pub(crate) static SMAA: crate::shading::Module = crate::shading::Module {
    name: "smaa",
    source: include_str!("smaa.wgsl"),
    deps: &[&crate::shading::SRGB],
};
/// The entry points SMAA's pipelines are created with: each pass's
/// fragment and vertex.
pub(crate) const DETECT_ENTRY: &str = "detect";
pub(crate) const DETECT_VERTEX_ENTRY: &str = "detect_vertex";
pub(crate) const CALCULATE_ENTRY: &str = "calculate";
pub(crate) const CALCULATE_VERTEX_ENTRY: &str = "calculate_vertex";
pub(crate) const BLEND_ENTRY: &str = "blend";
pub(crate) const BLEND_VERTEX_ENTRY: &str = "blend_vertex";

struct Bindings {
    input: wgpu::TextureView,
    groups: [wgpu::BindGroup; 3],
}
pub(crate) struct Smaa {
    size: [u32; 2],
    bindings: RefCell<Option<Bindings>>,
    edges: wgpu::TextureView,
    weights: wgpu::TextureView,
    area: wgpu::TextureView,
    search: wgpu::TextureView,
    linear: wgpu::Sampler,
    point: wgpu::Sampler,
    inverse_size: wgpu::Buffer,
    shader: wgpu::ShaderModule,
    /// The preset `detect` and `calculate` run.
    quality: SmaaQuality,
    detect: wgpu::RenderPipeline,
    calculate: wgpu::RenderPipeline,
    blend: wgpu::RenderPipeline,
}

/// SMAA.hlsl's `SMAA_PRESET_*` for `quality`, as `smaa.wgsl`'s pipeline
/// constants: the threshold and search steps, and High's and Ultra's
/// diagonal search steps and corner rounding, whose detection Low and
/// Medium disable.
fn preset(quality: SmaaQuality) -> [(&'static str, f64); 6] {
    let (threshold, steps, diagonal_and_corners) = match quality {
        SmaaQuality::Low => (0.15, 4, None),
        SmaaQuality::Medium => (0.1, 8, None),
        SmaaQuality::High => (0.1, 16, Some((8, 25))),
        SmaaQuality::Ultra => (0.05, 32, Some((16, 25))),
    };
    let detects = f64::from(u8::from(diagonal_and_corners.is_some()));
    let (diagonal_steps, rounding) = diagonal_and_corners.unwrap_or_default();
    [
        ("SMAA_THRESHOLD", threshold),
        ("SMAA_MAX_SEARCH_STEPS", f64::from(steps)),
        ("SMAA_DIAG_DETECTION", detects),
        ("SMAA_MAX_SEARCH_STEPS_DIAG", f64::from(diagonal_steps)),
        ("SMAA_CORNER_DETECTION", detects),
        ("SMAA_CORNER_ROUNDING", f64::from(rounding)),
    ]
}

fn target(device: &wgpu::Device, size: [u32; 2], label: &str) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | if cfg!(test) {
                    wgpu::TextureUsages::COPY_SRC
                } else {
                    wgpu::TextureUsages::empty()
                },
            view_formats: &[],
        })
        .create_view(&Default::default())
}

fn lookup(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bytes: &[u8],
    label: &str,
) -> Result<wgpu::TextureView, image::ImageError> {
    let pixels = image::load_from_memory(bytes)?.to_rgba8();
    let size = wgpu::Extent3d {
        width: pixels.width(),
        height: pixels.height(),
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    crate::counters::write_texture(
        queue,
        texture.as_image_copy(),
        pixels.as_raw(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.width * 4),
            rows_per_image: Some(size.height),
        },
        size,
    );
    Ok(texture.create_view(&Default::default()))
}

/// A pipeline of `shader`'s `vertex_entry` and `entry` into `format`, with
/// its pipeline `constants`.
fn pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    entry: &str,
    vertex_entry: &str,
    format: wgpu::TextureFormat,
    constants: &[(&str, f64)],
) -> wgpu::RenderPipeline {
    let options = || wgpu::PipelineCompilationOptions {
        constants,
        ..Default::default()
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(entry),
        layout: None,
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vertex_entry),
            compilation_options: options(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(entry),
            compilation_options: options(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn inverse_size(device: &wgpu::Device, size: [u32; 2]) -> wgpu::Buffer {
    let values = [
        (1.0 / f64::from(size[0])) as f32,
        (1.0 / f64::from(size[1])) as f32,
    ];
    crate::counters::buffer_init(
        device,
        &wgpu::util::BufferInitDescriptor {
            label: Some("SMAA inverse dimensions"),
            contents: bytemuck::cast_slice(&values),
            usage: wgpu::BufferUsages::UNIFORM,
        },
    )
}

fn texture(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}
fn sampler(binding: u32, sampler: &wgpu::Sampler) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::Sampler(sampler),
    }
}

impl Smaa {
    /// SMAA at `quality` into `output_format`.
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        output_format: wgpu::TextureFormat,
        quality: SmaaQuality,
    ) -> Result<Self, image::ImageError> {
        let size = [width.max(1), height.max(1)];
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("SMAA 1x"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&SMAA]).into()),
        });
        let [detect, calculate] = Self::preset_pipelines(device, &shader, quality);
        Ok(Self {
            size,
            bindings: RefCell::new(None),
            inverse_size: inverse_size(device, size),
            edges: target(device, size, "SMAA color edges"),
            weights: target(device, size, "SMAA neighborhood weights"),
            area: lookup(device, queue, include_bytes!("area.png"), "SMAA area atlas")?,
            search: lookup(
                device,
                queue,
                include_bytes!("search.png"),
                "SMAA search atlas",
            )?,
            linear: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("SMAA linear clamp"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            point: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("SMAA point clamp"),
                ..Default::default()
            }),
            detect,
            calculate,
            // Blending is the same at every preset.
            blend: pipeline(
                device,
                &shader,
                BLEND_ENTRY,
                BLEND_VERTEX_ENTRY,
                output_format,
                &preset(quality),
            ),
            shader,
            quality,
        })
    }

    /// The edge detection and weight pipelines of `quality`'s preset.
    fn preset_pipelines(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        quality: SmaaQuality,
    ) -> [wgpu::RenderPipeline; 2] {
        let constants = preset(quality);
        [
            (DETECT_ENTRY, DETECT_VERTEX_ENTRY),
            (CALCULATE_ENTRY, CALCULATE_VERTEX_ENTRY),
        ]
        .map(|(entry, vertex)| {
            pipeline(
                device,
                shader,
                entry,
                vertex,
                wgpu::TextureFormat::Rgba16Float,
                &constants,
            )
        })
    }

    /// Runs at `quality` from now on, rebuilding the pipelines whose preset
    /// changed.
    pub(crate) fn set_quality(&mut self, device: &wgpu::Device, quality: SmaaQuality) {
        if quality != self.quality {
            [self.detect, self.calculate] = Self::preset_pipelines(device, &self.shader, quality);
            self.quality = quality;
            // Groups made for the old pipelines' layouts.
            self.bindings.get_mut().take();
        }
    }

    /// Replaces only size-dependent targets; pipelines and atlases stay resident.
    pub(crate) fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let size = [width.max(1), height.max(1)];
        if size != self.size {
            self.bindings.get_mut().take();
            self.size = size;
            self.inverse_size = inverse_size(device, size);
            self.edges = target(device, size, "SMAA color edges");
            self.weights = target(device, size, "SMAA neighborhood weights");
        }
    }

    /// Source and destination must be distinct views with the current frame size.
    /// Each preset runs the same three passes with no temporal state or jitter.
    pub(crate) fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let mut bindings = self.bindings.borrow_mut();
        if bindings
            .as_ref()
            .is_none_or(|bindings| bindings.input != *input)
        {
            let detect = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("SMAA detect input"),
                layout: &self.detect.get_bind_group_layout(0),
                entries: &[
                    texture(0, input),
                    sampler(1, &self.linear),
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: self.inverse_size.as_entire_binding(),
                    },
                ],
            });
            let calculate = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("SMAA calculate input"),
                layout: &self.calculate.get_bind_group_layout(0),
                entries: &[
                    sampler(1, &self.linear),
                    texture(2, &self.edges),
                    texture(3, &self.area),
                    texture(4, &self.search),
                    sampler(5, &self.point),
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: self.inverse_size.as_entire_binding(),
                    },
                ],
            });
            let blend = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("SMAA blend input"),
                layout: &self.blend.get_bind_group_layout(0),
                entries: &[
                    texture(0, input),
                    sampler(1, &self.linear),
                    texture(6, &self.weights),
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: self.inverse_size.as_entire_binding(),
                    },
                ],
            });
            *bindings = Some(Bindings {
                input: input.clone(),
                groups: [detect, calculate, blend],
            });
        }
        let [detect, calculate, blend] = &bindings.as_ref().unwrap().groups;
        for (pipeline, group, view) in [
            (&self.detect, detect, &self.edges),
            (&self.calculate, calculate, &self.weights),
            (&self.blend, blend, output),
        ] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("SMAA 1x"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: timing.and_then(|t| t.render_pass("SMAA")),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(super) mod tests;
