//! Dynamic GI: the scene's dynamic GI volume of probes, kept up every frame
//! by rays through the scene's ray source, as Wicked Engine's DDGI keeps its
//! own (`wiRenderer::DDGI`, df44c3d wiRenderer.cpp 12412–12618): each
//! probe's rays allocated from its irradiance estimator, traced with each
//! hit shaded as the probe-hit receiver kind, then blended into its
//! irradiance and depth maps, the probe moving away from surfaces it nears.
//! The shaders are in `dynamic_gi/`; the probe texture's layout and the
//! sample are `shading::dynamic_gi`'s.
//!
//! Placement: first after prepare, as Wicked updates DDGI in its scene
//! update before any camera pass (wiRenderPath3D.cpp 905–940): its hits take
//! their light's visibility from rays, so it needs nothing of the shadows,
//! no depth and no camera pass, and every view that shades reads it.
//!
//! Reads: the volume's lit group 0 (its lights and decals, the frame's
//! directional lights, environment and hemisphere fill, and its own probes
//! from the last frame for the bounce), the scene's group 1 (the ray
//! source), the camera (its frustum) and its own state.
//! Writes: the probe texture, which lit group 0 lends every view that
//! shades, and its state.
//! Honours: `Settings::dynamic_gi` (the most rays a probe traces), while
//! the scene holds a volume.
//! Timing groups: `dynamic GI allocation`, `dynamic GI rays`, `dynamic GI
//! blend`.
//! History: the probes' state, world-space about each probe's centre. It
//! takes no renderer reset: it is keyed on the scene's identity and the
//! lattice its volume lies on, and starts afresh when either differs or
//! after a submitted frame that did not run it. A scroll keeps it: probes
//! are stored at their lattice coordinate plus the volume's scroll,
//! wrapping, as RTXGI's infinite scrolling volume stores its probes (its
//! probe scroll offsets; practice only), so the probes that stay keep their
//! texels, and a frame that scrolls clears the planes that enter, which
//! start as probes not yet blended. State that must agree with a submitted
//! frame is on the GPU; the key and the placement are committed at
//! `finish_frame`, so an abandoned frame leaves both as they were.
use crate::Scene;
use crate::content::dynamic_gi::DynamicGiVolume;
use crate::scene::dynamic_gi::{InstalledVolume, ProbePlacement};
use crate::shading::RayQueryForm;
use crate::shading::{self, dynamic_gi as layout};
use crate::view::effective::Effective;
use crate::view::frame::{FrameContext, HardwareRays};
use crate::view::pipelines::LitConstants;
use crate::view::trace_paths::{TracePath, TracePaths};
use glam::{I64Vec3, IVec3, Mat3, Mat4, Vec3, Vec4};
#[cfg(feature = "diagnostics")]
pub use inputs::DynamicGiChanges;
use inputs::Inputs;
use std::collections::HashMap;
use volume::{Layouts, Volume, texture};

mod inputs;
#[cfg(feature = "diagnostics")]
pub(crate) mod observe;
mod volume;

/// What the stage's passes share.
pub(crate) static COMMON: shading::Module = shading::Module {
    name: "dynamic_gi_common",
    source: include_str!("dynamic_gi/common.wgsl"),
    deps: &[&shading::DYNAMIC_GI],
};
/// The ray allocation and the trace's indirect dispatch, at the stage's own
/// group 0.
pub(crate) static ALLOCATE: shading::Module = shading::Module {
    name: "dynamic_gi_allocate",
    source: include_str!("dynamic_gi/allocate.wgsl"),
    deps: &[&COMMON],
};
/// The trace: the volume's lit group 0, the scene's group 1 and the stage's
/// own group 3.
pub(crate) static TRACE: shading::Module = shading::Module {
    name: "dynamic_gi_trace",
    source: include_str!("dynamic_gi/trace.wgsl"),
    deps: &[&shading::BIND_LIT, &shading::SURFACE_RAY, &COMMON],
};
/// The irradiance and depth blends, at the stage's own group 0.
pub(crate) static UPDATE: shading::Module = shading::Module {
    name: "dynamic_gi_update",
    source: include_str!("dynamic_gi/update.wgsl"),
    deps: &[&COMMON],
};

