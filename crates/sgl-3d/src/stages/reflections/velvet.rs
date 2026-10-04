//! Godot Engine's screen-space reflections (4.7.2-stable, revision
//! `ed1daf0bf001b61586d9930840f2f1394092c079`, MIT, `LICENSE-godot.txt`), as
//! `SSEffects::screen_space_reflection` (servers/rendering/renderer_rd/effects/
//! ss_effects.cpp) runs them: one mirror ray per pixel through a hierarchical
//! depth buffer, a Gaussian mip chain of the hits, and a resolve that reads
//! each pixel's mip for its roughness and ray length. Nothing is stochastic.
//! SGL3D converts its G-buffer to Godot's inputs first and traces this frame's
//! radiance. TAA's jitter changes those inputs every frame, so SGL3D
//! accumulates the traced hits over frames before the mip chain with Wicked
//! Engine's SSR temporal pass (`godot_reflections_temporal.wgsl`, through the
//! reprojection world-space rays share).
//!
//! SGL3D's orchestration of those passes, in upstream's order: this file
//! encodes them, `targets.rs` allocates what they read and write.
mod targets;

use crate::shading::Module;
use crate::view::cached_group::CachedGroup;
use crate::view::reflection_camera;
use glam::{Mat4, Vec4};
use targets::Targets;

static INPUTS: Module = Module {
    name: "godot_reflections_inputs",
    source: include_str!("velvet/godot_reflections_inputs.wgsl"),
    deps: &[&crate::shading::GBUFFER],
};
static DOWNSAMPLE: Module = Module {
    name: "godot_reflections_downsample",
    source: include_str!("velvet/godot_reflections_downsample.wgsl"),
    deps: &[],
};
static HIZ: Module = Module {
    name: "godot_reflections_hiz",
    source: include_str!("velvet/godot_reflections_hiz.wgsl"),
    deps: &[],
};
static TRACE: Module = Module {
    name: "godot_reflections_trace",
    source: include_str!("velvet/godot_reflections_trace.wgsl"),
    deps: &[],
};
static TEMPORAL: Module = Module {
    name: "godot_reflections_temporal",
    source: include_str!("velvet/godot_reflections_temporal.wgsl"),
    deps: &[&super::TEMPORAL_REPROJECTION],
};
static FILTER: Module = Module {
    name: "godot_reflections_filter",
    source: include_str!("velvet/godot_reflections_filter.wgsl"),
    deps: &[],
};
static RESOLVE: Module = Module {
    name: "godot_reflections_resolve",
    source: include_str!("velvet/godot_reflections_resolve.wgsl"),
    deps: &[],
};
/// Velvet's programs.
#[cfg(test)]
pub(crate) static PROGRAMS: [&Module; 7] = [
    &INPUTS,
    &DOWNSAMPLE,
    &HIZ,
    &TRACE,
    &TEMPORAL,
    &FILTER,
    &RESOLVE,
];

/// Godot's `Environment` defaults: `ssr_max_steps`, `ssr_fade_in`,
/// `ssr_fade_out` and `ssr_depth_tolerance` (metres).
const MAX_STEPS: i32 = 64;
const FADE_IN: f32 = 0.15;
const FADE_OUT: f32 = 2.0;
const DEPTH_TOLERANCE: f32 = 0.5;
/// Perceptual roughness at which Godot's trace stops (`roughness >= 0.7`),
/// and the width of the fade below it in its forward pass (0.6 to 0.7).
pub(crate) const ROUGHNESS_CUTOFF: f32 = 0.7;
pub(crate) const ROUGHNESS_FADE: f32 = 0.1;

