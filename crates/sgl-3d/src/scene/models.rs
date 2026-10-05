//! Models: each one's meshes (position and index buffers, culling
//! hierarchy, local-light shadow caster clusters, levels of detail), its
//! range of the ray source and a deforming model's deformation, with the
//! `Scene` operations that add, replace and remove them and add a loaded
//! asset whole.
use super::deformation::{self, InstanceDeformation, ModelDeformation};
use super::materials::Materials;
use super::mesh_ranges::MeshRanges;
use super::rays::{RayMesh, RayModel, SceneRays};
use super::shadow_clusters::{MeshClusters, PosedClusters};
use super::slots::Slots;
use super::static_edits::posed_bounds;
use super::{Scene, SceneError, buffer};
use crate::asset::{self, Asset, Vertex};
use crate::content::identity::{Identity, MaterialId, ModelId};
use crate::content::model::{AssetIds, ModelMesh};
use crate::counters::{BuildStep, step};
use crate::lod::MeshLod;
use crate::shading::vertex::CasterVertex;
use glam::Vec3;
use std::ops::Range;

pub(crate) struct Mesh {
    /// Its vertices' positions, which shadow casters read.
    pub positions: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    pub count: u32,
    pub material: MaterialId,
    pub ranges: MeshRanges,
    pub lods: Vec<MeshLod>,
    /// Every vertex has an authored tangent frame.
    pub tangents: bool,
    /// None without triangles.
    pub clusters: Option<MeshClusters>,
}

pub(crate) struct Model {
    pub bounds: [Vec3; 2],
    pub meshes: Vec<Mesh>,
    /// Changes when its geometry is replaced.
    pub geometry: u64,
    pub ray: RayModel,
    ray_range: Range<u32>,
    /// What deforms it; none when it is rigid.
    pub deformation: Option<ModelDeformation>,
    /// Instances showing it.
    pub instances: u32,
    /// Levels of detail naming one of its meshes.
    pub lod_uses: u32,
}

impl Model {
    /// Each mesh's caster clusters' and groups' world bounds at `pose`, in
    /// mesh order.
    pub fn caster_bounds(&self, pose: glam::Mat4) -> Vec<PosedClusters> {
        self.meshes
            .iter()
            .map(|mesh| {
                mesh.clusters
                    .as_ref()
                    .map_or_else(PosedClusters::default, |clusters| clusters.posed(pose))
            })
            .collect()
    }

    /// Whether it may be removed, or replaced (`replacing`).
    fn in_use(&self, replacing: bool) -> bool {
        self.lod_uses > 0 || (!replacing && self.instances > 0)
    }
}

/// `vertices` and `indices` as a mesh: indices name vertices and positions
/// are finite. Returns whether every vertex has an authored tangent frame.
fn validate_geometry(vertices: &[Vertex], indices: &[u32]) -> Result<bool, SceneError> {
    if indices
        .iter()
        .any(|&index| index as usize >= vertices.len())
    {
        return Err(SceneError::IndexOutOfRange);
    }
    if !vertices
        .iter()
        .all(|vertex| vertex.position.iter().all(|x| x.is_finite()))
    {
        return Err(SceneError::NonFiniteGeometry);
    }
    Ok(asset::tangent_frames(vertices))
}

