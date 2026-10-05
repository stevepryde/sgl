//! Ray-traced shadows (the architecture's Ray-traced shadows): while
//! hardware ray tracing is in effect and `Settings::ray_traced_shadows` is
//! on, the camera's opaque surfaces take the shadows of the slots' lights
//! from rays through the scene instead of from the maps, as Wicked Engine
//! traces them (2ff1d9e `Postprocess_RTShadow`, wiRenderer.cpp 15498–15880,
//! its resources at 15440–15497): a trace at half the render size, a
//! temporal blend and an upsample into the shadow mask, which the opaque
//! stage's lighting pass reads with the slot table at its group 3. Slot 0
//! is the directional light with the frame's cascades, slots 1 to 15 the
//! casting local lights the local-light atlas places, in its ranking
//! (`slots`). Wicked's shadow denoiser, which it runs on its first four
//! slots, is not run: every slot takes the temporal blend.
//!
//! Reads: the G-buffer's depth, normals, F0 and motion, the camera's lit
//! group 0 (its frame's directional lights and the scene's lights), the
//! scene's group 1 and TLAS, and the frame's projection.
//! Writes: its own tracing targets and history, the shadow mask and the
//! slot table, which it lends to the opaque stage's lighting pass.
//! Honours: the effective ray-traced shadows, on the frames whose rays
//! trace in hardware.
//! Timing groups: `ray-traced shadow rays`, `ray-traced shadow tile
//! classification`, `ray-traced shadow filter` (three passes),
//! `ray-traced shadow temporal`, `ray-traced shadow upsample`.
pub(crate) mod denoise;
pub(crate) mod slots;

use crate::shading::RayQueryForm;
use crate::shading::{self, shadow_mask};
use crate::view::cached_group::CachedGroup;
use crate::view::frame::{FrameContext, ShadowMask};
use crate::view::trace_paths::{TracePath, TracePaths};
use std::collections::HashMap;

/// Wicked's `DOWNSAMPLE`: the trace runs at half the render size.
const DOWNSAMPLE: u32 = 2;

/// `TracedParams` in traced_common.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    inverse_view_projection: [[f32; 4]; 4],
    view: [[f32; 4]; 4],
    inverse_projection: [[f32; 4]; 4],
    previous_view: [[f32; 4]; 4],
    full: [f32; 4],
    reduced: [f32; 4],
    eye: [f32; 4],
    frame: u32,
    seed: u32,
    padding: [u32; 2],
}

static COMMON: shading::Module = shading::Module {
    name: "traced_shadows_common",
    source: include_str!("traced/traced_common.wgsl"),
    deps: &[&shading::SHADOW_MASK_SLOTS],
};
/// The trace: the camera's lit group 0, the scene at group 1 and the
/// stage's own group 3 with the TLAS, composed with the hardware form's
/// query module (`TracePaths`).
pub(crate) static TRACE: shading::Module = shading::Module {
    name: "traced_shadows_trace",
    source: include_str!("traced/traced_trace.wgsl"),
    deps: &[
        &shading::BIND_LIT,
        &shading::GBUFFER,
        &shading::LIGHT_REACH,
        &shading::LIGHT_SURFACE,
        &shading::HASH,
        &shading::SHADOW_MASK_SLOT_KEY,
        &shading::SCENE_RAYS_PREDICATE,
        &COMMON,
    ],
};
/// The entry points the stage's pipelines are created with.
pub(crate) const TRACE_ENTRY: &str = "traced_shadow_rays";
pub(crate) const TEMPORAL_ENTRY: &str = "traced_shadow_temporal";
pub(crate) const UPSAMPLE_ENTRY: &str = "traced_shadow_upsample";
pub(crate) static TEMPORAL: shading::Module = shading::Module {
    name: "traced_shadows_temporal",
    source: include_str!("traced/traced_temporal.wgsl"),
    deps: &[&shading::SHADOW_MASK_SLOTS, &COMMON],
};
pub(crate) static UPSAMPLE: shading::Module = shading::Module {
    name: "traced_shadows_upsample",
    source: include_str!("traced/traced_upsample.wgsl"),
    deps: &[&shading::SHADOW_MASK_SLOTS, &COMMON],
};

fn texture(
    device: &wgpu::Device,
    label: &str,
    [width, height]: [u32; 2],
    layers: u32,
    format: wgpu::TextureFormat,
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: (layers > 1).then_some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    })
}

