//! Dynamic GI: the scene's dynamic GI volume of probes, kept up by rays
//! through the scene's ray source each frame, within a budget of rays, as
//! Wicked Engine's DDGI keeps its own (`wiRenderer::DDGI`, df44c3d
//! wiRenderer.cpp 12412–12618): each probe's rays allocated on its turns
//! from its irradiance estimator, traced with each hit shaded as the
//! probe-hit receiver kind, then blended into its irradiance and depth
//! maps, the probe moving away from surfaces it nears.
//! The shaders are in `dynamic_gi/`, with their pipelines in `pipelines`
//! and the buffers they share in `buffers`; the probe texture's layout and
//! the sample are `shading::dynamic_gi`'s.
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
use crate::scene::dynamic_gi::{InstalledVolume, ProbePlacement};
use crate::shading::dynamic_gi as layout;
use crate::view::effective::Effective;
use crate::view::frame::FrameContext;
use crate::view::pipelines::LitConstants;
use buffers::{
    ALLOCATION_BLEND_GROUPS, ALLOCATION_RAYS, ALLOCATION_TRACED, BoundsUniform, CONVERGENCE_SUMS,
    MOST_MOVING_BOUNDS, UNIFORM_RAYS, UNIFORM_TRACED, VolumeUniform, budget, dispatch,
};
use frame::{frustum, moving_bounds, rotation};
use glam::{I64Vec3, IVec3};
#[cfg(feature = "diagnostics")]
pub use inputs::DynamicGiChanges;
use inputs::Inputs;
use pipelines::Pipelines;
use volume::{Layouts, Volume, texture};

mod buffers;
mod frame;
mod inputs;
#[cfg(feature = "diagnostics")]
pub(crate) mod observe;
pub(crate) mod pipelines;
mod volume;

/// The longest distance a ray's depth counts, in spacings along the
/// volume's longest: Wicked's max_distance (wiScene.cpp 1037).
const MAX_DISTANCE_SPACINGS: f32 = 1.5;

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
    pipelines: Pipelines,
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
        Self {
            pipelines: Pipelines::new(device, &layouts, lit, scene),
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

    /// Allocates, traces and blends the frame's rays.
    pub fn encode(&mut self, ctx: &mut FrameContext<'_>) {
        let Some(max_rays) = ctx.effective.dynamic_gi else {
            return;
        };
        let lit = LitConstants::of(ctx.scene);
        let form = self
            .pipelines
            .trace_pipeline(ctx.device, lit, ctx.hardware_rays);
        #[cfg(feature = "diagnostics")]
        let mut observer = observe::Observer::for_frame(
            &mut self.observer,
            &mut self.pipelines.paths,
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
        let trace_group = self.pipelines.paths.group(
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
                pass.set_pipeline(&self.pipelines.scroll);
                pass.dispatch_workgroups(uniform.probe_count.div_ceil(64), 1, 1);
            }
            pass.set_bind_group(0, &rays.allocate, &[]);
            pass.set_pipeline(&self.pipelines.rank);
            pass.dispatch_workgroups(uniform.probe_count.div_ceil(64), 1, 1);
            pass.set_pipeline(&self.pipelines.threshold);
            pass.dispatch_workgroups(1, 1, 1);
            pass.set_pipeline(&self.pipelines.allocate);
            pass.dispatch_workgroups(probes[0], probes[1], 1);
            pass.set_pipeline(&self.pipelines.prepare_trace);
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
            pass.set_pipeline(&self.pipelines.trace[&(lit, form)]);
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
            pass.set_pipeline(&self.pipelines.update_irradiance);
            pass.dispatch_workgroups_indirect(&volume.allocation, ALLOCATION_BLEND_GROUPS);
            pass.set_pipeline(&self.pipelines.update_depth);
            pass.dispatch_workgroups_indirect(&volume.allocation, ALLOCATION_BLEND_GROUPS);
            pass.set_pipeline(&self.pipelines.settle);
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
            [std::mem::offset_of!(buffers::Allocation, paused) / 4]
            != 0
    }

    /// The rays each probe traced beside its fixed rays in the last frame
    /// that ran the stage, by its stored index: none off its turn.
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

/// The layouts this stage mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    let mut mirrors = buffers::mirrors();
    #[cfg(feature = "diagnostics")]
    mirrors.extend(observe::mirrors());
    mirrors
}

/// The constants this stage shares with its shaders.
#[cfg(test)]
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    let mut constants = buffers::constants();
    #[cfg(feature = "diagnostics")]
    constants.extend(observe::constants());
    constants
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod allocation_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