/// A mesh with `tangents` drawn with a material of `anisotropy` strength.
fn validate_tangents(anisotropy: f32, tangents: bool) -> Result<(), SceneError> {
    if anisotropy > 0. && !tangents {
        return Err(SceneError::MissingAnisotropyTangents);
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct Models {
    pub slots: Slots<ModelId, Model>,
}

impl Models {
    pub fn get(&self, id: ModelId) -> Result<&Model, SceneError> {
        self.slots.get(id).ok_or(SceneError::UnknownModel)
    }

    pub fn get_mut(&mut self, id: ModelId) -> Result<&mut Model, SceneError> {
        self.slots.get_mut(id).ok_or(SceneError::UnknownModel)
    }

    /// Whether a model deforms.
    pub fn holds_deforming(&self) -> bool {
        self.slots
            .iter()
            .any(|(_, model)| model.deformation.is_some())
    }

    /// Whether `lod`'s mesh has authored tangent frames.
    pub fn lod_tangents(&self, lod: &MeshLod) -> bool {
        self.slots
            .get(lod.model)
            .expect("a level of detail's model lives")
            .meshes[lod.mesh]
            .tangents
    }

    /// Ends `model`'s uses of other content: its levels of detail's models
    /// and its meshes' materials.
    fn release(&mut self, materials: &mut Materials, meshes: &[Mesh]) {
        for mesh in meshes {
            let material = materials
                .get_mut(mesh.material)
                .expect("a used material lives");
            material.users -= 1;
            material.untangented -= u32::from(!mesh.tangents);
        }
        for mesh in meshes {
            self.clear_lods(materials, mesh.material, &mesh.lods);
        }
    }

    /// Ends `lods`' uses of their models and of `material`'s tangent count.
    pub fn clear_lods(
        &mut self,
        materials: &mut Materials,
        material: MaterialId,
        lods: &[MeshLod],
    ) {
        for lod in lods {
            let untangented = !self.lod_tangents(lod);
            self.slots
                .get_mut(lod.model)
                .expect("a level of detail's model lives")
                .lod_uses -= 1;
            materials
                .get_mut(material)
                .expect("a used material lives")
                .untangented -= u32::from(untangented);
        }
    }

    /// A model of validated, uploaded meshes and their ray source range,
    /// which nothing uses yet and which does not use its materials yet.
    fn build(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &mut SceneRays,
        materials: &Materials,
        meshes: Vec<ModelMesh>,
    ) -> Result<Model, SceneError> {
        let limit = device.limits().max_buffer_size;
        let mut tangents = Vec::with_capacity(meshes.len());
        let mut ray_meshes = Vec::with_capacity(meshes.len());
        for mesh in &meshes {
            let material = materials.get(mesh.material)?;
            let has_tangents = step(BuildStep::Validate, || {
                validate_geometry(&mesh.vertices, &mesh.indices)
            })?;
            validate_tangents(material.values.anisotropy_strength, has_tangents)?;
            if std::mem::size_of_val(mesh.vertices.as_slice()) as u64 > limit
                || std::mem::size_of_val(mesh.indices.as_slice()) as u64 > limit
            {
                return Err(SceneError::DeviceLimit);
            }
            tangents.push(has_tangents);
            ray_meshes.push(RayMesh {
                vertices: &mesh.vertices,
                indices: &mesh.indices,
                material_word: material.word(),
            });
        }
        deformation::validate(device, &meshes)?;
        let words = rays.add_model(device, queue, &ray_meshes)?;
        let deformation = match ModelDeformation::add(device, queue, rays, &meshes, &words.vertices)
        {
            Ok(deformation) => deformation,
            Err(error) => {
                rays.free(words.range);
                return Err(error);
            }
        };
        let (ray, ray_range) = (words.ray, words.range);
        let bounds = meshes.iter().flat_map(|mesh| &mesh.vertices).fold(
            [Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)],
            |bounds, vertex| {
                let p = Vec3::from_array(vertex.position);
                [bounds[0].min(p), bounds[1].max(p)]
            },
        );
        let meshes = meshes
            .iter()
            .zip(tangents)
            .map(|(mesh, tangents)| {
                let (positions, indices) = step(BuildStep::MeshBuffers, || {
                    let positions: Vec<_> = mesh
                        .vertices
                        .iter()
                        .map(|vertex| CasterVertex {
                            position: vertex.position,
                        })
                        .collect();
                    (
                        buffer(
                            device,
                            "mesh positions",
                            bytemuck::cast_slice(&positions),
                            wgpu::BufferUsages::VERTEX,
                        ),
                        buffer(
                            device,
                            "mesh indices",
                            bytemuck::cast_slice(&mesh.indices),
                            wgpu::BufferUsages::INDEX,
                        ),
                    )
                });
                Mesh {
                    positions,
                    indices,
                    count: mesh.indices.len() as u32,
                    material: mesh.material,
                    ranges: step(BuildStep::Ranges, || {
                        MeshRanges::new(&mesh.vertices, &mesh.indices)
                    }),
                    lods: Vec::new(),
                    tangents,
                    clusters: step(BuildStep::Clusters, || {
                        MeshClusters::new(device, &mesh.vertices, &mesh.indices)
                    }),
                }
            })
            .collect();
        Ok(Model {
            bounds,
            meshes,
            geometry: super::next_generation(),
            ray,
            ray_range,
            deformation,
            instances: 0,
            lod_uses: 0,
        })
    }

    /// Frees a model's words in the ray source.
    fn free(rays: &mut SceneRays, range: Range<u32>, deformation: Option<ModelDeformation>) {
        rays.free(range);
        if let Some(deformation) = deformation {
            deformation.free(rays);
        }
    }

    /// `meshes` now use their materials.
    fn take_materials(materials: &mut Materials, meshes: &[Mesh]) {
        for mesh in meshes {
            let material = materials
                .get_mut(mesh.material)
                .expect("a validated material");
            material.users += 1;
            material.untangented += u32::from(!mesh.tangents);
        }
    }
}

impl Scene {
    /// Adds a model: an ordered list of meshes, each drawn with a material
    /// already in the scene.
    pub fn add_model(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        meshes: Vec<ModelMesh>,
    ) -> Result<ModelId, SceneError> {
        let model = Models::build(device, queue, &mut self.rays, &self.materials, meshes);
        self.refresh_scene_group(device);
        let model = model?;
        Models::take_materials(&mut self.materials, &model.meshes);
        Ok(self.models.slots.insert(model))
    }

    /// Replaces a model's whole geometry, with any vertex and index counts,
    /// none included, and clears the levels of detail registered on it. A
    /// model that is a level of detail cannot be replaced.
    pub fn set_model(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: ModelId,
        meshes: Vec<ModelMesh>,
    ) -> Result<(), SceneError> {
        if self.models.get(id)?.in_use(true) {
            return Err(SceneError::ModelInUse);
        }
        let deforms = meshes.iter().any(|mesh| !mesh.deformation.is_rigid());
        if deforms && self.instances.static_poses(id).next().is_some() {
            return Err(SceneError::DeformingModel);
        }
        let built = Models::build(device, queue, &mut self.rays, &self.materials, meshes);
        // The moving instances showing it deform as the new geometry does,
        // from its bind pose.
        let posed = built.and_then(|built| {
            let mut posed = Vec::new();
            if let Some(deformation) = &built.deformation {
                for (instance, _) in self
                    .instances
                    .slots
                    .iter()
                    .filter(|(_, i)| i.state.model == id)
                {
                    match InstanceDeformation::add(device, queue, &mut self.rays, deformation) {
                        Ok(added) => posed.push((instance, added)),
                        Err(error) => {
                            for (_, added) in posed {
                                InstanceDeformation::free(added, &mut self.rays);
                            }
                            Models::free(&mut self.rays, built.ray_range, built.deformation);
                            return Err(error);
                        }
                    }
                }
            }
            Ok((built, posed))
        });
        self.refresh_scene_group(device);
        let (built, posed) = posed?;
        for (_, instance) in self.instances.slots.iter_mut() {
            if instance.state.model == id
                && let Some(previous) = instance.deformation.take()
            {
                previous.free(&mut self.rays);
            }
        }
        for (instance, deformation) in posed {
            self.instances.slots.get_mut(instance).unwrap().deformation = Some(deformation);
        }
        Models::take_materials(&mut self.materials, &built.meshes);
        let model = self.models.get_mut(id).unwrap();
        // Replacing the geometry a static instance shows is a static edit.
        for pose in self.instances.static_poses(id) {
            self.static_edits.record(posed_bounds(model.bounds, pose));
            self.static_edits.record(posed_bounds(built.bounds, pose));
        }
        let previous = std::mem::replace(&mut model.meshes, built.meshes);
        model.bounds = built.bounds;
        model.geometry = built.geometry;
        model.ray = built.ray;
        let previous_range = std::mem::replace(&mut model.ray_range, built.ray_range);
        let previous_deformation = std::mem::replace(&mut model.deformation, built.deformation);
        self.models.release(&mut self.materials, &previous);
        Models::free(&mut self.rays, previous_range, previous_deformation);
        let model = self.models.get(id).unwrap();
        self.instances.pose_casters(model, id);
        // Rays see its instances' new geometry from their entries.
        for (instance, shown) in self.instances.slots.iter() {
            if shown.state.model == id && shown.deformation.is_none() {
                self.ray_instances
                    .set(instance.index(), model.ray, shown.state.pose);
            }
        }
        Ok(())
    }

    /// Removes a model no instance uses and that is no level of detail.
    pub fn remove_model(&mut self, id: ModelId) -> Result<(), SceneError> {
        if self.models.get(id)?.in_use(false) {
            return Err(SceneError::ModelInUse);
        }
        let model = self.models.slots.remove(id).unwrap();
        self.models.release(&mut self.materials, &model.meshes);
        Models::free(&mut self.rays, model.ray_range, model.deformation);
        Ok(())
    }

    /// Adds a loaded asset whole: its materials and images, then its meshes
    /// as one model. Its own indices do not outlive this call. On failure
    /// nothing remains added.
    pub fn add_asset(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        asset: Asset,
    ) -> Result<AssetIds, SceneError> {
        for mesh in &asset.meshes {
            let material = asset
                .materials
                .get(mesh.material)
                .ok_or(SceneError::MissingMaterial)?;
            let tangents = validate_geometry(&mesh.vertices, &mesh.indices)?;
            validate_tangents(material.anisotropy_strength, tangents)?;
        }
        let materials = self.add_materials(device, queue, &asset.materials, &asset.images)?;
        let meshes = asset
            .meshes
            .into_iter()
            .map(|mesh| ModelMesh {
                vertices: mesh.vertices,
                indices: mesh.indices,
                material: materials[mesh.material],
                deformation: mesh.deformation,
            })
            .collect();
        match self.add_model(device, queue, meshes) {
            Ok(model) => Ok(AssetIds { materials, model }),
            Err(error) => {
                for &material in &materials {
                    self.remove_material(material)
                        .expect("a material just added has no users");
                }
                Err(error)
            }
        }
    }
}