/// Workgroups to a row of a two-dimensional dispatch (`DDGI_GROUP_ROW`).
const GROUP_ROW: u32 = 32768;
/// The longest distance a ray's depth counts, in spacings along the
/// volume's longest: Wicked's max_distance (wiScene.cpp 1037).
const MAX_DISTANCE_SPACINGS: f32 = 1.5;

/// `DdgiVolume` in common.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct VolumeUniform {
    origin: [f32; 3],
    max_distance: f32,
    spacing: [f32; 3],
    max_rays: u32,
    probes: [u32; 3],
    probe_count: u32,
    rotation: [[f32; 4]; 3],
    frustum: [[f32; 4]; 6],
    eye: [f32; 3],
    frame: u32,
    rays: u32,
    budget: u32,
    traced: u32,
    padding: u32,
    scroll: [u32; 3],
    padding_scroll: u32,
    scrolled: [i32; 3],
    moving_count: u32,
    changed: u32,
    padding_changed: [u32; 3],
}

/// `DdgiBounds` in common.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct BoundsUniform {
    min: [f32; 3],
    padding_min: f32,
    max: [f32; 3],
    padding_max: f32,
}
/// The most moving instances' bounds a frame takes, the nearest the camera
/// (`DDGI_MOST_MOVING_BOUNDS`): the cap on the allocation's walk over them
/// (AR-12), generously above the moving instances a volume's cells hold in
/// a game's frame.
const MOST_MOVING_BOUNDS: u32 = 256;

/// The allocation's buffer (`DdgiAllocation` in allocate.wgsl): the
/// trace's indirect dispatch and ray count, the blends' dispatch and the
/// count of probes that trace, the starting probes' words and rays, whether
/// the volume paused, the stride the frame's periods take, the rays
/// reserved against the budget, the rays the shortened turns may take and
/// have taken, the blended probes' requests under each stride, and the
/// starting probes' bins. The trace and the blends read the counts in the
/// volume's uniform.
#[repr(C)]
struct Allocation {
    groups: [u32; 3],
    rays: u32,
    blend_groups: [u32; 3],
    traced: u32,
    ramp_bins: u32,
    ramp_room: u32,
    ramp_taken: u32,
    unblended: u32,
    unblended_rays: u32,
    paused: u32,
    stride: u32,
    reserved: u32,
    spare: u32,
    spare_taken: u32,
    demand: [u32; STRIDES as usize],
    bins: [u32; RAMP_BINS as usize],
}
const ALLOCATION_RAYS: u64 = std::mem::offset_of!(Allocation, rays) as u64;
const ALLOCATION_BLEND_GROUPS: u64 = std::mem::offset_of!(Allocation, blend_groups) as u64;
const ALLOCATION_TRACED: u64 = std::mem::offset_of!(Allocation, traced) as u64;
const ALLOCATION_BYTES: u64 = std::mem::size_of::<Allocation>() as u64;

/// `DdgiConvergence` in common.wgsl: the frame's sums of the active probes'
/// variability and their longest period, which the allocation and the
/// blends find and each frame clears, and the windows the settle pass
/// averages them over.
#[repr(C)]
struct Convergence {
    variability: u32,
    probes: u32,
    longest: u32,
    average: f32,
    window_sum: f32,
    window_updates: f32,
    window_turns: f32,
    previous: f32,
    converged: u32,
}
const CONVERGENCE_SUMS: u64 = std::mem::offset_of!(Convergence, average) as u64;
const CONVERGENCE_BYTES: u64 = std::mem::size_of::<Convergence>() as u64;
/// The starting probes' bins of distance (`RAMP_BINS` in allocate.wgsl).
const RAMP_BINS: u32 = 1024;
/// The lengthenings of every period the allocation weighs (`DDGI_STRIDES`).
const STRIDES: u32 = 7;
/// The frame's most rays, fixed rays included, in probes at the tier's
/// most: 32,768 at High, 16,384 at Low. Wicked's surfel GI traces at most
/// 100,000 a frame (4323a33c `SURFEL_RAY_BUDGET`) on hardware ray tracing;
/// on SGL3D's portable walk this many cost Hyperdrive's course about 2 ms
/// a frame on an Apple M5 (#196). A restart starts as many probes as the
/// budget holds at their starting rays, nearest first: about 126 a frame
/// within a spacing of the camera at High and about 124 at Low, and more
/// farther out, where a probe starts with fewer.
const BUDGET_PROBES: u32 = 128;

