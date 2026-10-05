//! Prepare: the frame's uploads and views. It turns the frame's input into
//! the camera's view data and the frame's data (the directional shadow's
//! cascades, fit from the camera, baked lighting and fog, the
//! jitter antialiasing chose), uploads them, ends the motion of moving
//! instances not posed since the last submitted frame, sorts the mist,
//! updates the scene's ray instances on frames that trace them (world-space
//! reflections, dynamic GI and ray-traced shadows) and, while
//! hardware ray tracing is in effect, chooses those frames'
//! acceleration-structure builds, which it records after the deform pass
//! (`encode_acceleration_structures`), clusters the
//! scene's lights and decals for the camera and culls them for ray hits and
//! the dynamic GI volume's probe hits, builds the camera's blended draw list
//! (culled, LOD-selected, sorted back to front) on the CPU, and prepares
//! the GPU-built lists of the camera's opaque and masked surfaces and each
//! directional shadow cascade, which the cull stage builds after the
//! deform pass (`stages::cull`). The local-light shadow atlas places its own faces
//! (`shadows::local`). For a probe capture it builds the capture's own
//! views, cascades and light and decal list (`capture`).
//!
//! Reads: the frame input, the camera history and the scene. Writes:
//! `FrameViews` (view uniforms, draw lists, their draw instances and
//! clusters), the frame uniform, the scene's stale object records, draw
//! candidates, ray instances, acceleration structures and mist order.
//! Honours: the effective local lights, temporal antialiasing (the shadow
//! filter), atmosphere, baked lighting, culling, world-space reflections,
//! dynamic GI, hardware ray tracing and ray-traced shadows.
//! Timing groups: none.
use crate::scene::dynamic_gi::ProbePlacement;
use crate::scene::rays::acceleration::RayTracingStats;
use crate::settings::WorldSpaceReflections;
use crate::shading::uniforms::{FrameValues, ViewUniform};
use crate::view::clusters::{BoxVolume, CAMERA_CLUSTERS, Clusters, ViewVolume};
use crate::view::draw_list::gpu::{camera_cull, cascade_cull};
use crate::view::draw_list::{DrawInstances, DrawList};
use crate::view::effective::{Effective, HardwareRayTracing};
use crate::view::frame::FrameContext;
use crate::view::history::HistoryFrame;
use crate::view::population::Population;
use crate::view::reflection_camera;
use crate::view::{FrameShadow, FrameViews, Jitter, View, frame_uniform, lod::LodSelector};
use crate::{FrameInput, Scene};
use glam::{Mat4, Vec3};

/// A probe capture's views: the frame data its views share, each face's
/// view data, each directional shadow cascade's, and what they draw.
pub(crate) struct CaptureViews {
    /// The frame's `FrameUniform`, with the capture's cascades.
    pub frame: wgpu::Buffer,
    /// Each face's `ViewUniform` (`View::probe_face`), in cube-face order.
    pub faces: Vec<wgpu::Buffer>,
    /// Each directional shadow cascade's `ViewUniform`, nearest first; none
    /// without a directional shadow.
    pub cascades: Vec<wgpu::Buffer>,
    /// What every cascade draws: the unculled static capture-visible casters.
    pub casters: DrawList,
    /// What every face draws: the same unculled static capture-visible
    /// instances.
    pub list: DrawList,
    /// The capture's draw instances: these lists' and its local-light
    /// shadow layers'.
    pub instances: DrawInstances,
    /// The scene lights and decals its surfaces take: every light that is
    /// on, baked ones taken only by receivers without baked lighting, and
    /// every decal.
    pub clusters: Clusters,
}

/// Sets the frame's directional shadow cascades to views with
/// `clip_from_world`, nearest first, and prepares each one's GPU-built list
/// of casters under the frame's `mask`, which are its own, independently of
/// the main view's.
pub(crate) fn set_cascades(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &Scene,
    views: &mut FrameViews,
    mask: u32,
    clip_from_world: impl ExactSizeIterator<Item = Mat4>,
) {
    views.cascade_count = clip_from_world.len();
    for (slot, clip_from_world) in views.cascades.iter_mut().zip(clip_from_world) {
        let view = View::shadow_cascade(clip_from_world);
        slot.set(queue, view);
        let started = crate::counters::Moment::now();
        slot.list
            .prepare(device, queue, scene, cascade_cull(&view, mask));
        slot.built(started);
    }
}

/// Why the hardware path did not trace a frame that asked for it.
const NO_RAY_QUERIES: &str = "the device has no hardware ray queries: the browser's WebGPU \
    has none, and a native device has them where its adapter does and the game requested \
    graphics_device::ray_tracing_features";
const NO_MEMORY: &str = "the device's memory could not hold the scene's TLAS";

#[derive(Default)]
pub(crate) struct Prepare {
    /// The last rendered frame's hardware ray tracing.
    ray_tracing: RayTracingStats,
    /// The last rendered frame's rays trace in hardware: the scene holds
    /// its acceleration structures, prepared for it.
    hardware_rays: bool,
    /// Why the hardware path did not trace the last frame that asked for
    /// it, a frame with `Settings::hardware_ray_tracing` on.
    ray_tracing_error: Option<&'static str>,
}