pub(crate) struct Inputs<'a> {
    /// The surface depth, with the opaque depth and the receiver layer that
    /// select the surface's lobe (the Surface contract).
    pub depth: &'a wgpu::TextureView,
    pub opaque_depth: &'a wgpu::TextureView,
    pub receivers: &'a wgpu::TextureView,
    pub normal: &'a wgpu::TextureView,
    pub material: &'a wgpu::TextureView,
    pub f0: &'a wgpu::TextureView,
    /// This frame's radiance: what the rays reflect.
    pub radiance: &'a wgpu::TextureView,
    /// SGL3D's motion vectors (current minus previous UV) at full resolution.
    pub motion: &'a wgpu::TextureView,
    pub camera: reflection_camera::Camera,
    /// The camera history the accumulation continues across.
    pub frame: crate::view::history::HistoryFrame,
}

/// `ScreenSpaceReflectionSceneData` for one view.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneData {
    projection: [[f32; 4]; 4],
    inv_projection: [[f32; 4]; 4],
    reprojection: [[f32; 4]; 4],
    eye_offset: [f32; 4],
}

/// `ScreenSpaceReflectionPushConstant`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TraceParams {
    screen_size: [i32; 2],
    mipmaps: i32,
    num_steps: i32,
    distance_fade: f32,
    curve_fade_in: f32,
    depth_tolerance: f32,
    orthogonal: u32,
}

/// `TemporalParams` in godot_reflections_temporal.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TemporalParams {
    inverse_view_projection: [[f32; 4]; 4],
    previous_view_projection: [[f32; 4]; 4],
    size: [f32; 4],
    near: f32,
    flags: u32,
    padding: [u32; 2],
}
/// `TemporalParams::flags`: history continues; clear, it resets.
const TEMPORAL_CONTINUES: u32 = 1;

/// `ScreenSpaceReflectionFilterPushConstant`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FilterParams {
    screen_size: [i32; 2],
    mip_level: u32,
    pad: i32,
}

/// The groups of Velvet's passes over one allocation of its targets: one per
/// pass, mip of the two chains and history half of the temporal pass.
struct Groups {
    inputs: CachedGroup,
    downsample: CachedGroup,
    /// By mip, from 1.
    hiz: Vec<CachedGroup>,
    trace: CachedGroup,
    /// By the half of the history pair the frame writes.
    temporal: [CachedGroup; 2],
    /// By mip, from 1.
    filter: Vec<CachedGroup>,
    resolve: CachedGroup,
}

/// Velvet: Godot's screen-space reflections with Wicked's temporal
/// accumulation. Owns its pipelines, its targets (reallocated when the
/// render size or half resolution changes) with their groups, and its
/// history, which continues across consecutive valid frames.
pub(crate) struct Velvet {
    inputs: wgpu::ComputePipeline,
    downsample: wgpu::ComputePipeline,
    hiz: wgpu::ComputePipeline,
    trace: wgpu::ComputePipeline,
    temporal: wgpu::ComputePipeline,
    filter: wgpu::ComputePipeline,
    resolve_half: wgpu::ComputePipeline,
    resolve_full: wgpu::ComputePipeline,
    /// Godot's `CANVAS_ITEM_TEXTURE_FILTER_LINEAR_WITH_MIPMAPS` without repeat.
    linear: wgpu::Sampler,
    view: wgpu::Buffer,
    scene_data: wgpu::Buffer,
    trace_params: wgpu::Buffer,
    temporal_params: wgpu::Buffer,
    targets: Option<Targets>,
    groups: Option<Groups>,
    /// Frames since history last reset; 0 resets it.
    frame: u32,
    /// Scene frame last encoded, matching the TAA/SSR continuity rule.
    previous_scene_frame: Option<u32>,
}

