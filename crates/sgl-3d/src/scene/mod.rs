//! The scene layer: [`Scene`], the retained content a game adds, edits and
//! removes (materials, models, instances, lights, decals, environments,
//! baked lighting, probes, the irradiance volume, the dynamic GI volume's
//! placement and transient geometry), with the GPU buffers that mirror it
//! and the ray-query structures built from its geometry. Nothing here
//! depends on a camera, an output size or a quality setting, but the
//! hardware path's acceleration structures, which a frame asks the scene to
//! build while hardware ray tracing is in effect, nearest its camera first
//! (`rays::acceleration`).
pub(crate) mod candidates;
mod decal_atlas;
pub(crate) mod decals;
pub(crate) mod deformation;
pub(crate) mod dynamic_gi;
pub(crate) mod environments;
pub(crate) mod error;
pub(crate) mod geometry;
pub(crate) mod instances;
pub(crate) mod irradiance_volume;
mod lattice;
pub(crate) mod lights;
pub(crate) mod lod;
pub(crate) mod lookup_tables;
pub(crate) mod materials;
pub(crate) mod mesh_ranges;
pub(crate) mod models;
pub(crate) mod objects;
pub(crate) mod origin;
pub(crate) mod prepared;
pub(crate) mod probe_grid;
pub(crate) mod probes;
pub(crate) mod ranges;
pub(crate) mod ray_class;
pub(crate) mod rays;
pub(crate) mod shadow_clusters;
mod slots;
pub(crate) mod static_edits;
pub(crate) mod static_lighting;
mod textures;
pub(crate) mod transient;

pub use error::SceneError;
pub use prepared::PreparedModel;

use crate::content::identity::{Identity, MaterialId, ModelId};
use crate::content::instance::Mobility;
use std::sync::atomic::{AtomicU64, Ordering};

/// Retained content and the GPU buffers mirroring it, which a
/// [`Renderer`](crate::Renderer) renders. A scene starts empty; the game
/// adds, edits and removes content between frames, and each addition
/// returns the content's identity. Content only: rendering settings are
/// `settings::Settings` and each frame's camera and look are a
/// [`FrameInput`](crate::FrameInput). All dimensions are metres, Y-up.
pub struct Scene {
    pub(crate) materials: materials::Materials,
    pub(crate) models: models::Models,
    pub(crate) instances: instances::Instances,
    /// Each instance's draw candidates, with the sets and level chains they
    /// name, which the GPU draw lists cull.
    pub(crate) candidates: candidates::Candidates,
    pub(crate) lights: lights::Lights,
    pub(crate) decals: decals::Decals,
    pub(crate) environments: environments::Environments,
    /// Lit group 0's lookup tables (`lookup_tables`).
    pub(crate) lookup_tables: wgpu::TextureView,
    pub(crate) rays: rays::SceneRays,
    /// What shadow casters draw from vertex and index buffers.
    pub(crate) geometry: geometry::GeometryBuffers,
    /// The ray source's instance entries and instance BVHs.
    pub(crate) ray_instances: rays::instances::RayInstances,
    /// The hardware path's acceleration structures, while hardware ray
    /// tracing is in effect on a device that has it; none until a frame
    /// builds them and after one runs with the setting off.
    acceleration: Option<rays::acceleration::AccelerationStructures>,
    /// Group 1: the object records and the ray buffers.
    pub(crate) scene_group: wgpu::BindGroup,
    /// The buffers `scene_group` binds: objects, ray source, ray instances.
    bound: [wgpu::Buffer; 3],
    scene_layout: wgpu::BindGroupLayout,
    pub(crate) static_lighting: static_lighting::StaticLighting,
    baked_specular_probes: Option<probes::UploadedProbes>,
    /// The irradiance volume (`Scene::set_irradiance_volume`).
    pub(crate) irradiance_cells: irradiance_volume::IrradianceCells,
    /// The dynamic GI volume's placement (`Scene::set_dynamic_gi_volume`).
    pub(crate) dynamic_gi: Option<dynamic_gi::InstalledVolume>,
    /// This scene among every scene created: a renderer given another scene
    /// restarts its history.
    pub(crate) id: u64,
    /// Changes whenever a resource renderers bind in group 0 is replaced
    /// (lights, decals, the decal atlas, lightmap, irradiance atlas,
    /// specular probes, the irradiance volume's texture).
    pub(crate) resources: u64,
    /// Changes with every edit to the content the dynamic GI volume's rays
    /// see and light (`Scene::edited`): each public edit that changes
    /// something, but the transient effects' and a deforming instance's
    /// pose and deformation, which the portable path's rays do not see
    /// (`deformation_edits`). A converged volume pauses while it holds
    /// still.
    pub(crate) edits: u64,
    /// Changes with every edit of a capture-visible deforming instance's
    /// pose or deformation, which rays see while the hardware path traces
    /// them, so the dynamic GI volume's pause counts them then.
    pub(crate) deformation_edits: u64,
    /// Caller-authored glow, heat and mist geometry.
    pub(crate) transient: transient::Transient,
    /// The world bounds static edits touched since the last submitted frame.
    pub(crate) static_edits: static_edits::StaticEdits,
    /// The deformations the frame being rendered writes (the deform stage).
    pub(crate) deformations: Vec<crate::shading::deformation::DeformDispatch>,
    /// Where the render origin lies in the frame the scene was created in
    /// (`origin`).
    origin: glam::DVec3,
}

