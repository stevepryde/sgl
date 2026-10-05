//! The scene layer: [`Scene`], the retained content a game adds, edits and
//! removes (materials, models, instances, lights, decals, environments,
//! baked lighting, probes, the irradiance volume, the dynamic GI volume's
//! placement and transient geometry), with the GPU buffers that mirror it
//! and the ray-query structure built from its geometry. Nothing here
//! depends on a camera, an output size or a quality setting.
mod decal_atlas;
pub(crate) mod decals;
pub(crate) mod deformation;
pub(crate) mod dynamic_gi;
pub(crate) mod environments;
pub(crate) mod error;
pub(crate) mod geometry;
pub(crate) mod instances;
pub(crate) mod irradiance_volume;
pub(crate) mod lights;
pub(crate) mod lod;
pub(crate) mod lookup_tables;
pub(crate) mod materials;
pub(crate) mod mesh_ranges;
pub(crate) mod models;
pub(crate) mod objects;
pub(crate) mod origin;
pub(crate) mod probe_grid;
pub(crate) mod probes;
mod ranges;
pub(crate) mod rays;
pub(crate) mod shadow_clusters;
mod slots;
pub(crate) mod static_edits;
pub(crate) mod static_lighting;
mod textures;
pub(crate) mod transient;

pub use error::SceneError;

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
            lights: lights::Lights::new(device),
            decals: decals::Decals::new(device, queue),
            environments: environments::Environments::new(device, queue),
            lookup_tables: lookup_tables::lookup_tables(device, queue),
            ray_instances,
            scene_group: scene_group(device, &scene_layout, &bound),
            bound,
            scene_layout,
            instances,
            rays,
            geometry: geometry::GeometryBuffers::new(device),
            static_lighting: static_lighting::StaticLighting::empty(device, queue),
            baked_specular_probes: None,
            irradiance_cells: irradiance_volume::IrradianceCells::new(device, queue),
            dynamic_gi: None,
            id: next_generation(),
            resources: next_generation(),
            transient: transient::Transient::new(device),
            static_edits: static_edits::StaticEdits::default(),
            deformations: Vec::new(),
            origin: glam::DVec3::ZERO,
        }
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

    /// Before a frame: uploads a decal atlas packed since the last one,
    /// rewrites records whose motion the last submitted frame ended, chooses
    /// the deformations the frame writes and shows, and orders the mist from
    /// `eye`.
    pub(crate) fn prepare_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        eye: glam::Vec3,
    ) {
        self.upload_decals(device, queue);
        self.instances
            .prepare_frame(queue, &self.models, &mut self.deformations);
        self.transient.sort_mist(queue, eye);
    }

    /// Before a traced frame: uploads the frame's visibility mask and the
    /// instance entries set since the last traced frame, and builds the
    /// instance BVHs over the capture-visible instances whose model has
    /// triangles and does not deform: the moving one every traced frame and
    /// the static one after a static edit. Rays see no deforming instance,
    /// as Bevy 9d12036's ray-traced scene leaves out meshes with joint
    /// attributes (crates/bevy_solari/src/scene/blas.rs,
    /// `is_mesh_raytracing_compatible`).
    pub(crate) fn update_rays(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        visibility_mask: u32,
    ) {
        self.rays.set_visibility_mask(queue, visibility_mask);
        let edits = self.static_edits.edits();
        let rebuild_statics = self.ray_instances.statics_stale(edits);
        let mut statics = Vec::new();
        let mut moving = Vec::new();
        for (id, instance) in self.instances.slots.iter() {
            let rebuilt = match instance.mobility {
                Mobility::Static => rebuild_statics,
                Mobility::Moving => true,
            };
            if !rebuilt || !instance.state.capture_visible || instance.deformation.is_some() {
                continue;
            }
            let model = self.drawn_model(instance.state.model);
            if model.ray.bvh_root == 0 {
                continue;
            }
            let bounded = rays::instances::bounded(id.index(), model.bounds, instance.state.pose);
            match instance.mobility {
                Mobility::Static => statics.push(bounded),
                Mobility::Moving => moving.push(bounded),
            }
        }
        self.ray_instances.update(
            device,
            queue,
            &self.rays,
            rebuild_statics.then_some((statics.as_mut_slice(), edits)),
            &mut moving,
        );
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
}

#[cfg(any(test, feature = "diagnostics"))]
impl Scene {
    /// The GPU memory the scene's content holds.
    pub fn diagnostic_resources(&self) -> SceneResources {
        let (ray_source_used, ray_source_live) = self.rays.words_in_use();
        let [geometry, geometry_live, geometry_buffers] = self.geometry.sizes();
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