impl Velvet {
    pub fn new(device: &wgpu::Device) -> Self {
        let pipeline = |label, program: &'static Module, entry_point| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[program]).into()),
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: None,
                module: &module,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let uniform = |label, size: usize| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        Self {
            inputs: pipeline("Godot SSR inputs", &INPUTS, "main"),
            downsample: pipeline("Godot SSR downsample", &DOWNSAMPLE, "main"),
            hiz: pipeline("Godot SSR hi-z", &HIZ, "main"),
            trace: pipeline("Godot SSR main", &TRACE, "main"),
            temporal: pipeline("Godot SSR temporal", &TEMPORAL, "main"),
            filter: pipeline("Godot SSR roughness filter", &FILTER, "main"),
            resolve_half: pipeline("Godot SSR resolve", &RESOLVE, "resolve_half"),
            resolve_full: pipeline("Godot SSR resolve at full size", &RESOLVE, "resolve_full"),
            linear: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("Godot SSR linear"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            }),
            view: uniform("Godot SSR view", std::mem::size_of::<[[f32; 4]; 4]>()),
            scene_data: uniform("Godot SSR scene data", std::mem::size_of::<SceneData>()),
            trace_params: uniform("Godot SSR parameters", std::mem::size_of::<TraceParams>()),
            temporal_params: uniform(
                "Godot SSR temporal parameters",
                std::mem::size_of::<TemporalParams>(),
            ),
            targets: None,
            groups: None,
            frame: 0,
            previous_scene_frame: None,
        }
    }

    /// Empty groups for targets with `mipmaps` mips, at half resolution with
    /// `half`.
    fn groups(&self, mipmaps: u32, half: bool) -> Groups {
        let group =
            |pipeline: &wgpu::ComputePipeline| CachedGroup::new(pipeline.get_bind_group_layout(0));
        let chain = |pipeline| (1..mipmaps).map(|_| group(pipeline)).collect();
        Groups {
            inputs: group(&self.inputs),
            downsample: group(&self.downsample),
            hiz: chain(&self.hiz),
            trace: group(&self.trace),
            temporal: [group(&self.temporal), group(&self.temporal)],
            filter: chain(&self.filter),
            resolve: group(if half {
                &self.resolve_half
            } else {
                &self.resolve_full
            }),
        }
    }

    /// Traces `input` at `size`, at half resolution with `half`, and returns
    /// each receiver's reflected radiance premultiplied by the confidence in
    /// it (rgb) and that confidence (a), at `size`.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        size: [u32; 2],
        half: bool,
        input: Inputs<'_>,
        timing: Option<&crate::timing::GpuTiming>,
    ) -> &wgpu::TextureView {
        let resized = self
            .targets
            .as_ref()
            .is_none_or(|t| t.full != size || t.half != half);
        if resized {
            let targets = Targets::new(device, size, half);
            self.groups = Some(self.groups(targets.mipmaps, half));
            self.targets = Some(targets);
        }
        // As with world rays, history continues only across consecutive
        // valid scene frames.
        let continuous = input.frame.valid
            && self
                .previous_scene_frame
                .is_some_and(|previous| input.frame.frames == previous.wrapping_add(1));
        if resized || !continuous {
            self.frame = 0;
        }
        self.previous_scene_frame = Some(input.frame.frames);
        // The renderer's camera history is the previous camera, as it
        // rasterized; a new history reprojects through this frame's.
        let previous = match input.frame.previous_camera {
            Some(camera) if self.frame > 0 => camera.jittered_view_projection(),
            _ => {
                Mat4::from_cols_array_2d(&input.camera.proj)
                    * Mat4::from_cols_array_2d(&input.camera.view)
            }
        };
        let current = (self.frame % 2) as usize;
        let continues = self.frame > 0;
        self.frame = self.frame.wrapping_add(1).max(1);
        let t = self.targets.as_ref().unwrap();
        let [width, height] = t.size.map(|v| v as f32);
        queue.write_buffer(
            &self.temporal_params,
            0,
            bytemuck::bytes_of(&TemporalParams {
                inverse_view_projection: input.camera.inverse_view_proj,
                previous_view_projection: previous.to_cols_array_2d(),
                size: [width, height, 1. / width, 1. / height],
                near: input.camera.proj[3][2],
                flags: if continues { TEMPORAL_CONTINUES } else { 0 },
                padding: [0; 2],
            }),
        );
        // Godot's projection with its depth correction flips y into
        // Vulkan's downward NDC, so uv = ndc * 0.5 + 0.5 in its shaders.
        // SGL3D's is already reversed-Z in [0, 1] with y up.
        let flip = Mat4::from_diagonal(Vec4::new(1., -1., 1., 1.));
        let projection = flip * Mat4::from_cols_array_2d(&input.camera.proj);
        queue.write_buffer(&self.view, 0, bytemuck::cast_slice(&input.camera.view));
        queue.write_buffer(
            &self.scene_data,
            0,
            bytemuck::bytes_of(&SceneData {
                projection: projection.to_cols_array_2d(),
                inv_projection: projection.inverse().to_cols_array_2d(),
                // The rays read this frame's radiance.
                reprojection: Mat4::IDENTITY.to_cols_array_2d(),
                eye_offset: [0.; 4],
            }),
        );
        queue.write_buffer(
            &self.trace_params,
            0,
            bytemuck::bytes_of(&TraceParams {
                screen_size: t.size.map(|v| v as i32),
                mipmaps: t.mipmaps as i32,
                num_steps: MAX_STEPS,
                distance_fade: FADE_OUT,
                curve_fade_in: FADE_IN,
                depth_tolerance: DEPTH_TOLERANCE,
                orthogonal: 0,
            }),
        );
        let groups = self.groups.as_mut().unwrap();
        let view = wgpu::BindingResource::TextureView;
        let dispatch = |encoder: &mut wgpu::CommandEncoder,
                        pipeline: &wgpu::ComputePipeline,
                        group: &mut CachedGroup,
                        label: &'static str,
                        entries: &[(u32, wgpu::BindingResource)],
                        grid: [u32; 2]| {
            let group = group.get(device, label, entries);
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(label),
                timestamp_writes: timing.and_then(|t| t.compute_pass(label)),
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(grid[0].div_ceil(8), grid[1].div_ceil(8), 1);
        };
        let mip_size = |m: u32| t.size.map(|v| (v >> m).max(1));
        // SGL3D: Godot's inputs at full resolution. At full size the depth is
        // written straight into the hierarchy (Godot's "SSR Copy Depth").
        let full_depth = t.depth.as_ref().unwrap_or(&t.hiz_mips[0]);
        dispatch(
            encoder,
            &self.inputs,
            &mut groups.inputs,
            "Godot SSR inputs",
            &[
                (0, view(input.normal)),
                (1, view(input.material)),
                (2, view(input.f0)),
                (3, view(input.depth)),
                (4, self.view.as_entire_binding()),
                (5, view(&t.normal_roughness)),
                (6, view(full_depth)),
                (7, view(input.opaque_depth)),
                (8, view(input.receivers)),
            ],
            t.full,
        );
        if let Some(normal_roughness_half) = &t.normal_roughness_half {
            dispatch(
                encoder,
                &self.downsample,
                &mut groups.downsample,
                "Godot SSR downsample",
                &[
                    (0, view(full_depth)),
                    (1, view(&t.normal_roughness)),
                    (2, view(&t.hiz_mips[0])),
                    (3, view(normal_roughness_half)),
                ],
                t.size,
            );
        }
        for (m, group) in (1..t.mipmaps).zip(&mut groups.hiz) {
            dispatch(
                encoder,
                &self.hiz,
                group,
                "Godot SSR hi-z",
                &[
                    (0, view(&t.hiz_mips[m as usize - 1])),
                    (1, view(&t.hiz_mips[m as usize])),
                ],
                mip_size(m),
            );
        }
        let normal_roughness = t
            .normal_roughness_half
            .as_ref()
            .unwrap_or(&t.normal_roughness);
        dispatch(
            encoder,
            &self.trace,
            &mut groups.trace,
            "Godot SSR main",
            &[
                (0, view(input.radiance)),
                (1, view(&t.hiz)),
                (2, view(normal_roughness)),
                (3, view(&t.traced)),
                (4, view(&t.mip_level)),
                (5, self.scene_data.as_entire_binding()),
                (6, self.trace_params.as_entire_binding()),
                (7, wgpu::BindingResource::Sampler(&self.linear)),
                (8, view(&t.reprojection)),
            ],
            t.size,
        );
        dispatch(
            encoder,
            &self.temporal,
            &mut groups.temporal[current],
            "Godot SSR temporal",
            &[
                (0, view(&t.traced)),
                (1, view(&t.history[1 - current])),
                (2, view(&t.reprojection)),
                (3, view(input.motion)),
                (4, view(&t.hiz_mips[0])),
                (5, view(&t.depth_history[1 - current])),
                (6, view(normal_roughness)),
                (7, wgpu::BindingResource::Sampler(&self.linear)),
                (8, self.temporal_params.as_entire_binding()),
                (9, view(&t.ssr_mips[0])),
                (10, view(&t.history[current])),
                (11, view(&t.depth_history[current])),
            ],
            t.size,
        );
        for (m, group) in (1..t.mipmaps).zip(&mut groups.filter) {
            dispatch(
                encoder,
                &self.filter,
                group,
                "Godot SSR roughness filter",
                &[
                    (0, view(&t.ssr_mips[m as usize - 1])),
                    (1, view(&t.ssr_mips[m as usize])),
                    (2, t.filter_params[m as usize].as_entire_binding()),
                    (3, wgpu::BindingResource::Sampler(&self.linear)),
                ],
                mip_size(m),
            );
        }
        if let (Some(depth), Some(normal_roughness_half)) = (&t.depth, &t.normal_roughness_half) {
            dispatch(
                encoder,
                &self.resolve_half,
                &mut groups.resolve,
                "Godot SSR resolve",
                &[
                    (0, view(depth)),
                    (1, view(&t.normal_roughness)),
                    (2, view(&t.hiz_mips[0])),
                    (3, view(normal_roughness_half)),
                    (4, view(&t.ssr)),
                    (5, view(&t.mip_level)),
                    (6, view(&t.output)),
                    (7, wgpu::BindingResource::Sampler(&self.linear)),
                ],
                t.full,
            );
        } else {
            dispatch(
                encoder,
                &self.resolve_full,
                &mut groups.resolve,
                "Godot SSR resolve",
                &[
                    (4, view(&t.ssr)),
                    (5, view(&t.mip_level)),
                    (6, view(&t.output)),
                    (7, wgpu::BindingResource::Sampler(&self.linear)),
                ],
                t.full,
            );
        }
        &t.output
    }
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 4] {
    use crate::shading::layout_tests::mirror;
    [
        mirror!(
            "godot_reflections_trace",
            "SceneData",
            SceneData,
            [projection, inv_projection, reprojection, eye_offset]
        ),
        mirror!(
            "godot_reflections_trace",
            "Params",
            TraceParams,
            [
                screen_size,
                mipmaps,
                num_steps,
                distance_fade,
                curve_fade_in,
                depth_tolerance,
                orthogonal,
            ]
        ),
        mirror!(
            "godot_reflections_temporal",
            "TemporalParams",
            TemporalParams,
            [
                inverse_view_projection,
                previous_view_projection,
                size,
                near,
                flags,
                padding,
            ]
        ),
        mirror!(
            "godot_reflections_filter",
            "Params",
            FilterParams,
            [screen_size, mip_level, pad]
        ),
    ]
}

/// The constants with WGSL twins.
#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 1] {
    [crate::shading::layout_tests::Constant::new(
        "godot_reflections_temporal",
        "TEMPORAL_CONTINUES",
        naga::Literal::U32(TEMPORAL_CONTINUES),
    )]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod hdr_tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod temporal_tests;