/// The frame's most rays, fixed rays included, at the tier's `max_rays`:
/// the ray list holds them.
fn budget(max_rays: u32) -> u32 {
    max_rays * BUDGET_PROBES
}
const UNIFORM_RAYS: u64 = std::mem::offset_of!(VolumeUniform, rays) as u64;
const UNIFORM_TRACED: u64 = std::mem::offset_of!(VolumeUniform, traced) as u64;

/// What the probes' state is kept for: a scene and the lattice its volume
/// lies on, which a scroll and a move of the render origin keep.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Key {
    scene: u64,
    lattice: u64,
}

impl Key {
    /// `scene`'s installed volume's.
    fn of(scene: &Scene) -> Option<Self> {
        Some(Self {
            scene: scene.id,
            lattice: scene.dynamic_gi?.lattice,
        })
    }
}

/// The dynamic GI stage. Owns its pipelines, the volume's probes and their
/// state, and a stand-in lit group 0 binds while no volume runs.
pub(crate) struct DynamicGi {
    layouts: Layouts,
    rank: wgpu::ComputePipeline,
    threshold: wgpu::ComputePipeline,
    allocate: wgpu::ComputePipeline,
    prepare_trace: wgpu::ComputePipeline,
    update_irradiance: wgpu::ComputePipeline,
    update_depth: wgpu::ComputePipeline,
    settle: wgpu::ComputePipeline,
    scroll: wgpu::ComputePipeline,
    /// The trace's programs for each path its rays take.
    paths: TracePaths,
    /// The trace's pipelines for each set of lit constants and path, each
    /// created when a frame first needs it, as the geometry pipelines
    /// specialise on the scene's rectangle lights and decals.
    trace: HashMap<(LitConstants, Option<RayQueryForm>), wgpu::ComputePipeline>,
    uniform: wgpu::Buffer,
    /// The moving instances' bounds the allocation takes.
    moving_bounds: wgpu::Buffer,
    stand_in: wgpu::TextureView,
    /// The probes of the last submitted frame that ran the stage, and the
    /// frames since they started before it.
    committed: Option<(Volume, u32)>,
    /// What the frame being rendered does with them, which `finish_frame`
    /// commits; an abandoned frame's is dropped by the next.
    rendered: Option<Rendered>,
    /// What the rendered frame's probes followed, which `finish_frame`
    /// commits with them.
    rendered_inputs: Option<Inputs>,
    /// The observation, from the first observed frame on, and the trace's
    /// groups 0 and 1 it creates its trace's pipelines with.
    #[cfg(feature = "diagnostics")]
    observer: Option<observe::Observer>,
    #[cfg(feature = "diagnostics")]
    trace_groups: [wgpu::BindGroupLayout; 2],
    /// The frames the renderer has finished: the number of the frame being
    /// rendered, which its report carries.
    #[cfg(feature = "diagnostics")]
    finished: u64,
}

/// What a rendered frame does with the probes.
enum Rendered {
    /// It does not run the stage.
    Off,
    /// It continues the committed probes, scrolled to this placement.
    Continue(InstalledVolume),
    /// It starts these fresh ones, leaving the committed probes as they were
    /// until it is submitted.
    Fresh(Box<Volume>),
}

