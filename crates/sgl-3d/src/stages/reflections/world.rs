//! World-space reflection rays for screen-space misses on moving objects or
//! everything, and their denoiser, as Wicked Engine's RT reflections run them
//! (`Postprocess_RTReflection`, wiRenderer.cpp): trace at half resolution,
//! spatial resolve, temporal accumulation, bilateral upsample; the tracing
//! pixels that need a ray classified first and traced from a list, as
//! FidelityFX SSSR traces its rays (`classify`).
use crate::settings::WorldSpaceReflections;
use crate::shading;
use crate::shading::RayQueryForm;
use crate::view::cached_group::CachedGroup;
use crate::view::frame::HardwareRays;
use crate::view::history::HistoryFrame;
use crate::view::pipelines::LitConstants;
use crate::view::reflection_camera;
use crate::view::trace_paths::{TracePath, TracePaths};
use glam::Mat4;
use std::collections::HashMap;

pub(crate) mod classify;

/// Wicked's default RT reflection downscale.
const DOWNSCALE: u32 = 2;
/// How far a reflection ray looks, in metres: Wicked's default RT
/// reflection range (4323a33 `Postprocess_RTReflection`,
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
    rays: u32,
    traced: f32,
    range: f32,
}
/// Where the listed rays' count lies in the parameters, which the trace
/// reads it from.
const PARAMS_RAYS: wgpu::BufferAddress = std::mem::offset_of!(Params, rays) as u64;

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
    lists: classify::Lists,
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
        let storage = wgpu::TextureUsages::STORAGE_BINDING;
        let hdr = crate::shading::gbuffer::COLOR;
        let float = wgpu::TextureFormat::R32Float;
        let half = |label, format, usage| texture(device, label, reduced, format, usage);
        Self {
            full,
            reduced,
            // Read back by the tests, as the ray-traced shadow stage's.
            indirect: half(
                "world reflection radiance",
                hdr,
                storage | wgpu::TextureUsages::COPY_SRC,
            ),
            direction_pdf: half("world reflection direction and pdf", hdr, storage),
            length: half("world reflection ray length", float, storage),
            resolve: half("world reflection resolve", hdr, storage),
            resolve_variance: half("world reflection resolve variance", hdr, storage),
            reprojection: half("world reflection reprojection depth", float, storage),
            temporal: [0, 1].map(|_| half("world reflection temporal", hdr, storage)),
            temporal_variance: [0, 1]
                .map(|_| half("world reflection temporal variance", hdr, storage)),
            depth: [0, 1].map(|_| half("world reflection depth history", float, storage)),
            output: texture(device, "world reflections", full, hdr, storage),
            lists: classify::Lists::new(device, reduced),
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
    /// The trace's programs for each path its rays take, with its receivers
    /// at group 3.
    paths: TracePaths,
    /// The trace's pipelines for each set of lit constants, whether the
    /// rays reach static geometry (`world_reach_all`) and path, each
    /// created when a frame first needs it, as the geometry pipelines
    /// specialise on the scene's rectangle lights and decals.
    trace: HashMap<((LitConstants, bool), Option<RayQueryForm>), wgpu::ComputePipeline>,
    classify: classify::Classify,
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
}

pub(crate) static COMMON: shading::Module = shading::Module {
    name: "world_reflections_common",
    source: include_str!("world/world_reflections_common.wgsl"),
    deps: &[&shading::GBUFFER, &shading::DEPTH, &shading::HASH],
};
/// The trace, a compute pass over the classification's ray list: the lit
/// layout at group 0 (the camera's ray-hit group, with the installed
/// probes), the scene at group 1 and its receivers, list and targets at
/// group 3, with the TLAS on the hardware path. Its pipelines compose the
/// ray function set of the path the frame's rays take (`TracePaths`). The
/// denoiser's receivers are at group 3 too, its own bindings at group 0.
pub(crate) static TRACE: shading::Module = shading::Module {
    name: "world_reflections",
    source: include_str!("world/world_reflections.wgsl"),
    deps: &[
        &shading::BIND_LIT,
        &shading::SURFACE_RAY,
        &shading::SHADOW_MASK_NONE,
        &COMMON,
    ],
};
/// The entry point the trace's pipelines are created with.
pub(crate) const WORLD_TRACE_ENTRY: &str = "world_trace";
pub(crate) static DENOISE: shading::Module = shading::Module {
    name: "world_reflections_denoise",
    source: include_str!("world/world_reflections_denoise.wgsl"),
    deps: &[&COMMON, &super::TEMPORAL_REPROJECTION, &shading::LUMINANCE],
};
/// The entry points the denoiser's pipelines are created with.
pub(crate) const WORLD_RESOLVE_ENTRY: &str = "world_resolve";
pub(crate) const WORLD_TEMPORAL_ENTRY: &str = "world_temporal";
pub(crate) const WORLD_UPSAMPLE_ENTRY: &str = "world_upsample";

