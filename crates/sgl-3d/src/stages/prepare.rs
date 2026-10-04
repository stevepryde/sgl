//! Prepare: the frame's uploads and views. It turns the frame's input into
//! the camera's view data and the frame's data (the directional shadow's
//! cascades, fit from the camera, baked lighting and fog, the
//! jitter antialiasing chose), uploads them, ends the motion of moving
//! instances not posed since the last submitted frame, sorts the mist,
//! updates the scene's ray instances on frames that trace them, clusters the
//! scene's lights and decals for the camera and culls them for ray hits, and builds the
//! camera's draw lists (culled, LOD-selected; its blended surfaces' sorted
//! back to front) and each directional shadow cascade's. The local-light shadow atlas places its own faces
//! (`shadows::local`). For a probe capture it builds the capture's own
//! views, cascades and light and decal list (`capture`).
//!
//! Reads: the frame input, the camera history and the scene. Writes:
//! `FrameViews` (view uniforms, draw lists, their draw instances and
//! clusters), the frame uniform, the scene's stale object records, ray
//! instances and mist order.
//! Honours: the effective local lights, temporal antialiasing (the shadow
//! filter), atmosphere, baked lighting, culling and world-space reflections.
//! Timing groups: none.
use crate::shading::uniforms::{FrameValues, ViewUniform};
use crate::view::clusters::{CAMERA_CLUSTERS, Clusters, ViewVolume};
use crate::view::draw_list::{DrawInstances, DrawList};
use crate::view::effective::Effective;
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
/// `clip_from_world`, nearest first, and builds each one's casters, which
/// are its own, independently of the main view's.
pub(crate) fn set_cascades(
    queue: &wgpu::Queue,
    scene: &Scene,
    views: &mut FrameViews,
    mask: Option<u32>,
    clip_from_world: impl ExactSizeIterator<Item = Mat4>,
) {
    views.cascade_count = clip_from_world.len();
    for (slot, clip_from_world) in views.cascades.iter_mut().zip(clip_from_world) {
        let view = View::shadow_cascade(clip_from_world);
        slot.set(queue, view);
        slot.list.build(
            &mut views.instances,
            scene,
            &view,
            mask,
            Population::DirectionalShadow {
                cull: true,
                moving: true,
            },
        );
    }
}

#[derive(Default)]
pub(crate) struct Prepare;

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
        let frame = frame_uniform(
            input,
            &scene.static_lighting,
            &shadow,
            effective.fog.is_some(),
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
        queue.write_buffer(frame_buffer, 0, bytemuck::bytes_of(&frame));
        views.camera.set(queue, View::camera(view));
        scene.prepare_frame(device, queue, camera.eye);
        // Only world-space rays read the ray instances and visibility mask.
        // The entries set and static edits made since the last traced frame
        // wait for the next, so the frames that skip it leave it nothing
        // stale.
        if effective.world_space {
            scene.update_rays(device, queue, frame.visibility_mask);
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
        if effective.world_space {
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
        let mask = Some(frame.visibility_mask);
        // The frame's lists, these and the local-light shadow faces', are
        // built into its draw instances, which the renderer then uploads.
        views.instances.clear();
        let camera_view = views.camera.view;
        let lod = LodSelector::new(camera.view, camera.projection, render_size);
        let cull = effective.culling;
        views.camera.list.build(
            &mut views.instances,
            scene,
            &camera_view,
            mask,
            Population::Camera { lod, cull },
        );
        views.blended.build(
            &mut views.instances,
            scene,
            &camera_view,
            mask,
            Population::Blended { lod, cull },
        );
        let cascades = shadow.cascades.as_slice().iter();
        set_cascades(
            queue,
            scene,
            views,
            mask,
            cascades.map(|cascade| cascade.clip_from_world),
        );
        values
    }

    /// The views of a probe capture at `center` with `input`'s lights, the
    /// directional shadow's cascades about `center` in maps of
    /// `cascade_size` texels and, when `local_lights`, the scene's lights,
    /// without their shadow. A capture has no fog: completion fogs what
    /// reflects it at the receiver.
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
    ) -> CaptureViews {
        let shadow = FrameShadow::capture(input, center, cascade_size, scene.origin());
        let frame = frame_uniform(input, &scene.static_lighting, &shadow, false);
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
                Population::DirectionalShadow {
                    cull: false,
                    moving: false,
                },
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