impl DynamicGi {
    /// `lit` and `scene` are group 0's lit layout and group 1's.
    pub fn new(
        device: &wgpu::Device,
        lit: &wgpu::BindGroupLayout,
        scene: &wgpu::BindGroupLayout,
    ) -> Self {
        let layouts = Layouts::new(device);
        let module = |label, root| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(shading::compose(&[root]).into()),
            })
        };
        let allocate_shader = module("dynamic GI allocation", &ALLOCATE);
        let update_shader = module("dynamic GI blend", &UPDATE);
        let pipeline = |label, layout: &wgpu::BindGroupLayout, shader, entry_point| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                module: shader,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let rank = pipeline(
            "dynamic GI ramp rank",
            &layouts.allocate,
            &allocate_shader,
            "rank",
        );
        let threshold = pipeline(
            "dynamic GI ramp threshold",
            &layouts.allocate,
            &allocate_shader,
            "threshold",
        );
        let allocate = pipeline(
            "dynamic GI allocation",
            &layouts.allocate,
            &allocate_shader,
            "allocate",
        );
        let prepare_trace = pipeline(
            "dynamic GI dispatch",
            &layouts.allocate,
            &allocate_shader,
            "prepare_trace",
        );
        let update_irradiance = pipeline(
            "dynamic GI irradiance blend",
            &layouts.update,
            &update_shader,
            "update_irradiance",
        );
        let update_depth = pipeline(
            "dynamic GI depth blend",
            &layouts.update,
            &update_shader,
            "update_depth",
        );
        let settle = pipeline(
            "dynamic GI convergence",
            &layouts.update,
            &update_shader,
            "settle",
        );
        let scroll = pipeline(
            "dynamic GI scroll",
            &layouts.update,
            &update_shader,
            "scroll",
        );
        Self {
            paths: TracePaths::new("dynamic GI rays", &TRACE, &layouts.trace, [lit, scene]),
            trace: HashMap::new(),
            rank,
            threshold,
            allocate,
            prepare_trace,
            update_irradiance,
            update_depth,
            settle,
            scroll,
            uniform: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("dynamic GI volume"),
                    size: std::mem::size_of::<VolumeUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            moving_bounds: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("dynamic GI moving bounds"),
                    size: MOST_MOVING_BOUNDS as u64 * std::mem::size_of::<BoundsUniform>() as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            stand_in: texture(device, "no dynamic GI probes", [1, 1], layout::FORMAT),
            layouts,
            committed: None,
            rendered: None,
            rendered_inputs: None,
            #[cfg(feature = "diagnostics")]
            observer: None,
            #[cfg(feature = "diagnostics")]
            trace_groups: [lit.clone(), scene.clone()],
            #[cfg(feature = "diagnostics")]
            finished: 0,
        }
    }

    /// The probes lit group 0 binds: the rendered frame's, else the
    /// committed ones, else a stand-in.
    pub fn probes(&self) -> &wgpu::TextureView {
        match &self.rendered {
            Some(Rendered::Fresh(volume)) => &volume.probes,
            Some(Rendered::Off) => &self.stand_in,
            Some(Rendered::Continue(_)) | None => self
                .committed
                .as_ref()
                .map_or(&self.stand_in, |(volume, _)| &volume.probes),
        }
    }

    /// The committed probes, those of the last submitted frame that ran
    /// the stage, with the placement that frame gave them, while they are
    /// for `scene`'s installed volume's lattice: what a probe capture
    /// between frames is lit by. A frame rendered since and abandoned, and a
    /// scroll no frame has run, do not change them.
    pub fn lights(&self, scene: &Scene) -> Option<(ProbePlacement, &wgpu::TextureView)> {
        let (volume, _) = self.committed.as_ref()?;
        (Some(volume.key) == Key::of(scene))
            .then(|| (volume.installed.placement(scene.origin()), &volume.probes))
    }

    /// What lit group 0 binds where no probes light the view.
    pub fn stand_in(&self) -> &wgpu::TextureView {
        &self.stand_in
    }

    /// Chooses the frame's probes: the committed ones where they are for
    /// the scene and its placement, else fresh ones, and none while the
    /// stage does not run.
    pub fn prepare(&mut self, device: &wgpu::Device, scene: &Scene, effective: &Effective) {
        #[cfg(feature = "diagnostics")]
        if let Some(observer) = &mut self.observer {
            observer.begin();
        }
        let (Some(max_rays), Some(key), Some(installed)) =
            (effective.dynamic_gi, Key::of(scene), scene.dynamic_gi)
        else {
            self.rendered = Some(Rendered::Off);
            return;
        };
        let rendered = match &mut self.committed {
            Some((volume, _)) if volume.key == key => {
                // The rays' textures hold nothing from frame to frame.
                if volume.max_rays != max_rays {
                    volume.rays = Some(volume.ray_groups(
                        device,
                        &self.layouts,
                        (&self.uniform, &self.moving_bounds),
                        max_rays,
                    ));
                    volume.max_rays = max_rays;
                }
                Rendered::Continue(installed)
            }
            _ => Rendered::Fresh(Box::new(Volume::new(
                device,
                &self.layouts,
                (&self.uniform, &self.moving_bounds),
                (key, installed),
                max_rays,
            ))),
        };
        self.rendered = Some(rendered);
    }

    /// After the caller submitted the rendered frame: its probes become the
    /// committed ones.
    pub fn finish_frame(&mut self) {
        #[cfg(feature = "diagnostics")]
        {
            if let Some(observer) = &mut self.observer {
                observer.submitted();
            }
            self.finished += 1;
        }
        let inputs = self.rendered_inputs.take();
        match self.rendered.take() {
            None => {}
            Some(Rendered::Off) => self.committed = None,
            Some(Rendered::Continue(installed)) => {
                if let Some((volume, frames)) = &mut self.committed {
                    volume.installed = installed;
                    volume.inputs = inputs;
                    *frames = frames.wrapping_add(1);
                }
            }
            Some(Rendered::Fresh(mut volume)) => {
                volume.inputs = inputs;
                self.committed = Some((*volume, 0));
            }
        }
    }

    /// Makes the trace's pipeline compiled with `lit` for the path
    /// `hardware` takes; returns the path it took (`TracePaths::pipeline`).
    fn trace_pipeline(
        &mut self,
        device: &wgpu::Device,
        lit: LitConstants,
        hardware: Option<HardwareRays<'_>>,
    ) -> Option<RayQueryForm> {
        self.paths.pipeline(
            device,
            hardware,
            (&mut self.trace, lit),
            |TracePath { shader, layout, .. }| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("dynamic GI rays"),
                    layout: Some(layout),
                    module: shader,
                    entry_point: Some("trace"),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &lit.constants(),
                        ..Default::default()
                    },
                    cache: None,
                })
            },
        )
    }

    /// Allocates, traces and blends the frame's rays.
    pub fn encode(&mut self, ctx: &mut FrameContext<'_>) {
        let Some(max_rays) = ctx.effective.dynamic_gi else {
            return;
        };
        let lit = LitConstants::of(ctx.scene);
        let form = self.trace_pipeline(ctx.device, lit, ctx.hardware_rays);
        #[cfg(feature = "diagnostics")]
        let mut observer = observe::Observer::for_frame(
            &mut self.observer,
            &mut self.paths,
            (&self.trace_groups, &self.layouts.trace),
            ctx,
            (lit, form),
        );
        // The whole spacings the volume has moved since the committed frame.
        let (volume, frame, installed, scrolled) = match (&self.rendered, &self.committed) {
            (Some(Rendered::Fresh(volume)), _) => (&**volume, 0, volume.installed, I64Vec3::ZERO),
            (Some(Rendered::Continue(installed)), Some((volume, frames))) => (
                volume,
                frames.wrapping_add(1),
                *installed,
                installed.scroll - volume.installed.scroll,
            ),
            _ => return,
        };
        let Some(rays) = volume.rays.as_ref() else {
            return;
        };
        let ProbePlacement {
            volume: placement,
            scroll,
        } = installed.placement(ctx.scene.origin());
        // A move of a whole lattice or more clears every probe.
        let probes = I64Vec3::from_array(placement.probes.map(i64::from));
        let scrolled = scrolled.clamp(-probes, probes).as_ivec3();
        let camera = ctx.input.camera;
        let uniform = VolumeUniform {
            origin: placement.origin.to_array(),
            max_distance: placement.spacing.max_element() * MAX_DISTANCE_SPACINGS,
            spacing: placement.spacing.to_array(),
            max_rays,
            probes: placement.probes,
            probe_count: volume::probe_count(placement.probes) as u32,
            rotation: rotation(frame).to_cols_array_2d().map(|column| {
                let [x, y, z] = column;
                [x, y, z, 0.]
            }),
            frustum: frustum(camera.projection * camera.view),
            eye: camera.eye.to_array(),
            frame,
            rays: 0,
            budget: budget(max_rays),
            traced: 0,
            padding: 0,
            scroll,
            padding_scroll: 0,
            scrolled: scrolled.to_array(),
            moving_count: 0,
            changed: 0,
            padding_changed: [0; 3],
        };
        let bounds = moving_bounds(ctx.scene, &placement, camera.eye);
        let inputs = Inputs::of(ctx, max_rays);
        let trace_group = self.paths.group(
            ctx.device,
            ctx.hardware_rays,
            &[
                (0, self.uniform.as_entire_binding()),
                (1, wgpu::BindingResource::TextureView(&rays.list)),
                (2, wgpu::BindingResource::TextureView(&rays.results)),
            ],
        );
        let changes = inputs.changes(match (&self.rendered, &self.committed) {
            (Some(Rendered::Continue(_)), Some((volume, _))) => volume.inputs.as_ref(),
            _ => None,
        });
        let changed = changes.any();
        self.rendered_inputs = Some(inputs);
        let uniform = VolumeUniform {
            moving_count: bounds.len() as u32,
            changed: u32::from(changed),
            ..uniform
        };
        if !bounds.is_empty() {
            crate::counters::write_buffer(
                ctx.queue,
                &self.moving_bounds,
                0,
                bytemuck::cast_slice(&bounds),
            );
        }
        crate::counters::write_buffer(ctx.queue, &self.uniform, 0, bytemuck::bytes_of(&uniform));
        let probes = dispatch(uniform.probe_count);
        let timing = ctx.timing;
        let encoder = &mut *ctx.encoder;
        encoder.clear_buffer(&volume.allocation, 0, None);
        encoder.clear_buffer(&volume.convergence, 0, Some(CONVERGENCE_SUMS));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("dynamic GI allocation"),
                timestamp_writes: timing.and_then(|t| t.compute_pass("dynamic GI allocation")),
            });
            if scrolled != IVec3::ZERO {
                pass.set_bind_group(0, &rays.update, &[]);
                pass.set_pipeline(&self.scroll);
                pass.dispatch_workgroups(uniform.probe_count.div_ceil(64), 1, 1);
            }
            pass.set_bind_group(0, &rays.allocate, &[]);
            pass.set_pipeline(&self.rank);
            pass.dispatch_workgroups(uniform.probe_count.div_ceil(64), 1, 1);
            pass.set_pipeline(&self.threshold);
            pass.dispatch_workgroups(1, 1, 1);
            pass.set_pipeline(&self.allocate);
            pass.dispatch_workgroups(probes[0], probes[1], 1);
            pass.set_pipeline(&self.prepare_trace);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(
            &volume.allocation,
            ALLOCATION_RAYS,
            &self.uniform,
            UNIFORM_RAYS,
            4,
        );
        encoder.copy_buffer_to_buffer(
            &volume.allocation,
            ALLOCATION_TRACED,
            &self.uniform,
            UNIFORM_TRACED,
            4,
        );
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("dynamic GI rays"),
                timestamp_writes: timing.and_then(|t| t.compute_pass("dynamic GI rays")),
            });
            pass.set_pipeline(&self.trace[&(lit, form)]);
            #[cfg(feature = "diagnostics")]
            if let Some(observer) = observer.as_deref_mut() {
                observer.bind_trace(ctx.device, &mut pass, lit, rays);
            }
            pass.set_bind_group(0, ctx.bindings.volume_lit(), &[]);
            pass.set_bind_group(1, &ctx.scene.scene_group, &[]);
            pass.set_bind_group(3, trace_group, &[]);
            pass.dispatch_workgroups_indirect(&volume.allocation, 0);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("dynamic GI blend"),
                timestamp_writes: timing.and_then(|t| t.compute_pass("dynamic GI blend")),
            });
            pass.set_bind_group(0, &rays.update, &[]);
            pass.set_pipeline(&self.update_irradiance);
            pass.dispatch_workgroups_indirect(&volume.allocation, ALLOCATION_BLEND_GROUPS);
            pass.set_pipeline(&self.update_depth);
            pass.dispatch_workgroups_indirect(&volume.allocation, ALLOCATION_BLEND_GROUPS);
            pass.set_pipeline(&self.settle);
            pass.dispatch_workgroups(1, 1, 1);
        }
        #[cfg(feature = "diagnostics")]
        if let Some(observer) = observer {
            observer.observe(
                ctx.device,
                encoder,
                (&self.uniform, rays, volume),
                observe::Frame {
                    number: self.finished,
                    probes: uniform.probe_count,
                    changes,
                    scrolled: uniform.scrolled,
                    moving_bounds: uniform.moving_count,
                },
            );
        }
    }

    /// The observed frames read back since the last call, oldest first.
    #[cfg(feature = "diagnostics")]
    pub fn take_reports(
        &mut self,
        device: &wgpu::Device,
    ) -> Vec<crate::diagnostics::DynamicGiReport> {
        self.observer
            .as_mut()
            .map_or_else(Vec::new, |observer| observer.take_reports(device))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
impl DynamicGi {
    /// The rays the last frame that ran the stage traced.
    pub fn test_traced_rays(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> u32 {
        let volume = match (&self.rendered, &self.committed) {
            (Some(Rendered::Fresh(volume)), _) => volume,
            (_, Some((volume, _))) => volume,
            _ => return 0,
        };
        crate::test_support::read_words(device, queue, &volume.allocation)
            [(ALLOCATION_RAYS / 4) as usize]
    }

    /// Whether the last frame that ran the stage paused the volume.
    pub fn test_paused(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        let volume = match (&self.rendered, &self.committed) {
            (Some(Rendered::Fresh(volume)), _) => volume,
            (_, Some((volume, _))) => volume,
            _ => return false,
        };
        crate::test_support::read_words(device, queue, &volume.allocation)
            [std::mem::offset_of!(Allocation, paused) / 4]
            != 0
    }

    /// The rays each probe blended in the last frame that ran the stage,
    /// by its stored index: those beside its fixed rays, or on its first
    /// turn its fixed rays too; none off its turn.
    pub fn test_probe_rays(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<u32> {
        let volume = match (&self.rendered, &self.committed) {
            (Some(Rendered::Fresh(volume)), _) => volume,
            (_, Some((volume, _))) => volume,
            _ => return Vec::new(),
        };
        let count = volume::probe_count(volume.installed.probes) as usize;
        crate::test_support::read_words(device, queue, &volume.ray_counts)[..count].to_vec()
    }

    /// Each probe's share of back faces (`DdgiProbe::backfaces`), by its
    /// stored index, after the last frame that ran the stage.
    pub fn test_backface_shares(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<f32> {
        let volume = match (&self.rendered, &self.committed) {
            (Some(Rendered::Fresh(volume)), _) => volume,
            (_, Some((volume, _))) => volume,
            _ => return Vec::new(),
        };
        crate::test_support::read_words(device, queue, &volume.probe_states)
            .chunks_exact(4)
            .map(|state| f32::from_bits(state[2]))
            .collect()
    }
}

/// The world bounds of `scene`'s drawn moving instances that reach into the
/// cells of `placement`'s probes, the nearest `eye` first, at most
/// `MOST_MOVING_BOUNDS`: the probes about them trace as active ones do.
fn moving_bounds(scene: &Scene, placement: &DynamicGiVolume, eye: Vec3) -> Vec<BoundsUniform> {
    let extent = [
        placement.origin - placement.spacing,
        placement.end() + placement.spacing,
    ];
    let mut bounds: Vec<(f32, [Vec3; 2])> = scene
        .instances
        .slots
        .iter()
        .filter(|(_, instance)| {
            instance.mobility == crate::Mobility::Moving && instance.state.visible
        })
        .map(|(_, instance)| {
            let model = scene.drawn_model(instance.state.model);
            crate::scene::static_edits::posed_bounds(instance.bounds(model), instance.state.pose)
        })
        .filter(|[min, max]| min.cmple(extent[1]).all() && max.cmpge(extent[0]).all())
        .map(|[min, max]| ((min + max).distance_squared(eye * 2.), [min, max]))
        .collect();
    bounds.sort_by(|a, b| a.0.total_cmp(&b.0));
    bounds
        .into_iter()
        .take(MOST_MOVING_BOUNDS as usize)
        .map(|(_, [min, max])| BoundsUniform {
            min: min.to_array(),
            padding_min: 0.,
            max: max.to_array(),
            padding_max: 0.,
        })
        .collect()
}

/// A workgroup per probe of `count`, in rows of `GROUP_ROW`.
fn dispatch(count: u32) -> [u32; 2] {
    [count.min(GROUP_ROW), count.div_ceil(GROUP_ROW)]
}

/// Thomas Wang's hash, as Wicked's RNG seeds through it.
fn hash(seed: u32) -> u32 {
    let mut seed = (seed ^ 61) ^ (seed >> 16);
    seed = seed.wrapping_mul(9);
    seed ^= seed >> 4;
    seed = seed.wrapping_mul(0x27d4_eb2d);
    seed ^ (seed >> 15)
}

/// Frame `frame`'s rotation of every probe's rays: a random angle about a
/// random axis, as `wiRenderer::DDGI` draws its `g_xTransform` each frame
/// (df44c3d wiRenderer.cpp 12520–12528).
fn rotation(frame: u32) -> Mat3 {
    let random = |index: u32| hash(frame.wrapping_mul(4).wrapping_add(index)) as f32 / 4294967296.;
    let angle = random(0) * std::f32::consts::TAU;
    let axis = Vec3::new(random(1), random(2), random(3)) * 2. - 1.;
    Mat3::from_axis_angle(axis.try_normalize().unwrap_or(Vec3::Y), angle)
}

/// The planes of `clip_from_world`'s clip volume, each normalised with the
/// inside where `dot(xyz, p) + w >= 0`; a plane at infinity, such as
/// `perspective`'s far plane, takes everything.
fn frustum(clip_from_world: Mat4) -> [[f32; 4]; 6] {
    let row = |index| clip_from_world.row(index);
    [
        row(3) + row(0),
        row(3) - row(0),
        row(3) + row(1),
        row(3) - row(1),
        row(2),
        row(3) - row(2),
    ]
    .map(|plane: Vec4| {
        let length = plane.truncate().length();
        if length > 1e-12 && length.is_finite() {
            (plane / length).to_array()
        } else {
            [0., 0., 0., 1.]
        }
    })
}

/// The layouts this stage mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    let mut mirrors = vec![
        mirror!(
            "dynamic_gi_trace",
            "DdgiVolume",
            VolumeUniform,
            [
                origin,
                max_distance,
                spacing,
                max_rays,
                probes,
                probe_count,
                rotation,
                frustum,
                eye,
                frame,
                rays,
                budget,
                traced,
                scroll,
                scrolled,
                moving_count,
                changed,
            ]
        ),
        mirror!(
            "dynamic_gi_allocate",
            "DdgiConvergence",
            Convergence,
            [
                variability,
                probes,
                longest,
                average,
                window_sum,
                window_updates,
                window_turns,
                previous,
                converged,
            ]
        ),
        mirror!(
            "dynamic_gi_allocate",
            "DdgiBounds",
            BoundsUniform,
            [min, max]
        ),
        mirror!(
            "dynamic_gi_allocate",
            "DdgiAllocation",
            Allocation,
            [
                groups,
                rays,
                blend_groups,
                traced,
                ramp_bins,
                ramp_room,
                ramp_taken,
                unblended,
                unblended_rays,
                paused,
                stride,
                reserved,
                spare,
                spare_taken,
                demand,
                bins,
            ]
        ),
    ];
    #[cfg(feature = "diagnostics")]
    mirrors.extend(observe::mirrors());
    mirrors
}

/// The constants this stage shares with its shaders.
#[cfg(test)]
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    use crate::shading::layout_tests::Constant;
    let mut constants = vec![
        Constant::new(
            "dynamic_gi_trace",
            "DDGI_GROUP_ROW",
            naga::Literal::U32(GROUP_ROW),
        ),
        Constant::new(
            "dynamic_gi_allocate",
            "RAMP_BINS",
            naga::Literal::U32(RAMP_BINS),
        ),
        Constant::new(
            "dynamic_gi_allocate",
            "DDGI_STRIDES",
            naga::Literal::U32(STRIDES),
        ),
        Constant::new(
            "dynamic_gi_allocate",
            "DDGI_MOST_MOVING_BOUNDS",
            naga::Literal::U32(MOST_MOVING_BOUNDS),
        ),
    ];
    #[cfg(feature = "diagnostics")]
    constants.extend(observe::constants());
    constants
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod allocation_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