/// The stage's targets at one render size: the trace's words and linear
/// depth at the tracing resolution, the temporal blend's pair, and the
/// mask at the render size.
struct Targets {
    full: [u32; 2],
    reduced: [u32; 2],
    raw: wgpu::TextureView,
    /// Each frame's linear depth at the tracing resolution, the half the
    /// frame writes and the other, last frame's, which its reprojection
    /// reads.
    depth: [wgpu::TextureView; 2],
    temporal: [wgpu::TextureView; 2],
    mask: wgpu::TextureView,
    /// The denoiser's.
    denoise: denoise::Targets,
}

impl Targets {
    fn new(device: &wgpu::Device, full: [u32; 2]) -> Self {
        let reduced = full.map(|side| side.div_ceil(DOWNSAMPLE).max(1));
        let words = wgpu::TextureFormat::Rgba32Uint;
        let half = |label, format| texture(device, label, reduced, 1, format);
        Self {
            full,
            reduced,
            raw: half("ray-traced shadow rays", words),
            depth: [0, 1].map(|_| half("ray-traced shadow depth", wgpu::TextureFormat::R32Float)),
            temporal: [0, 1].map(|_| half("ray-traced shadow temporal", words)),
            mask: texture(
                device,
                "ray-traced shadow mask",
                full,
                shadow_mask::LAYERS,
                shadow_mask::FORMAT,
            ),
            denoise: denoise::Targets::new(device, reduced),
        }
    }
}

/// A compute pass with its group 0 for each half of the history pair.
struct Pass {
    pipeline: wgpu::ComputePipeline,
    groups: [CachedGroup; 2],
}

impl Pass {
    /// The pass `entry` of `module`, its pipeline constants `constants`.
    fn new(
        device: &wgpu::Device,
        module: &'static shading::Module,
        entry: &str,
        constants: &[(&str, f64)],
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(entry),
            source: wgpu::ShaderSource::Wgsl(shading::compose(&[module]).into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: None,
            module: &shader,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants,
                ..Default::default()
            },
            cache: None,
        });
        let group = || CachedGroup::new(pipeline.get_bind_group_layout(0));
        Self {
            groups: [group(), group()],
            pipeline,
        }
    }
}

/// The ray-traced shadow stage. Owns its pipelines, targets (reallocated
/// when the render size changes), slot table and history, which continues
/// across consecutive frames it runs in.
pub(crate) struct TracedShadows {
    /// The trace's programs, with its group 3.
    paths: TracePaths,
    /// The trace's pipeline for each form a frame's rays took.
    trace: HashMap<((), Option<RayQueryForm>), wgpu::ComputePipeline>,
    temporal: Pass,
    upsample: Pass,
    /// AMD's shadow denoiser over the first four slots.
    denoiser: denoise::Denoiser,
    params: wgpu::Buffer,
    /// The slot table, which the lighting pass reads too.
    slot_table: wgpu::Buffer,
    slots: slots::Slots,
    /// The last frame's slot table, and whether the stage ran in it.
    table: shadow_mask::ShadowMaskSlots,
    ran: bool,
    targets: Option<Targets>,
    /// Frames since the history last restarted; 0 restarts it.
    frame: u32,
    /// Turns the rays' draws on the lights each frame the stage runs.
    seed: u32,
    /// The camera history's frame count of the last frame the stage ran.
    previous_frame: Option<u32>,
}