/// Group 1 (`shading::bind::scene`) over `objects`, `source` and
/// `instances`.
fn scene_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    [objects, source, instances]: &[wgpu::Buffer; 3],
) -> wgpu::BindGroup {
    use crate::shading::bind::group1;
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("scene objects and rays"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: group1::OBJECTS,
                resource: objects.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: group1::SCENE_SOURCE,
                resource: source.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: group1::SCENE_INSTANCES,
                resource: instances.as_entire_binding(),
            },
        ],
    })
}

impl Scene {
    /// An empty scene.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let scene_layout = crate::shading::bind::scene(device);
        let instances = instances::Instances::new(device);
        let rays = rays::SceneRays::new(device);
        let ray_instances = rays::instances::RayInstances::new(device);
        let bound = Self::buffers(&instances, &rays, &ray_instances);
        Self {
            materials: materials::Materials::new(device, queue),
            models: models::Models::default(),
            candidates: candidates::Candidates::new(&device.limits()),
            lights: lights::Lights::new(device),
            decals: decals::Decals::new(device, queue),
            environments: environments::Environments::new(device, queue),
            lookup_tables: lookup_tables::lookup_tables(device, queue),
            ray_instances,
            acceleration: None,
            scene_group: scene_group(device, &scene_layout, &bound),
            bound,
            scene_layout,
            instances,
            rays,
            geometry: geometry::GeometryBuffers::new(device),
            static_lighting: static_lighting::StaticLighting::empty(device, queue),
            baked_specular_probes: None,
            irradiance_cells: irradiance_volume::IrradianceCells::new(device),
            dynamic_gi: None,
            id: next_generation(),
            resources: next_generation(),
            edits: 0,
            deformation_edits: 0,
            transient: transient::Transient::new(device),
            static_edits: static_edits::StaticEdits::default(),
            deformations: Vec::new(),
            origin: glam::DVec3::ZERO,
        }
    }

    /// Records an edit to content the dynamic GI volume sees (`edits`).
    pub(crate) fn edited(&mut self) {
        self.edits = self.edits.wrapping_add(1);
    }

    /// Records an edit of a capture-visible deforming instance
    /// (`deformation_edits`).
    pub(crate) fn deformation_edited(&mut self) {
        self.deformation_edits = self.deformation_edits.wrapping_add(1);
    }

    fn buffers(
        instances: &instances::Instances,
        rays: &rays::SceneRays,
        ray_instances: &rays::instances::RayInstances,
    ) -> [wgpu::Buffer; 3] {
        [
            instances.objects.buffer().clone(),
            rays.source().clone(),
            ray_instances.buffer().clone(),
        ]
    }

    /// Rebuilds group 1 after content growth replaced a buffer it binds.
    fn refresh_scene_group(&mut self, device: &wgpu::Device) {
        let buffers = Self::buffers(&self.instances, &self.rays, &self.ray_instances);
        if buffers != self.bound {
            self.scene_group = scene_group(device, &self.scene_layout, &buffers);
            self.bound = buffers;
        }
    }

    /// A model an instance or level of detail draws, which lives while it
    /// does.
    pub(crate) fn drawn_model(&self, model: ModelId) -> &models::Model {
        self.models.slots.get(model).expect("a drawn model lives")
    }

    /// A material a mesh draws with, which lives while the mesh does.
    pub(crate) fn drawn_material(&self, material: MaterialId) -> &materials::Material {
        self.materials
            .slots
            .get(material)
            .expect("a mesh's material lives")
    }

    /// Before a frame: uploads a decal atlas packed since the last one and
    /// the draw candidates, sets and chains edits changed, rewrites records
    /// whose motion the last submitted frame ended, chooses the deformations
    /// the frame writes and shows, and orders the mist from `eye`.
    pub(crate) fn prepare_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        eye: glam::Vec3,
    ) {
        self.upload_decals(device, queue);
        self.candidates.upload(device, queue);
        self.instances
            .prepare_frame(queue, &self.models, &mut self.deformations);
        self.transient.sort_mist(queue, eye);
    }

    /// Before a traced frame: uploads the frame's visibility mask and the
    /// instance entries set since the last traced frame, and builds the
    /// instance BVHs over the capture-visible instances whose model has
    /// triangles and does not deform: the moving one every traced frame and
    /// the static one after a static edit or a change of the instances it
    /// covers. Where the frame traces in `hardware` (after
    /// `prepare_acceleration_structures`), they cover only the instances
    /// its TLAS does not hold (predicate instances under the baseline form,
    /// pending and left out; the architecture's Hardware ray tracing,
    /// *Portable coverage*), and
    /// none whose model rays pass through whole (`RayClass::None`). The
    /// portable path sees no deforming instance, as Bevy 9d12036's
    /// ray-traced scene leaves out meshes with joint attributes
    /// (crates/bevy_solari/src/scene/blas.rs, `is_mesh_raytracing_compatible`).
    pub(crate) fn update_rays(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        visibility_mask: u32,
        hardware: bool,
    ) {
        self.rays.set_visibility_mask(queue, visibility_mask);
        let tlas = self.acceleration.as_ref().filter(|_| hardware);
        let mut covered = Vec::new();
        let mut moving = Vec::new();
        for (id, instance) in self.instances.slots.iter() {
            if !instance.state.capture_visible || instance.deformation.is_some() {
                continue;
            }
            let model = self.drawn_model(instance.state.model);
            if model.ray.bvh_root == 0 {
                continue;
            }
            if let Some(tlas) = tlas
                && (tlas.holds(id.index()) || model.ray_class == ray_class::RayClass::None)
            {
                continue;
            }
            match instance.mobility {
                Mobility::Static => covered.push(id.index() as u32),
                Mobility::Moving => moving.push(rays::instances::bounded(
                    id.index(),
                    model.bounds,
                    instance.state.pose,
                )),
            }
        }
        let edits = self.static_edits.edits();
        let mut statics = Vec::new();
        let rebuild_statics = self.ray_instances.statics_stale(edits, &covered);
        if rebuild_statics {
            statics.extend(covered.iter().map(|&index| {
                let instance = self
                    .instances
                    .slots
                    .at(index as usize)
                    .expect("a covered instance lives");
                let model = self.drawn_model(instance.state.model);
                rays::instances::bounded(index as usize, model.bounds, instance.state.pose)
            }));
        }
        self.ray_instances.update(
            device,
            queue,
            &self.rays,
            rebuild_statics.then_some(rays::instances::StaticBuild {
                instances: statics.as_mut_slice(),
                edits,
                covered,
            }),
            &mut moving,
        );
    }

    /// Before a frame that builds the hardware path's acceleration
    /// structures for the query form `form`, seen from `eye`, and before
    /// `update_rays`, which covers what its TLAS does not hold: chooses its
    /// builds and sets the TLAS (`rays::acceleration`), creating the
    /// structures on the first such frame; none when the device's memory
    /// cannot hold them. The frame records the builds with
    /// `encode_acceleration_structures` after its deform pass, and
    /// `finish_frame` commits them.
    pub(crate) fn prepare_acceleration_structures(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        eye: glam::Vec3,
        form: crate::shading::RayQueryForm,
    ) -> Option<rays::acceleration::RayTracingStats> {
        if self.acceleration.is_none() {
            self.acceleration = rays::acceleration::AccelerationStructures::new(device);
        }
        let acceleration = self.acceleration.as_mut()?;
        Some(acceleration.prepare(
            device,
            queue,
            (&self.models, &self.instances),
            (self.ray_instances.capacity(), eye, form),
        ))
    }

    /// Records the frame's acceleration-structure builds, which
    /// `prepare_acceleration_structures` chose, into `encoder`.
    pub(crate) fn encode_acceleration_structures(&self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(acceleration) = &self.acceleration {
            acceleration.encode(encoder, self.rays.source());
        }
    }

    /// A frame that keeps the hardware path's acceleration structures but
    /// does not build them, tracing no rays: it records no builds.
    pub(crate) fn skip_acceleration_structures(&mut self) {
        if let Some(acceleration) = &mut self.acceleration {
            acceleration.skip_frame();
        }
    }

    /// Frees the hardware path's acceleration structures: a frame runs with
    /// hardware ray tracing off. A frame that turns it on again builds them
    /// anew.
    pub(crate) fn free_acceleration_structures(&mut self) {
        self.acceleration = None;
    }

    /// The hardware path's acceleration structures.
    pub(crate) fn acceleration_structures(
        &self,
    ) -> Option<&rays::acceleration::AccelerationStructures> {
        self.acceleration.as_ref()
    }

    /// Upload caller-generated additive geometry, growing the retained buffer when necessary.
    pub fn update_effects(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertices: &[crate::effects::Glow],
    ) {
        self.transient.update_glow(device, queue, vertices);
    }
    /// Replace the bounded heat triangle list; empty clears it. Invalid submissions
    /// return an error and retain the previous list. Camera-path only, before bloom/HUD.
    pub fn update_heat_distortion(
        &mut self,
        queue: &wgpu::Queue,
        vertices: &[crate::heat_distortion::HeatDistortion],
    ) -> Result<(), SceneError> {
        self.transient.update_heat(queue, vertices)
    }
    /// Replace the fog volumes (`FogVolume`), which add medium to the
    /// frame's fog where they lie; empty clears them. An invalid volume
    /// fails and keeps the previous ones.
    pub fn update_fog_volumes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        volumes: &[crate::FogVolume],
    ) -> Result<(), SceneError> {
        self.transient.update_fog_volumes(device, queue, volumes)
    }
    /// Replace the ground mist's world positions; empty clears it.
    pub fn update_mist(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        positions: &[[f32; 3]],
    ) {
        self.transient.update_mist(device, queue, positions);
    }
    /// The installed specular probe collection.
    pub(crate) fn specular_probes(&self) -> Option<&probes::UploadedProbes> {
        self.baked_specular_probes.as_ref()
    }
    /// Commits a submitted frame: each moving instance's pose becomes the
    /// one its motion is measured from, and its static edits are no longer
    /// pending. `Renderer::finish_frame` calls it.
    pub(crate) fn finish_frame(&mut self) {
        self.instances.finish_frame();
        self.static_edits.finish();
        if let Some(acceleration) = &mut self.acceleration {
            acceleration.finish_frame();
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
impl Scene {
    /// Group 1's layout.
    pub(crate) fn scene_layout(&self) -> &wgpu::BindGroupLayout {
        &self.scene_layout
    }

    /// The object records, as a storage binding.
    pub(crate) fn object_records(&self) -> wgpu::BindingResource<'_> {
        self.instances.objects.buffer().as_entire_binding()
    }
}

/// The GPU memory a scene's content holds (`Scene::diagnostic_resources`,
/// re-exported as `diagnostics::SceneResources`).
#[cfg(any(test, feature = "diagnostics"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneResources {
    /// The ray source buffer's bytes, the bytes up to the last word content
    /// holds, and the bytes content holds.
    pub ray_source: u64,
    pub ray_source_used: u64,
    pub ray_source_live: u64,
    /// The object records', instance entries' and light records' buffers.
    pub object_records: u64,
    pub instance_entries: u64,
    pub light_records: u64,
    /// The geometry buffers' bytes (models' positions, indices and caster
    /// clusters' indices), the bytes their content holds, and how many
    /// buffers there are.
    pub geometry: u64,
    pub geometry_live: u64,
    pub geometry_buffers: u64,
    /// The hardware path's BLASes, its models' and deforming instances',
    /// and the triangles they hold. wgpu 30 reports no acceleration
    /// structure's size.
    pub blases: u64,
    pub blas_triangles: u64,
    /// The GPU draw lists' buffers: the draw candidates', the draw sets'
    /// and the level chains' bytes, and the bytes the sets' regions take
    /// in each GPU-built view's cluster list, which holds them once for
    /// each phase it culls and, a cascade's, once more for its paired
    /// regions.
    pub draw_candidates: u64,
    pub draw_sets: u64,
    pub level_chains: u64,
    pub cluster_list: u64,
}

