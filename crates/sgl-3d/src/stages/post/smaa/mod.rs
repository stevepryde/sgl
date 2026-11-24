//! Deterministic SMAA 1x at SMAA 2.8's quality presets; Medium matches the
//! browser's Three.js SMAANode. Input is tone-mapped, display-linear colour,
//! whose sRGB encoding edges are detected on; HUD follows output. See README.md and the adjacent upstream
//! licenses for shader/atlas provenance.
use crate::settings::SmaaQuality;
use crate::view::compile::{Compiled, Compiler, Creation, Creator, SharedModule};

pub(crate) static SMAA: crate::shading::Module = crate::shading::Module {
    name: "smaa",
    source: sgl_post_fx::embed_wgsl!("smaa.wgsl"),
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
/// What SMAA holds at the scene size, only while it runs.
struct Targets {
    edges: wgpu::TextureView,
    weights: wgpu::TextureView,
    inverse_size: wgpu::Buffer,
}
impl Targets {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        Self {
            edges: target(device, size, "SMAA color edges"),
            weights: target(device, size, "SMAA neighborhood weights"),
            inverse_size: inverse_size(device, size),
        }
    }
}
pub(crate) struct Smaa {
    size: [u32; 2],
    /// From the first frame SMAA runs at `size` until it stops or resizes.
    targets: Option<Targets>,
    bindings: Option<Bindings>,
    /// The renderer's tables, from the first frame SMAA may run.
    tables: Option<Tables>,
    linear: wgpu::Sampler,
    point: wgpu::Sampler,
    output_format: wgpu::TextureFormat,
    shader: SharedModule,
    /// Blending, the same at every preset.
    blend: Compiled<wgpu::RenderPipeline>,
    /// The edge detection and weight pipelines of the preset last asked
    /// for, `asked`: another one replaces them, compiled or still a job.
    preset: Compiled<Preset>,
    asked: Option<SmaaQuality>,
}

/// The frame's three pipelines.
struct Pipelines<'a> {
    detect: &'a wgpu::RenderPipeline,
    calculate: &'a wgpu::RenderPipeline,
    blend: &'a wgpu::RenderPipeline,
}

struct Preset {
    detect: wgpu::RenderPipeline,
    calculate: wgpu::RenderPipeline,
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

/// SMAA's area and search lookup textures, which every view of a renderer
/// shares. The renderer makes them once, the first frame a view may run
/// SMAA (`renderer::frame::render`, a planning pass included), and holds
/// nothing before: decoding them takes tens of kilobytes of stack in a
/// debug build, which a stage deep in a frame cannot spare on the
/// browser's 1 MiB stack (#893).
#[derive(Clone)]
pub(crate) struct Tables {
    area: wgpu::TextureView,
    search: wgpu::TextureView,
}

impl Tables {
    /// # Panics
    ///
    /// If the embedded PNGs do not decode, which every SMAA test rules out.
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self {
            area: lookup(device, queue, include_bytes!("area.png"), "SMAA area atlas"),
            search: lookup(
                device,
                queue,
                include_bytes!("search.png"),
                "SMAA search atlas",
            ),
        }
    }
}

fn lookup(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bytes: &[u8],
    label: &str,
) -> wgpu::TextureView {
    let pixels = image::load_from_memory(bytes)
        .expect("SMAA's embedded lookup tables decode")
        .to_rgba8();
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
    texture.create_view(&Default::default())
}