impl WorldReflections {
    /// `lit` and `scene` are group 0's lit layout and group 1's.
    pub fn new(
        device: &wgpu::Device,
        lit: &shading::bind::LitLayout,
        scene: &wgpu::BindGroupLayout,
        size: [u32; 2],
    ) -> Self {
        let denoise_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world-space reflection denoise"),
            source: wgpu::ShaderSource::Wgsl(shading::compose(&[&DENOISE]).into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let write = |format| wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        };
        let sampled = |sample_type| wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let unfilterable = sampled(wgpu::TextureSampleType::Float { filterable: false });
        let receivers = [
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
            entry(6, sampled(wgpu::TextureSampleType::Uint)),
            entry(9, sampled(wgpu::TextureSampleType::Uint)),
            entry(10, write(crate::shading::gbuffer::COLOR)),
            entry(11, write(crate::shading::gbuffer::COLOR)),
            entry(12, write(wgpu::TextureFormat::R32Float)),
        ];
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
            paths: TracePaths::new(
                "world-space reflection rays",
                &TRACE,
                &receivers,
                lit,
                scene,
            ),
            trace: HashMap::new(),
            classify: classify::Classify::new(device),
            resolve: compute(WORLD_RESOLVE_ENTRY),
            temporal: compute(WORLD_TEMPORAL_ENTRY),
            upsample: compute(WORLD_UPSAMPLE_ENTRY),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            params: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("world-space reflection parameters"),
                    size: std::mem::size_of::<Params>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            targets: Targets::new(device, size),
            frame: 0,
            previous_scene_frame: None,
        }
    }

    /// Premultiplied radiance of what each receiver's traced lobe reflects
    /// (rgb), and the share of its rays that hit it (a).
    pub fn output(&self) -> &wgpu::TextureView {
        &self.targets.output
    }

    /// Makes the trace's pipeline compiled with `lit` for the path
    /// `hardware` takes, its rays reaching static geometry where `all`;
    /// returns the path it took (`TracePaths::pipeline`).
    fn trace(
        &mut self,
        device: &wgpu::Device,
        lit: LitConstants,
        hardware: Option<HardwareRays<'_>>,
        all: bool,
    ) -> Option<RayQueryForm> {
        self.paths.pipeline(
            device,
            hardware,
            (&mut self.trace, (lit, all)),
            |TracePath { shader, layout, .. }| {
                let constants = [
                    lit.constants().as_slice(),
                    &[("world_reach_all", f64::from(u8::from(all)))],
                ]
                .concat();
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("world-space reflection rays"),
                    layout: Some(layout),
                    module: shader,
                    entry_point: Some(WORLD_TRACE_ENTRY),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &constants,
                        ..Default::default()
                    },
                    cache: None,
                })
            },
        )
    }

    /// `groups` are the camera's ray-hit lit group 0 (with the installed
    /// probes) and the scene's group 1; `lit_constants`, the scene's
    /// (`LitConstants::of`); `hardware`, the hardware path the frame's rays
    /// take, if any; `reach`, what they reach (`Moving` or `All`).
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        [lit, scene]: [&wgpu::BindGroup; 2],
        lit_constants: LitConstants,
        hardware: Option<HardwareRays<'_>>,
        reach: WorldSpaceReflections,
        history: HistoryFrame,
        size: [u32; 2],
        input: Inputs<'_>,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let all = reach == WorldSpaceReflections::All;
        let form = self.trace(device, lit_constants, hardware, all);
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
        }
        self.previous_scene_frame = Some(history.frames);
        // The renderer's camera history is the previous camera, as it
        // rasterized; a new history reprojects through this frame's.
        let previous = match history.previous_camera {
            Some(camera) if self.frame > 0 => camera.jittered_view_projection(),
            _ => {
                Mat4::from_cols_array_2d(&input.camera.proj)
                    * Mat4::from_cols_array_2d(&input.camera.view)
            }
        };
        let t = &self.targets;
        let [w, h] = t.full.map(|v| v as f32);
        let [rw, rh] = t.reduced.map(|v| v as f32);
        let eye = input.camera.camera_position;
        let near = input.camera.proj[3][2];
        crate::counters::write_buffer(
            queue,
            &self.params,
            0,
            bytemuck::bytes_of(&Params {
                inverse_view_projection: input.camera.inverse_view_proj,
                previous_view_projection: previous.to_cols_array_2d(),
                eye: [eye[0], eye[1], eye[2], near],
                full: [w, h, 1. / w, 1. / h],
                reduced: [rw, rh, 1. / rw, 1. / rh],
                frame: self.frame,
                rays: 0,
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
        self.classify.encode(
            device,
            encoder,
            t.reduced,
            classify::Bindings {
                targets: [&t.indirect, &t.direction_pdf, &t.length],
                lists: &t.lists,
                depth: input.depth,
                material: input.material,
                f0: input.f0,
                params: &self.params,
                screen_space: input.screen_space,
                surface_depth: input.surface_depth,
            },
            timing,
        );
        encoder.copy_buffer_to_buffer(
            &t.lists.count,
            classify::RAYS_OFFSET,
            &self.params,
            PARAMS_RAYS,
            4,
        );
        let trace_group = self.paths.group(
            device,
            hardware,
            &[
                receivers.as_slice(),
                &[
                    (6, view(input.source_id)),
                    (9, view(&t.lists.rays)),
                    (10, view(&t.indirect)),
                    (11, view(&t.direction_pdf)),
                    (12, view(&t.length)),
                ],
            ]
            .concat(),
        );
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("world-space reflection rays"),
                timestamp_writes: timing.and_then(|t| t.compute_pass("world reflection rays")),
            });
            pass.set_pipeline(&self.trace[&((lit_constants, all), form)]);
            pass.set_bind_group(0, lit, &[]);
            pass.set_bind_group(1, scene, &[]);
            pass.set_bind_group(3, trace_group, &[]);
            pass.dispatch_workgroups_indirect(&t.lists.count, classify::GROUPS_OFFSET);
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

#[cfg(all(test, not(target_arch = "wasm32")))]
impl WorldReflections {
    /// The trace's radiance target, as the last frame left it, with the
    /// share of each tracing pixel's rays that hit in alpha, and the
    /// tracing grid's size.
    pub(crate) fn test_radiance(&self) -> (&wgpu::TextureView, [u32; 2]) {
        (&self.targets.indirect, self.targets.reduced)
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
            rays,
            traced,
            range,
        ]
    )]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