#[cfg(any(test, feature = "diagnostics"))]
impl Scene {
    /// The GPU memory the scene's content holds.
    pub fn diagnostic_resources(&self) -> SceneResources {
        let (ray_source_used, ray_source_live) = self.rays.words_in_use();
        let [geometry, geometry_live, geometry_buffers] = self.geometry.sizes();
        let (blases, blas_triangles) = self
            .acceleration
            .as_ref()
            .map_or((0, 0), |acceleration| acceleration.held());
        let [draw_candidates, draw_sets, level_chains] = self.candidates.bytes();
        SceneResources {
            ray_source: self.rays.source().size(),
            ray_source_used: ray_source_used * 4,
            ray_source_live: ray_source_live * 4,
            object_records: self.instances.objects.buffer().size(),
            instance_entries: self.ray_instances.buffer().size(),
            light_records: self.lights.buffer().size(),
            geometry,
            geometry_live,
            geometry_buffers,
            blases,
            blas_triangles,
            draw_candidates,
            draw_sets,
            level_chains,
            cluster_list: u64::from(self.candidates.sets.region_end())
                * std::mem::size_of::<crate::shading::vertex::DrawInstance>() as u64,
        }
    }
}

/// A value no other scene, resource or identity generation has.
pub(crate) fn next_generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg_attr(any(test, feature = "diagnostics"), track_caller)]
pub(crate) fn buffer(
    device: &wgpu::Device,
    label: &str,
    contents: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    crate::counters::buffer_init(
        device,
        &wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents,
            usage,
        },
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "content_tests.rs"]
mod content_tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod origin_tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod streaming_tests;