/// A pipeline of `shader`'s `vertex_entry` and `entry` into `format`, with
/// its pipeline `constants`.
fn pipeline(
    creator: &Creator<'_>,
    shader: &wgpu::ShaderModule,
    entry: &str,
    vertex_entry: &str,
    format: wgpu::TextureFormat,
    constants: &[(&str, f64)],
) -> Creation<wgpu::RenderPipeline> {
    let options = || wgpu::PipelineCompilationOptions {
        constants,
        ..Default::default()
    };
    creator.render_pipeline(&wgpu::RenderPipelineDescriptor {
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
    /// SMAA, holding no program: the first frame it runs asks for them.
    /// It runs once it holds the renderer's tables (`hold_tables`).
    pub(crate) fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        output_format: wgpu::TextureFormat,
    ) -> Self {
        let size = [width.max(1), height.max(1)];
        Self {
            size,
            targets: None,
            bindings: None,
            tables: None,
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
            output_format,
            shader: SharedModule::composed("SMAA 1x", &SMAA),
            blend: Compiled::default(),
            preset: Compiled::default(),
            asked: None,
        }
    }
}

impl Preset {
    /// The edge detection and weight pipelines of `quality`'s preset.
    fn new(
        creator: &Creator<'_>,
        shader: &wgpu::ShaderModule,
        quality: SmaaQuality,
    ) -> Creation<Self> {
        let constants = preset(quality);
        Creation::array(
            [
                (DETECT_ENTRY, DETECT_VERTEX_ENTRY),
                (CALCULATE_ENTRY, CALCULATE_VERTEX_ENTRY),
            ]
            .map(|(entry, vertex)| {
                pipeline(
                    creator,
                    shader,
                    entry,
                    vertex,
                    wgpu::TextureFormat::Rgba16Float,
                    &constants,
                )
            }),
        )
        .map(|[detect, calculate]| Self { detect, calculate })
    }
}

impl Smaa {
    /// Whether SMAA holds its scene-size targets.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn holds_targets(&self) -> bool {
        self.targets.is_some()
    }

    /// Holds the renderer's `tables`, before the first frame SMAA may run.
    pub(crate) fn hold_tables(&mut self, tables: &Tables) {
        self.tables.get_or_insert_with(|| tables.clone());
    }

    /// Drop groups retaining a presentation target that no longer runs.
    pub(crate) fn forget_inputs(&mut self) {
        self.bindings = None;
    }

    /// Releases the scene-size targets and the groups that hold them, while
    /// SMAA does not run; pipelines and atlases stay resident.
    pub(crate) fn release(&mut self) {
        self.bindings = None;
        self.targets = None;
    }

    /// Runs at this size from now on, releasing targets of another size;
    /// the next frame SMAA runs makes them.
    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        let size = [width.max(1), height.max(1)];
        if size != self.size {
            self.size = size;
            self.release();
        }
    }

    /// Source and destination must be distinct views with the current frame size.
    /// Each preset runs the same three passes with no temporal state or jitter.
    pub(crate) fn encode(
        &mut self,
        (device, compiler): (&wgpu::Device, &Compiler),
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
        quality: SmaaQuality,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        // Another preset replaces the detection and weight pipelines,
        // whether the last one's were compiled or are still a job.
        if self
            .asked
            .replace(quality)
            .is_some_and(|last| last != quality)
        {
            self.preset.clear();
            self.bindings = None;
        }
        let (shader, output_format) = (&self.shader, self.output_format);
        let blend = self.blend.get(compiler, "SMAA blend", || {
            let shader = shader.clone();
            move |creator: &Creator<'_>| {
                pipeline(
                    creator,
                    shader.get(creator.device()),
                    BLEND_ENTRY,
                    BLEND_VERTEX_ENTRY,
                    output_format,
                    &preset(quality),
                )
            }
        });
        let preset = self.preset.get(compiler, "SMAA", || {
            let shader = shader.clone();
            move |creator: &Creator<'_>| Preset::new(creator, shader.get(creator.device()), quality)
        });
        // Both are asked for before a planning pass that lacks one leaves
        // the passes out.
        let (Some(blend), Some(preset)) = (blend, preset) else {
            return;
        };
        let pipelines = Pipelines {
            detect: &preset.detect,
            calculate: &preset.calculate,
            blend,
        };
        let tables = self
            .tables
            .as_ref()
            .expect("the renderer readies SMAA's tables before a frame it may run in");
        let size = self.size;
        let targets = self
            .targets
            .get_or_insert_with(|| Targets::new(device, size));
        let bindings = &mut self.bindings;
        if bindings
            .as_ref()
            .is_none_or(|bindings| bindings.input != *input)
        {
            let detect = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("SMAA detect input"),
                layout: &pipelines.detect.get_bind_group_layout(0),
                entries: &[
                    texture(0, input),
                    sampler(1, &self.linear),
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: targets.inverse_size.as_entire_binding(),
                    },
                ],
            });
            let calculate = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("SMAA calculate input"),
                layout: &pipelines.calculate.get_bind_group_layout(0),
                entries: &[
                    sampler(1, &self.linear),
                    texture(2, &targets.edges),
                    texture(3, &tables.area),
                    texture(4, &tables.search),
                    sampler(5, &self.point),
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: targets.inverse_size.as_entire_binding(),
                    },
                ],
            });
            let blend = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("SMAA blend input"),
                layout: &pipelines.blend.get_bind_group_layout(0),
                entries: &[
                    texture(0, input),
                    sampler(1, &self.linear),
                    texture(6, &targets.weights),
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: targets.inverse_size.as_entire_binding(),
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
            (pipelines.detect, detect, &targets.edges),
            (pipelines.calculate, calculate, &targets.weights),
            (pipelines.blend, blend, output),
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