impl TracedShadows {
    /// `lit` and `scene` are group 0's lit layout and group 1's. Its
    /// targets are made in the first frame it runs.
    pub fn new(
        device: &wgpu::Device,
        lit: &wgpu::BindGroupLayout,
        scene: &wgpu::BindGroupLayout,
    ) -> Self {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let sampled = |sample_type| wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let uniform = wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let storage = |format| wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        };
        let unfilterable = wgpu::TextureSampleType::Float { filterable: false };
        let entries = [
            entry(0, sampled(wgpu::TextureSampleType::Depth)),
            entry(1, sampled(unfilterable)),
            entry(2, sampled(unfilterable)),
            entry(3, uniform),
            entry(4, uniform),
            entry(5, storage(wgpu::TextureFormat::Rgba32Uint)),
            entry(6, storage(wgpu::TextureFormat::R32Float)),
            entry(7, storage(wgpu::TextureFormat::Rgba32Uint)),
            entry(8, storage(denoise::NORMAL_FORMAT)),
        ];
        let uniform_buffer = |label, size| {
            crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some(label),
                    size,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            )
        };
        Self {
            paths: TracePaths::new("ray-traced shadow rays", &TRACE, &entries, [lit, scene]),
            trace: HashMap::new(),
            temporal: Pass::new(device, &TEMPORAL, TEMPORAL_ENTRY, &[]),
            upsample: Pass::new(device, &UPSAMPLE, UPSAMPLE_ENTRY, &[]),
            denoiser: denoise::Denoiser::new(device),
            params: uniform_buffer(
                "ray-traced shadow parameters",
                std::mem::size_of::<Params>() as u64,
            ),
            slot_table: uniform_buffer(
                "ray-traced shadow slots",
                std::mem::size_of::<shadow_mask::ShadowMaskSlots>() as u64,
            ),
            slots: slots::Slots::default(),
            table: shadow_mask::ShadowMaskSlots::new(
                [shadow_mask::SHADOW_MASK_EMPTY; shadow_mask::RT_SHADOW_LIGHTS],
                0,
            ),
            ran: false,
            targets: None,
            frame: 0,
            seed: 0,
            previous_frame: None,
        }
    }

    /// Traces, blends and upsamples the shadows of the frame's slots: slot
    /// 0 the directional light with the frame's cascades, the others the
    /// local lights the atlas placed, best first (`lights`). Returns the
    /// mask and slot table for the lighting pass; none where the stage does
    /// not run (`Effective::ray_traced_shadows`), a frame whose rays do not
    /// trace in hardware or whose slots hold no light.
    pub fn encode<'s>(
        &'s mut self,
        ctx: &mut FrameContext<'_>,
        lights: &slots::SlotLights,
    ) -> Option<ShadowMask<'s>> {
        self.ran = false;
        let hardware = ctx
            .hardware_rays
            .filter(|_| ctx.effective.ray_traced_shadows)?;
        let size = ctx.sizes.render;
        let resized = self.targets.as_ref().is_none_or(|t| t.full != size);
        if resized {
            self.targets = Some(Targets::new(ctx.device, size));
        }
        // History continues across consecutive valid frames the stage runs
        // in, as the world-space reflection denoiser's does.
        let history = ctx.history;
        let continuous = history.valid
            && self
                .previous_frame
                .is_some_and(|previous| history.frames == previous.wrapping_add(1));
        if resized || !continuous {
            self.frame = 0;
        }
        self.previous_frame = Some(history.frames);
        let table = self.slots.assign(lights.directional, &lights.local);
        self.table = table;
        crate::counters::write_buffer(ctx.queue, &self.slot_table, 0, bytemuck::bytes_of(&table));
        let targets = self.targets.as_ref().unwrap();
        let [width, height] = targets.full.map(|side| side as f32);
        let [reduced_width, reduced_height] = targets.reduced.map(|side| side as f32);
        let view = &ctx.values.view;
        // The projection as it rasterized, jitter and all, and the last
        // submitted frame's view, this frame's after a restart.
        let camera = history.camera;
        let previous_view = history
            .previous_camera
            .map_or(camera.view, |previous| previous.view);
        let eye = ctx.input.camera.eye;
        crate::counters::write_buffer(
            ctx.queue,
            &self.params,
            0,
            bytemuck::bytes_of(&Params {
                inverse_view_projection: view.inverse_view_projection,
                view: view.view,
                inverse_projection: camera.jittered_projection().inverse().to_cols_array_2d(),
                previous_view: previous_view.to_cols_array_2d(),
                full: [width, height, 1. / width, 1. / height],
                reduced: [
                    reduced_width,
                    reduced_height,
                    1. / reduced_width,
                    1. / reduced_height,
                ],
                eye: [eye.x, eye.y, eye.z, 1.],
                frame: self.frame,
                seed: self.seed,
                padding: [0; 2],
            }),
        );
        self.seed = self.seed.wrapping_add(1);
        let current = (self.frame % 2) as usize;
        let previous = 1 - current;
        let shared = ctx.targets;
        let resource = wgpu::BindingResource::TextureView;
        // The trace's pipeline for the device's form, made through
        // `TracePaths::pipeline`, so that a candidate program that fails
        // falls the device back to the baseline before the group is made.
        let device = ctx.device;
        let form = self.paths.pipeline(
            device,
            Some(hardware),
            (&mut self.trace, ()),
            |TracePath { shader, layout, .. }| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("ray-traced shadow rays"),
                    layout: Some(layout),
                    module: shader,
                    entry_point: Some(TRACE_ENTRY),
                    compilation_options: Default::default(),
                    cache: None,
                })
            },
        );
        let trace_group = self.paths.group(
            ctx.device,
            Some(hardware),
            &[
                (0, resource(&shared.depth)),
                (1, resource(&shared.normal)),
                (2, resource(&shared.f0)),
                (3, self.params.as_entire_binding()),
                (4, self.slot_table.as_entire_binding()),
                (5, resource(&targets.raw)),
                (6, resource(&targets.depth[current])),
                (7, resource(&targets.denoise.tiles)),
                (8, resource(&targets.denoise.normal)),
            ],
        );
        {
            let mut pass = ctx
                .encoder
                .begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("ray-traced shadow rays"),
                    timestamp_writes: ctx
                        .timing
                        .and_then(|t| t.compute_pass("ray-traced shadow rays")),
                });
            pass.set_pipeline(&self.trace[&((), form)]);
            pass.set_bind_group(0, ctx.bindings.camera_lit(), &[]);
            pass.set_bind_group(1, &ctx.scene.scene_group, &[]);
            pass.set_bind_group(3, trace_group, &[]);
            pass.dispatch_workgroups(
                targets.reduced[0].div_ceil(8),
                targets.reduced[1].div_ceil(4),
                1,
            );
        }
        self.denoiser.encode(
            ctx.device,
            ctx.encoder,
            ctx.timing,
            &targets.denoise,
            denoise::Inputs {
                depth: &shared.depth,
                motion: &shared.motion,
                half_depth: &targets.depth[current],
                previous_depth: &targets.depth[previous],
                params: &self.params,
                slot_table: &self.slot_table,
                reduced: targets.reduced,
                current,
            },
        );
        let temporal_group = self.temporal.groups[current].get(
            ctx.device,
            "ray-traced shadow temporal",
            &[
                (0, resource(&targets.raw)),
                (1, resource(&targets.temporal[previous])),
                (2, resource(&targets.depth[current])),
                (3, resource(&targets.depth[previous])),
                (4, resource(&shared.motion)),
                (5, self.params.as_entire_binding()),
                (6, self.slot_table.as_entire_binding()),
                (7, resource(&targets.temporal[current])),
                (8, targets.denoise.denoised.as_entire_binding()),
            ],
        );
        {
            let mut pass = ctx
                .encoder
                .begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("ray-traced shadow temporal"),
                    timestamp_writes: ctx
                        .timing
                        .and_then(|t| t.compute_pass("ray-traced shadow temporal")),
                });
            pass.set_pipeline(&self.temporal.pipeline);
            pass.set_bind_group(0, temporal_group, &[]);
            pass.dispatch_workgroups(
                targets.reduced[0].div_ceil(8),
                targets.reduced[1].div_ceil(8),
                1,
            );
        }
        let upsample_group = self.upsample.groups[current].get(
            ctx.device,
            "ray-traced shadow upsample",
            &[
                (0, resource(&targets.temporal[current])),
                (1, resource(&targets.depth[current])),
                (2, resource(&shared.depth)),
                (3, self.params.as_entire_binding()),
                (4, resource(&targets.mask)),
                (5, self.slot_table.as_entire_binding()),
            ],
        );
        {
            let mut pass = ctx
                .encoder
                .begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("ray-traced shadow upsample"),
                    timestamp_writes: ctx
                        .timing
                        .and_then(|t| t.compute_pass("ray-traced shadow upsample")),
                });
            pass.set_pipeline(&self.upsample.pipeline);
            pass.set_bind_group(0, upsample_group, &[]);
            pass.dispatch_workgroups(targets.full[0].div_ceil(8), targets.full[1].div_ceil(8), 1);
        }
        self.frame = self.frame.wrapping_add(1).max(1);
        self.ran = true;
        Some(ShadowMask {
            mask: &targets.mask,
            slots: &self.slot_table,
        })
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
impl TracedShadows {
    /// The last frame's shadow mask and slot table, where the stage ran.
    pub fn last(&self) -> Option<(&wgpu::TextureView, shadow_mask::ShadowMaskSlots)> {
        let targets = self.targets.as_ref().filter(|_| self.ran)?;
        Some((&targets.mask, self.table))
    }
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    [crate::shading::layout_tests::mirror!(
        "traced_shadows_temporal",
        "TracedParams",
        Params,
        [
            inverse_view_projection,
            view,
            inverse_projection,
            previous_view,
            full,
            reduced,
            eye,
            frame,
            seed
        ]
    )]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
