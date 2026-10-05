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
//! volume's placement, and starts afresh when either differs or after a
//! submitted frame that did not run it. State that must agree with a
//! submitted frame is on the GPU; the key is committed at `finish_frame`,
//! so an abandoned frame leaves both as they were.
use crate::Scene;
use crate::content::dynamic_gi::DynamicGiVolume;
use crate::scene::dynamic_gi::InstalledVolume;
use crate::shading::{self, dynamic_gi as layout};
use crate::view::effective::Effective;
use crate::view::frame::FrameContext;
use crate::view::pipelines::LitConstants;
use glam::{Mat3, Mat4, Vec3, Vec4};
use std::collections::HashMap;
use volume::{Layouts, Volume, texture};

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
    deps: &[
        &shading::BIND_LIT,
        &shading::SCENE_RAYS_PORTABLE,
        &shading::SURFACE_RAY,
        &COMMON,
    ],
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
    ramp_probes: u32,
    traced: u32,
    padding: u32,
}

/// The allocation's buffer (`DdgiAllocation` in allocate.wgsl): the
/// trace's indirect dispatch and ray count, the blends' dispatch and the
/// count of probes that trace, the ramp's words and its bins. The trace and
/// the blends read the counts in the volume's uniform.
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
    bins: [u32; RAMP_BINS as usize],
}
const ALLOCATION_RAYS: u64 = std::mem::offset_of!(Allocation, rays) as u64;
const ALLOCATION_BLEND_GROUPS: u64 = std::mem::offset_of!(Allocation, blend_groups) as u64;
const ALLOCATION_TRACED: u64 = std::mem::offset_of!(Allocation, traced) as u64;
const ALLOCATION_BYTES: u64 = std::mem::size_of::<Allocation>() as u64;
/// The ramp's bins of distance (`RAMP_BINS` in allocate.wgsl).
const RAMP_BINS: u32 = 1024;
/// The rays a frame gives the probes it starts while some have not
/// started: at most this many over the quality's most rays start a frame,
/// where Wicked starts every probe at once.
const RAMP_RAYS: u32 = 32768;
const UNIFORM_RAYS: u64 = std::mem::offset_of!(VolumeUniform, rays) as u64;
const UNIFORM_TRACED: u64 = std::mem::offset_of!(VolumeUniform, traced) as u64;

/// What the probes' state is kept for: a scene and its volume's placement,
/// in the frame the scene was created in, so a move of the render origin
/// keeps it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Key {
    scene: u64,
    volume: InstalledVolume,
}

impl Key {
    /// `scene`'s installed volume's.
    fn of(scene: &Scene) -> Option<Self> {
        Some(Self {
            scene: scene.id,
            volume: scene.dynamic_gi?,
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
    trace_shader: wgpu::ShaderModule,
    trace_layout: wgpu::PipelineLayout,
    /// The trace's pipelines for each set of lit constants, each created
    /// when a frame first needs it, as the geometry pipelines specialise on
    /// the scene's rectangle lights and decals.
    trace: HashMap<LitConstants, wgpu::ComputePipeline>,
    uniform: wgpu::Buffer,
    stand_in: wgpu::TextureView,
    /// The probes of the last submitted frame that ran the stage, and the
    /// frames since they started before it.
    committed: Option<(Volume, u32)>,
    /// What the frame being rendered does with them, which `finish_frame`
    /// commits; an abandoned frame's is dropped by the next.
    rendered: Option<Rendered>,
}

/// What a rendered frame does with the probes.
enum Rendered {
    /// It does not run the stage.
    Off,
    /// It continues the committed probes.
    Continue,
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
        let trace_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dynamic GI rays"),
            bind_group_layouts: &[Some(lit), Some(scene), None, Some(&layouts.trace)],
            immediate_size: 0,
        });
        Self {
            trace_shader: module("dynamic GI rays", &TRACE),
            trace_layout,
            trace: HashMap::new(),
            rank,
            threshold,
            allocate,
            prepare_trace,
            update_irradiance,
            update_depth,
            uniform: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("dynamic GI volume"),
                    size: std::mem::size_of::<VolumeUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            stand_in: texture(device, "no dynamic GI probes", [1, 1], layout::FORMAT),
            layouts,
            committed: None,
            rendered: None,
        }
    }

    /// The probes lit group 0 binds: the rendered frame's, else the
    /// committed ones, else a stand-in.
    pub fn probes(&self) -> &wgpu::TextureView {
        match &self.rendered {
            Some(Rendered::Fresh(volume)) => &volume.probes,
            Some(Rendered::Off) => &self.stand_in,
            Some(Rendered::Continue) | None => self
                .committed
                .as_ref()
                .map_or(&self.stand_in, |(volume, _)| &volume.probes),
        }
    }