impl Prepare {
    /// Uploads the view and frame data of `input` seen with `history` and
    /// `jitter` at `render_size`, and builds the frame's views. `history`'s
    /// camera carries `jitter`'s offset. Returns the data as uploaded.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &mut Scene,
        input: &FrameInput,
        history: HistoryFrame,
        effective: &Effective,
        jitter: Option<Jitter>,
        render_size: [u32; 2],
        views: &mut FrameViews,
        frame_buffer: &wgpu::Buffer,
    ) -> FrameValues {
        let camera = input.camera;
        let stable = history.stable;
        let mut view = ViewUniform {
            view: camera.view.to_cols_array_2d(),
            projection: camera.projection.to_cols_array_2d(),
            view_projection: stable.to_cols_array_2d(),
            inverse_view_projection: stable.inverse().to_cols_array_2d(),
            stable_view_projection: stable.to_cols_array_2d(),
            previous_view_projection: history.previous.to_cols_array_2d(),
            eye: camera.eye.to_array(),
            mip_bias: 0.,
            jitter: [0.; 2],
            viewport: [render_size[0] as f32, render_size[1] as f32],
            flags: 0,
            padding: [0; 3],
        };
        let shadow = FrameShadow::camera(
            input,
            effective.shadow_quality.cascade_size(),
            effective.shadow_filter,
            history.frames,
            scene.origin(),
        );
        let volume = scene
            .dynamic_gi_placement()
            .filter(|_| effective.dynamic_gi.is_some());
        let frame = frame_uniform(
            input,
            (&scene.static_lighting, scene.irradiance_volume()),
            &shadow,
            effective.fog.is_some(),
            volume.as_ref(),
        );
        views.reflection_camera = reflection_camera::Camera::new(camera.view, camera.projection);
        if let Some(jitter) = jitter {
            // As Diligent's `TemporalAntiAliasing::GetJitteredProjMatrix`
            // and FSR2's documented jitter translation apply it to the
            // projection, which the camera history records with the jitter
            // (`CameraFrame::jittered_projection`). Motion vectors stay
            // unjittered (`stable_view_projection`).
            let projection = history.camera.jittered_projection();
            let raster = projection * camera.view;
            view.view_projection = raster.to_cols_array_2d();
            view.inverse_view_projection = raster.inverse().to_cols_array_2d();
            // vertex.wgsl offsets clip xy by 2 · jitter · w.
            view.jitter = [jitter.ndc[0] / 2., jitter.ndc[1] / 2.];
            view.mip_bias = jitter.mip_bias;
            views.reflection_camera = reflection_camera::Camera::new(camera.view, projection);
        }
        let values = FrameValues { view, frame };
        crate::counters::write_buffer(queue, frame_buffer, 0, bytemuck::bytes_of(&frame));
        views.camera.set(queue, View::camera(view));
        scene.prepare_frame(device, queue, camera.eye);
        // Only world-space rays, the dynamic GI volume's and ray-traced
        // shadows read the ray instances and visibility mask. The entries
        // set and static edits made since the last traced frame wait for
        // the next, so the frames that skip it leave it nothing stale.
        let world_space = effective.world_space != WorldSpaceReflections::Off;
        let traced = world_space || volume.is_some() || effective.ray_traced_shadows;
        // The acceleration structures are built on the frames that trace,
        // and freed by a frame with hardware ray tracing off. The portable
        // BVHs then cover what the TLAS does not hold.
        self.ray_tracing = RayTracingStats::default();
        self.hardware_rays = false;
        match effective.hardware_ray_tracing {
            HardwareRayTracing::Off | HardwareRayTracing::Unsupported => {
                scene.free_acceleration_structures();
                self.ray_tracing_error = (effective.hardware_ray_tracing
                    == HardwareRayTracing::Unsupported)
                    .then_some(NO_RAY_QUERIES);
            }
            HardwareRayTracing::On(_) if traced => {
                let prepared = scene.prepare_acceleration_structures(device, queue, camera.eye);
                self.hardware_rays = prepared.is_some();
                self.ray_tracing = prepared.unwrap_or_default();
                self.ray_tracing_error = prepared.is_none().then_some(NO_MEMORY);
            }
            HardwareRayTracing::On(_) => scene.skip_acceleration_structures(),
        }
        if traced {
            scene.update_rays(device, queue, frame.visibility_mask, self.hardware_rays);
        }
        let scene = &*scene;
        views.clusters.cluster(
            device,
            queue,
            &scene.lights,
            effective.local_lights,
            &scene.decals,
            camera.view,
            camera.projection,
            render_size,
            CAMERA_CLUSTERS,
        );
        if world_space {
            // Ray hits shade with the lights and decals in the camera's
            // view, as Wicked Engine's take the frame's culled light list.
            let volume = ViewVolume::new(camera.projection * camera.view);
            views.ray_lists.list(
                device,
                queue,
                &scene.lights,
                effective.local_lights,
                |light| volume.reaches(light),
                &scene.decals,
                |decal| volume.reaches_decal(decal),
            );
        }
        if let Some(volume) = volume {
            // The probe rays' hits shade with the lights whose range reaches
            // the volume's extent and the decals that reach it.
            let extent = BoxVolume {
                min: volume.volume.origin,
                max: volume.volume.end(),
            };
            views.volume_lists.list(
                device,
                queue,
                &scene.lights,
                effective.local_lights,
                |light| extent.reaches(light),
                &scene.decals,
                |decal| extent.reaches_decal(decal),
            );
        }
        let mask = frame.visibility_mask;
        // The frame's CPU-built lists, these and the local-light shadow
        // faces', are built into its draw instances, which the renderer then
        // uploads.
        views.instances.clear();
        let camera_view = views.camera.view;
        let cull = effective.culling;
        let started = crate::counters::Moment::now();
        let camera_cull = camera_cull(&camera_view, render_size, mask, cull);
        views.camera.list.prepare(device, queue, scene, camera_cull);
        views.camera.built(started);
        let lod = LodSelector::new(camera.view, camera.projection, render_size);
        views.blended.build(
            &mut views.instances,
            scene,
            &camera_view,
            Some(mask),
            Population::Blended { lod, cull },
        );
        let cascades = shadow.cascades.as_slice().iter();
        set_cascades(
            (device, queue),
            scene,
            views,
            mask,
            cascades.map(|cascade| cascade.clip_from_world),
        );
        values
    }

    /// Records the frame's acceleration-structure builds that `run` chose,
    /// after the deform pass and before any pass traces.
    pub fn encode_acceleration_structures(&self, ctx: &mut FrameContext<'_>) {
        ctx.scene.encode_acceleration_structures(ctx.encoder);
    }

    /// The last rendered frame's hardware ray tracing.
    pub fn ray_tracing_stats(&self) -> RayTracingStats {
        self.ray_tracing
    }

    /// Whether the last rendered frame's rays trace in hardware.
    pub fn hardware_rays(&self) -> bool {
        self.hardware_rays
    }

    /// Why the hardware path did not trace the last frame that asked for it.
    pub fn ray_tracing_error(&self) -> Option<&'static str> {
        self.ray_tracing_error
    }

    /// The views of a probe capture at `center` with `input`'s lights, the
    /// directional shadow's cascades about `center` in maps of
    /// `cascade_size` texels and, when `local_lights`, the scene's lights,
    /// without their shadow, lit by the `dynamic_gi` volume whose probes
    /// group 0 binds. A capture has no fog: completion fogs what reflects
    /// it at the receiver.
    #[allow(clippy::too_many_arguments)]
    pub fn capture(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        input: &FrameInput,
        local_lights: bool,
        center: Vec3,
        cascade_size: u32,
        dynamic_gi: Option<&ProbePlacement>,
    ) -> CaptureViews {
        let shadow = FrameShadow::capture(input, center, cascade_size, scene.origin());
        let frame = frame_uniform(
            input,
            (&scene.static_lighting, scene.irradiance_volume()),
            &shadow,
            false,
            dynamic_gi,
        );
        let uniform = |label, bytes: &[u8]| {
            crate::scene::buffer(device, label, bytes, wgpu::BufferUsages::UNIFORM)
        };
        let frame_buffer = uniform("static capture frame", bytemuck::bytes_of(&frame));
        // Each face is a view.
        let views: Vec<_> = (0..6).map(|face| View::probe_face(center, face)).collect();
        let faces = views
            .iter()
            .map(|view| uniform("static capture camera", bytemuck::bytes_of(&view.uniform)))
            .collect();
        let mask = Some(frame.visibility_mask);
        let cascades: Vec<_> = shadow
            .cascades
            .as_slice()
            .iter()
            .map(|cascade| View::shadow_cascade(cascade.clip_from_world))
            .collect();
        // Every cascade draws the same unculled static content.
        let mut instances = DrawInstances::default();
        let mut casters = DrawList::default();
        if let Some(cascade) = cascades.first() {
            casters.build(
                &mut instances,
                scene,
                cascade,
                mask,
                Population::CaptureShadow,
            );
        }
        let cascades = cascades
            .iter()
            .map(|cascade| {
                uniform(
                    "static capture shadow cascade",
                    bytemuck::bytes_of(&cascade.uniform),
                )
            })
            .collect();
        // Every face sees the same unculled static content.
        let mut list = DrawList::default();
        list.build(
            &mut instances,
            scene,
            &views[0],
            mask,
            Population::ProbeFace,
        );
        let mut clusters = Clusters::new(device, "static capture lights and decals");
        clusters.list(
            device,
            queue,
            &scene.lights,
            local_lights,
            |_| true,
            &scene.decals,
            |_| true,
        );
        CaptureViews {
            frame: frame_buffer,
            faces,
            cascades,
            casters,
            list,
            instances,
            clusters,
        }
    }
}
