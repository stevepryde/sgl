//! World-space reflection rays for screen-space misses on moving objects, and
//! their denoiser, as Wicked Engine's RT reflections run them
//! (`Postprocess_RTReflection`, wiRenderer.cpp): trace at half resolution,
//! spatial resolve, temporal accumulation, bilateral upsample.
use super::cached_group::CachedGroup;
use crate::shading;
use crate::view::history::HistoryFrame;
use crate::view::pipelines::LitConstants;
use crate::view::reflection_camera;
use glam::Mat4;
use std::collections::HashMap;

/// Wicked's default RT reflection downscale.
const DOWNSCALE: u32 = 2;
/// How far a reflection ray looks for moving objects, in metres: Wicked's
/// default RT reflection range (4323a33 `Postprocess_RTReflection`,
/// wiRenderer.h).
const RANGE: f32 = 1000.;

pub(crate) struct Inputs<'a> {
    /// The opaque depth, and the surface depth nearer than it under a
    /// receiver, whose pixels the rays skip.
    pub depth: &'a wgpu::TextureView,
    pub surface_depth: &'a wgpu::TextureView,
    pub normal: &'a wgpu::TextureView,
    pub material: &'a wgpu::TextureView,
    pub f0: &'a wgpu::TextureView,
    pub motion: &'a wgpu::TextureView,
    pub source_id: &'a wgpu::TextureView,
    pub screen_space: &'a wgpu::TextureView,
    pub camera: reflection_camera::Camera,
    /// Alpha roughness below which screen-space reflections trace a lobe.
    pub traced: f32,
}

/// `WorldParams` in world_reflections_common.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    inverse_view_projection: [[f32; 4]; 4],
    previous_view_projection: [[f32; 4]; 4],
    eye: [f32; 4],
    full: [f32; 4],
    reduced: [f32; 4],
    frame: u32,
    downscale: u32,
    traced: f32,
    range: f32,
}