    /// The committed probes, those of the last submitted frame that ran
    /// the stage, with their placement, while they are `scene`'s installed
    /// volume's: what a probe capture between frames is lit by. A frame
    /// rendered since and abandoned does not change them.
    pub fn lights(&self, scene: &Scene) -> Option<(DynamicGiVolume, &wgpu::TextureView)> {
        let (volume, _) = self.committed.as_ref()?;
        let placement = scene.dynamic_gi_volume()?;
        (Some(volume.key) == Key::of(scene)).then_some((placement, &volume.probes))
    }

    /// What lit group 0 binds where no probes light the view.
    pub fn stand_in(&self) -> &wgpu::TextureView {
        &self.stand_in
    }

    /// Chooses the frame's probes: the committed ones where they are for
    /// the scene and its placement, else fresh ones, and none while the
    /// stage does not run.
    pub fn prepare(&mut self, device: &wgpu::Device, scene: &Scene, effective: &Effective) {
        let (Some(max_rays), Some(key)) = (effective.dynamic_gi, Key::of(scene)) else {
            self.rendered = Some(Rendered::Off);
            return;
        };
        let rendered = match &mut self.committed {
            Some((volume, _)) if volume.key == key => {
                // The rays' textures hold nothing from frame to frame.
                if volume.max_rays != max_rays {
                    volume.rays =
                        Some(volume.ray_groups(device, &self.layouts, &self.uniform, max_rays));
                    volume.max_rays = max_rays;
                }
                Rendered::Continue
            }
            _ => Rendered::Fresh(Box::new(Volume::new(
                device,
                &self.layouts,
                &self.uniform,
                key,
                max_rays,
            ))),
        };
        self.rendered = Some(rendered);
    }

    /// After the caller submitted the rendered frame: its probes become the
    /// committed ones.
    pub fn finish_frame(&mut self) {
        match self.rendered.take() {
            None => {}
            Some(Rendered::Off) => self.committed = None,
            Some(Rendered::Continue) => {
                if let Some((_, frames)) = &mut self.committed {
                    *frames = frames.wrapping_add(1);
                }
            }
            Some(Rendered::Fresh(volume)) => self.committed = Some((*volume, 0)),
        }
    }

    fn trace_pipeline(&mut self, device: &wgpu::Device, lit: LitConstants) {
        let (shader, layout) = (&self.trace_shader, &self.trace_layout);
        self.trace.entry(lit).or_insert_with(|| {
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
        });
    }

    /// Allocates, traces and blends the frame's rays.
    pub fn encode(&mut self, ctx: &mut FrameContext<'_>) {
        let Some(max_rays) = ctx.effective.dynamic_gi else {
            return;
        };
        let lit = LitConstants::of(ctx.scene);
        self.trace_pipeline(ctx.device, lit);
        let (volume, frame) = match (&self.rendered, &self.committed) {
            (Some(Rendered::Fresh(volume)), _) => (&**volume, 0),
            (Some(Rendered::Continue), Some((volume, frames))) => (volume, frames.wrapping_add(1)),
            _ => return,
        };
        let Some(rays) = volume.rays.as_ref() else {
            return;
        };
        // In the render frame of the scene the key matched.
        let Some(placement) = ctx.scene.dynamic_gi_volume() else {
            return;
        };
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
            ramp_probes: (RAMP_RAYS / max_rays).max(1),
            traced: 0,
            padding: 0,
        };
        crate::counters::write_buffer(ctx.queue, &self.uniform, 0, bytemuck::bytes_of(&uniform));
        let probes = dispatch(uniform.probe_count);
        let timing = ctx.timing;
        let encoder = &mut *ctx.encoder;
        encoder.clear_buffer(&volume.allocation, 0, None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("dynamic GI allocation"),
                timestamp_writes: timing.and_then(|t| t.compute_pass("dynamic GI allocation")),
            });
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
            pass.set_pipeline(&self.trace[&lit]);
            pass.set_bind_group(0, ctx.bindings.volume_lit(), &[]);
            pass.set_bind_group(1, &ctx.scene.scene_group, &[]);
            pass.set_bind_group(3, &rays.trace, &[]);
            pass.dispatch_workgroups_indirect(&volume.allocation, 0);
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("dynamic GI blend"),
            timestamp_writes: timing.and_then(|t| t.compute_pass("dynamic GI blend")),
        });
        pass.set_bind_group(0, &rays.update, &[]);
        pass.set_pipeline(&self.update_irradiance);
        pass.dispatch_workgroups_indirect(&volume.allocation, ALLOCATION_BLEND_GROUPS);
        pass.set_pipeline(&self.update_depth);
        pass.dispatch_workgroups_indirect(&volume.allocation, ALLOCATION_BLEND_GROUPS);
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
    vec![
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
                ramp_probes,
                traced,
            ]
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
                bins,
            ]
        ),
    ]
}

/// The constants this stage shares with its shaders.
#[cfg(test)]
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    use crate::shading::layout_tests::Constant;
    vec![
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
    ]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
