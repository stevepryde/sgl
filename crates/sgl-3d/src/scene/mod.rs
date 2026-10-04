//! The scene layer: [`Scene`], the retained content a game adds, edits and
//! removes (materials, models, instances, lights, decals, environments,
//! baked lighting, probes and transient geometry), with the GPU buffers that
//! mirror it and the ray-query structure built from its geometry. Nothing here depends on a
//! camera, an output size or a quality setting.
mod decal_atlas;
pub(crate) mod decals;
pub(crate) mod deformation;
pub(crate) mod environments;
pub(crate) mod error;
pub(crate) mod instances;
pub(crate) mod lights;
pub(crate) mod lod;
pub(crate) mod lookup_tables;
pub(crate) mod materials;
pub(crate) mod mesh_ranges;
pub(crate) mod models;
pub(crate) mod objects;
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
use std::sync::atomic::{AtomicU64, Ordering};
use wgpu::util::DeviceExt;

/// Retained content and the GPU buffers mirroring it, which a
/// [`Renderer`](crate::Renderer) renders. A scene starts empty; the game
/// adds, edits and removes content between frames, and each addition
/// returns the content's identity. Content only: player choices are
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
    ray_instances: Vec<rays::SceneRayInstance>,
    /// Group 1: the object records and the ray buffers.
    pub(crate) scene_group: wgpu::BindGroup,
    /// The buffers `scene_group` binds: objects, ray source, ray instances.
    bound: [wgpu::Buffer; 3],
    scene_layout: wgpu::BindGroupLayout,
    pub(crate) static_lighting: static_lighting::StaticLighting,
    baked_specular_probes: Option<probes::UploadedProbes>,
    /// This scene among every scene created: a renderer given another scene
    /// restarts its history.
    pub(crate) id: u64,
    /// Changes whenever a resource renderers bind in group 0 is replaced
    /// (lights, decals, the decal atlas, lightmap, irradiance atlas,
    /// specular probes).
    pub(crate) resources: u64,
    /// Caller-authored glow, heat and mist geometry.
    pub(crate) transient: transient::Transient,
    /// The world bounds static edits touched since the last submitted frame.
    pub(crate) static_edits: static_edits::StaticEdits,
    /// The deformations the frame being rendered writes (the deform stage).
    pub(crate) deformations: Vec<crate::shading::deformation::DeformDispatch>,
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
        let bound = Self::buffers(&instances, &rays);
        Self {
            materials: materials::Materials::new(device, queue),
            models: models::Models::default(),
            lights: lights::Lights::new(device),
            decals: decals::Decals::new(device, queue),
            environments: environments::Environments::new(device, queue),
            lookup_tables: lookup_tables::lookup_tables(device, queue),
            ray_instances: Vec::new(),
            scene_group: scene_group(device, &scene_layout, &bound),
            bound,
            scene_layout,
            instances,
            rays,
            static_lighting: static_lighting::StaticLighting::empty(device, queue),
            baked_specular_probes: None,
            id: next_generation(),
            resources: next_generation(),
            transient: transient::Transient::new(device),
            static_edits: static_edits::StaticEdits::default(),
            deformations: Vec::new(),
        }
    }

    fn buffers(instances: &instances::Instances, rays: &rays::SceneRays) -> [wgpu::Buffer; 3] {
        let [source, ray_instances] = rays.buffers();
        [
            instances.objects.buffer().clone(),
            source.clone(),
            ray_instances.clone(),
        ]
    }

    /// Rebuilds group 1 after content growth replaced a buffer it binds.
    fn refresh_scene_group(&mut self, device: &wgpu::Device) {
        let buffers = Self::buffers(&self.instances, &self.rays);
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

    /// Uploads the instance list of the capture-visible instances that do
    /// not deform, in index order, and the frame's visibility mask. Rays see
    /// no deforming instance, as Bevy 9d12036's ray-traced scene leaves out
    /// meshes with joint attributes
    /// (crates/bevy_solari/src/scene/blas.rs, `is_mesh_raytracing_compatible`).
    pub(crate) fn update_rays(&mut self, queue: &wgpu::Queue, visibility_mask: u32) {
        let list = &mut self.ray_instances;
        list.clear();
        list.extend(
            self.instances
                .slots
                .iter()
                .filter(|(_, instance)| {
                    instance.state.capture_visible && instance.deformation.is_none()
                })
                .map(|(id, instance)| rays::SceneRayInstance {
                    baked_irradiance: instance.baked_irradiance,
                    model: self
                        .models
                        .slots
                        .get(instance.state.model)
                        .expect("an instance's model lives")
                        .ray,
                    world: instance.state.pose,
                    id: id.index() as u32,
                    flags: instance.flags(),
                }),
        );
        self.rays.set_visibility_mask(queue, visibility_mask);
        self.rays.update(queue, list);
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

/// A value no other scene, resource or identity generation has.
pub(crate) fn next_generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn buffer(
    device: &wgpu::Device,
    label: &str,
    contents: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents,
        usage,
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "content_tests.rs"]
mod content_tests;