struct Targets {
    full: [u32; 2],
    reduced: [u32; 2],
    indirect: wgpu::TextureView,
    direction_pdf: wgpu::TextureView,
    length: wgpu::TextureView,
    resolve: wgpu::TextureView,
    resolve_variance: wgpu::TextureView,
    reprojection: wgpu::TextureView,
    temporal: [wgpu::TextureView; 2],
    temporal_variance: [wgpu::TextureView; 2],
    depth: [wgpu::TextureView; 2],
    output: wgpu::TextureView,
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    size: [u32; 2],
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::TextureView {
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
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | usage,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

impl Targets {
    fn new(device: &wgpu::Device, full: [u32; 2]) -> Self {
        let reduced = full.map(|v| (v / DOWNSCALE).max(1));
        let attachment = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let storage = wgpu::TextureUsages::STORAGE_BINDING;
        let hdr = crate::shading::gbuffer::COLOR;
        let float = wgpu::TextureFormat::R32Float;
        let half = |label, format, usage| texture(device, label, reduced, format, usage);
        Self {
            full,
            reduced,
            indirect: half("world reflection radiance", hdr, attachment),
            direction_pdf: half("world reflection direction and pdf", hdr, attachment),
            length: half("world reflection ray length", float, attachment),
            resolve: half("world reflection resolve", hdr, storage),
            resolve_variance: half("world reflection resolve variance", hdr, storage),
            reprojection: half("world reflection reprojection depth", float, storage),
            temporal: [0, 1].map(|_| half("world reflection temporal", hdr, storage)),
            temporal_variance: [0, 1]
                .map(|_| half("world reflection temporal variance", hdr, storage)),
            depth: [0, 1].map(|_| half("world reflection depth history", float, storage)),
            output: texture(device, "world reflections", full, hdr, storage),
        }
    }
}

/// A denoise pass with its group 0, one for each half of the history pair
/// the frame writes, and its receivers at group 3.
struct Denoise {
    pipeline: wgpu::ComputePipeline,
    groups: [CachedGroup; 2],
    receivers: CachedGroup,
}

impl Denoise {
    fn new(pipeline: wgpu::ComputePipeline) -> Self {
        let group = |index| CachedGroup::new(pipeline.get_bind_group_layout(index));
        Self {
            groups: [group(0), group(0)],
            receivers: group(3),
            pipeline,
        }
    }
}

/// World-space rays and their denoiser. Owns its pipelines and their groups,
/// its targets (reallocated when the render size changes) and its history,
/// which continues across consecutive valid frames.
pub(crate) struct WorldReflections {
    trace_shader: wgpu::ShaderModule,
    trace_layout: wgpu::PipelineLayout,
    /// The trace's pipelines for each set of lit constants, each created
    /// when a frame first needs it, as the geometry pipelines specialise on
    /// the scene's rectangle lights and decals.
    trace: HashMap<LitConstants, wgpu::RenderPipeline>,
    /// The trace's receivers at group 3.
    trace_group: CachedGroup,
    resolve: Denoise,
    temporal: Denoise,
    upsample: Denoise,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    targets: Targets,
    /// Frames since history last reset; 0 resets it.
    frame: u32,
    /// Scene frame last encoded, matching the TAA/SSR continuity rule.
    previous_scene_frame: Option<u32>,
    previous_view_projection: Option<Mat4>,
}

static COMMON: shading::Module = shading::Module {
    name: "world_reflections_common",
    source: include_str!("world/world_reflections_common.wgsl"),
    deps: &[&shading::GBUFFER, &shading::DEPTH],
};
/// The trace: the lit layout at group 0 (the camera's ray-hit group, with the
/// installed probes), the scene at group 1 and its receivers at group 3. The
/// denoiser's receivers are at group 3 too, its own bindings at group 0.
pub(crate) static TRACE: shading::Module = shading::Module {
    name: "world_reflections",
    source: include_str!("world/world_reflections.wgsl"),
    deps: &[
        &shading::BIND_LIT,
        &shading::SCENE_RAYS_PORTABLE,
        &shading::SURFACE_RAY,
        &shading::FULLSCREEN,
        &COMMON,
    ],
};
pub(crate) static DENOISE: shading::Module = shading::Module {
    name: "world_reflections_denoise",
    source: include_str!("world/world_reflections_denoise.wgsl"),
    deps: &[&COMMON, &super::TEMPORAL_REPROJECTION, &shading::LUMINANCE],
};

impl WorldReflections {
    /// `lit` and `scene` are group 0's lit layout and group 1's.
    pub fn new(
        device: &wgpu::Device,
        lit: &wgpu::BindGroupLayout,
        scene: &wgpu::BindGroupLayout,
        size: [u32; 2],
    ) -> Self {
        let trace_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world-space reflection rays"),
            source: wgpu::ShaderSource::Wgsl(shading::compose(&[&TRACE]).into()),
        });
        let denoise_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world-space reflection denoise"),
            source: wgpu::ShaderSource::Wgsl(shading::compose(&[&DENOISE]).into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let sampled = |sample_type| wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let unfilterable = sampled(wgpu::TextureSampleType::Float { filterable: false });
        let trace_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world-space reflection receivers"),
            entries: &[
                entry(0, sampled(wgpu::TextureSampleType::Depth)),
                entry(1, unfilterable),
                entry(2, unfilterable),
                entry(3, unfilterable),
                entry(
                    4,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                entry(5, unfilterable),
                entry(6, sampled(wgpu::TextureSampleType::Uint)),
                entry(7, sampled(wgpu::TextureSampleType::Depth)),
            ],
        });
        let trace_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("world-space reflection rays"),
                bind_group_layouts: &[Some(lit), Some(scene), None, Some(&trace_layout)],
                immediate_size: 0,
            });
        let compute = |entry_point| {
            Denoise::new(
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry_point),
                    layout: None,
                    module: &denoise_shader,
                    entry_point: Some(entry_point),
                    compilation_options: Default::default(),
                    cache: None,
                }),
            )
        };
        Self {
            trace_shader,
            trace_layout: trace_pipeline_layout,
            trace: HashMap::new(),
            trace_group: CachedGroup::new(trace_layout),
            resolve: compute("world_resolve"),
            temporal: compute("world_temporal"),
            upsample: compute("world_upsample"),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            params: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("world-space reflection parameters"),
                size: std::mem::size_of::<Params>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            targets: Targets::new(device, size),
            frame: 0,
            previous_scene_frame: None,
            previous_view_projection: None,
        }
    }

    /// Premultiplied radiance of the moving objects each receiver's traced
    /// lobe reflects (rgb), and the share of its rays that hit them (a).
    pub fn output(&self) -> &wgpu::TextureView {
        &self.targets.output
    }

    /// The trace's pipeline compiled with `lit`.
    fn trace(&mut self, device: &wgpu::Device, lit: LitConstants) -> &wgpu::RenderPipeline {
        let (shader, layout) = (&self.trace_shader, &self.trace_layout);
        self.trace.entry(lit).or_insert_with(|| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("world-space reflection rays"),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some("fullscreen_vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: shader,
                    entry_point: Some("world_trace"),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &lit.constants(),
                        ..Default::default()
                    },
                    targets: &[
                        Some(crate::shading::gbuffer::COLOR.into()),
                        Some(crate::shading::gbuffer::COLOR.into()),
                        Some(wgpu::TextureFormat::R32Float.into()),
                    ],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        })
    }

    /// `groups` are the camera's ray-hit lit group 0 (with the installed
    /// probes) and the scene's group 1; `lit_constants`, the scene's
    /// (`LitConstants::of`).
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        [lit, scene]: [&wgpu::BindGroup; 2],
        lit_constants: LitConstants,
        history: HistoryFrame,
        size: [u32; 2],
        input: Inputs<'_>,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        self.trace(device, lit_constants);
        let resized = self.targets.full != size;
        if resized {
            self.targets = Targets::new(device, size);
        }
        // DiligentFX TemporalAntiAliasing::AccumulationBufferInfo::UpdateConstantBuffer
        // and PostFx::prepare: use history only across consecutive valid scene
        // frames. A camera cut or missed encode makes receiver motion insufficient.
        let continuous = history.valid
            && self
                .previous_scene_frame
                .is_some_and(|previous| history.frames == previous.wrapping_add(1));
        if resized || !continuous {
            self.frame = 0;
            self.previous_view_projection = None;
        }
        self.previous_scene_frame = Some(history.frames);
        let view_projection = Mat4::from_cols_array_2d(&input.camera.proj)
            * Mat4::from_cols_array_2d(&input.camera.view);
        let previous = self.previous_view_projection.unwrap_or(view_projection);
        self.previous_view_projection = Some(view_projection);
        let t = &self.targets;
        let [w, h] = t.full.map(|v| v as f32);
        let [rw, rh] = t.reduced.map(|v| v as f32);
        let eye = input.camera.camera_position;
        let near = input.camera.proj[3][2];
        queue.write_buffer(
            &self.params,
            0,
            bytemuck::bytes_of(&Params {
                inverse_view_projection: input.camera.inverse_view_proj,
                previous_view_projection: previous.to_cols_array_2d(),
                eye: [eye[0], eye[1], eye[2], near],
                full: [w, h, 1. / w, 1. / h],
                reduced: [rw, rh, 1. / rw, 1. / rh],
                frame: self.frame,
                downscale: DOWNSCALE,
                traced: input.traced,
                range: RANGE,
            }),
        );
        let current = (self.frame % 2) as usize;
        let history = 1 - current;
        let view = wgpu::BindingResource::TextureView;
        let receivers = [
            (0, view(input.depth)),
            (1, view(input.normal)),
            (2, view(input.material)),
            (3, view(input.f0)),
            (4, self.params.as_entire_binding()),
        ];
        let trace_group = self.trace_group.get(
            device,
            "world-space reflection receivers",
            &[
                receivers.as_slice(),
                &[
                    (5, view(input.screen_space)),
                    (6, view(input.source_id)),
                    (7, view(input.surface_depth)),
                ],
            ]
            .concat(),
        );
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world-space reflection rays"),
                color_attachments: &[
                    crate::view::targets::attachment(&t.indirect),
                    crate::view::targets::attachment(&t.direction_pdf),
                    crate::view::targets::attachment(&t.length),
                ],
                timestamp_writes: timing.and_then(|t| t.render_pass("world reflection rays")),
                ..Default::default()
            });
            pass.set_pipeline(&self.trace[&lit_constants]);
            pass.set_bind_group(0, lit, &[]);
            pass.set_bind_group(1, scene, &[]);
            pass.set_bind_group(3, trace_group, &[]);
            pass.draw(0..3, 0..1);
        }
        let sampler = || wgpu::BindingResource::Sampler(&self.sampler);
        // `slot`: the half of the history pair a pass that binds it writes.
        let dispatch = |denoise: &mut Denoise,
                        slot: usize,
                        label: &'static str,
                        entries: &[(u32, wgpu::BindingResource)],
                        grid: [u32; 2],
                        encoder: &mut wgpu::CommandEncoder| {
            let bind = denoise.groups[slot].get(device, label, entries);
            let receiver_group =
                denoise
                    .receivers
                    .get(device, "world reflection receivers", &receivers);
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(label),
                timestamp_writes: timing.and_then(|t| t.compute_pass("world reflection denoise")),
            });
            pass.set_pipeline(&denoise.pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.set_bind_group(3, receiver_group, &[]);
            pass.dispatch_workgroups(grid[0].div_ceil(8), grid[1].div_ceil(8), 1);
        };
        dispatch(
            &mut self.resolve,
            0,
            "world reflection resolve",
            &[
                (10, view(&t.indirect)),
                (11, view(&t.direction_pdf)),
                (12, view(&t.length)),
                (20, view(&t.resolve)),
                (21, view(&t.resolve_variance)),
                (22, view(&t.reprojection)),
            ],
            t.reduced,
            encoder,
        );
        dispatch(
            &mut self.temporal,
            current,
            "world reflection temporal",
            &[
                (8, sampler()),
                (30, view(&t.resolve)),
                (31, view(&t.temporal[history])),
                (32, view(&t.resolve_variance)),
                (33, view(&t.temporal_variance[history])),
                (34, view(&t.reprojection)),
                (35, view(input.motion)),
                (36, view(&t.depth[history])),
                (40, view(&t.temporal[current])),
                (41, view(&t.temporal_variance[current])),
                (42, view(&t.depth[current])),
            ],
            t.reduced,
            encoder,
        );
        dispatch(
            &mut self.upsample,
            current,
            "world reflection upsample",
            &[
                (8, sampler()),
                (50, view(&t.temporal[current])),
                (51, view(&t.temporal_variance[current])),
                (52, view(&t.output)),
            ],
            t.full,
            encoder,
        );
        self.frame = self.frame.wrapping_add(1).max(1);
    }
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    [crate::shading::layout_tests::mirror!(
        "world_reflections",
        "WorldParams",
        Params,
        [
            inverse_view_projection,
            previous_view_projection,
            eye,
            full,
            reduced,
            frame,
            downscale,
            traced,
            range,
        ]
    )]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
