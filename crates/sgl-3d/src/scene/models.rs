//! Models: each one's meshes (their positions and indices in the scene's
//! geometry buffers, culling hierarchy, local-light shadow caster clusters,
//! levels of detail), its range of the ray source and a deforming model's
//! deformation, with the `Scene` operations that add, replace and remove
//! them and add a loaded asset whole.
use super::deformation::{InstanceDeformation, ModelDeformation};
use super::geometry::{Elements, GeometryBuffers, GeometryRange};
use super::materials::Materials;
use super::mesh_ranges::MeshRanges;
use super::prepared::{PreparedMesh, PreparedModel};
use super::ray_class::RayClass;
use super::rays::{RayMeshWords, RayModel, SceneRays};
use super::shadow_clusters::{MeshClusters, PosedClusters};
use super::slots::Slots;
use super::static_edits::posed_bounds;
use super::{Scene, SceneError};
use crate::asset::Asset;
use crate::content::identity::{Identity, MaterialId, ModelId};
use crate::content::model::{AssetIds, ModelMesh};
use crate::counters::{BuildStep, step};
use crate::lod::MeshLod;
use glam::Vec3;
use std::ops::Range;

pub(crate) struct Mesh {
    /// Its vertices' positions, which shadow casters read, and its indices.
    pub positions: GeometryRange,
    pub indices: GeometryRange,
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
    /// Where each mesh's vertices and indices lie in the ray source.
    pub ray_meshes: Vec<RayMeshWords>,
    /// What rays see of it, from its meshes' alpha modes, kept through its
    /// materials' use lists (`Models::classify_users`).
    pub ray_class: RayClass,
    /// Which of its meshes are masked, kept with `ray_class`.
    pub ray_masked: Vec<bool>,
    /// What deforms it; none when it is rigid.
    pub deformation: Option<ModelDeformation>,
    /// Instances showing it.
    pub instances: u32,
    /// Levels of detail naming one of its meshes.
    pub lod_uses: u32,
}

/// A prepared mesh's ranges of the geometry buffers, placed and not yet
/// written.
struct PlacedMesh {
    positions: GeometryRange,
    indices: GeometryRange,
    clusters: Option<GeometryRange>,
}

impl PlacedMesh {
    /// Places `mesh`'s positions, indices and caster clusters' indices in
    /// `geometry`, or nothing when one does not fit.
    fn place(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: &mut GeometryBuffers,
        mesh: &PreparedMesh,
    ) -> Result<Self, SceneError> {
        let mut placed = Self {
            positions: GeometryRange::EMPTY,
            indices: GeometryRange::EMPTY,
            clusters: None,
        };
        match placed.fill(device, queue, geometry, mesh) {
            Ok(()) => Ok(placed),
            Err(error) => {
                placed.free(geometry);
                Err(error)
            }
        }
    }

    fn fill(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: &mut GeometryBuffers,
        mesh: &PreparedMesh,
    ) -> Result<(), SceneError> {
        self.positions =
            geometry.place(device, queue, Elements::Positions, mesh.positions.len())?;
        self.indices = geometry.place(device, queue, Elements::Indices, mesh.indices.len())?;
        if let Some(clustered) = &mesh.clusters {
            let count = clustered.indices.len();
            self.clusters = Some(geometry.place(device, queue, Elements::Indices, count)?);
        }
        Ok(())
    }

    /// Copies `mesh`'s geometry to its ranges.
    fn write(&self, queue: &wgpu::Queue, geometry: &GeometryBuffers, mesh: &PreparedMesh) {
        geometry.write(queue, self.positions, bytemuck::cast_slice(&mesh.positions));
        geometry.write(queue, self.indices, bytemuck::cast_slice(&mesh.indices));
        if let (Some(range), Some(clustered)) = (self.clusters, &mesh.clusters) {
            geometry.write(queue, range, bytemuck::cast_slice(&clustered.indices));
        }
    }

    fn free(&self, geometry: &mut GeometryBuffers) {
        geometry.free(self.positions);
        geometry.free(self.indices);
        if let Some(clusters) = self.clusters {
            geometry.free(clusters);
        }
    }
}

impl Mesh {
    /// Frees its ranges of `geometry`.
    fn free(&self, geometry: &mut GeometryBuffers) {
        geometry.free(self.positions);
        geometry.free(self.indices);
        if let Some(clusters) = &self.clusters {
            geometry.free(clusters.indices);
        }
    }
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
    fn release(&mut self, materials: &mut Materials, model: ModelId, meshes: &[Mesh]) {
        for mesh in meshes {
            let material = materials
                .get_mut(mesh.material)
                .expect("a used material lives");
            let uses = material
                .users
                .get_mut(&model)
                .expect("a user of its material");
            *uses -= 1;
            if *uses == 0 {
                material.users.remove(&model);
            }
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

    /// Prepared `model` placed and written: a model which nothing uses yet
    /// and which does not use its materials yet. Placing it checks it
    /// against `materials` and the device, allocates its ranges and rebases
    /// the words that address them; on failure nothing stays placed.
    fn place(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        (rays, geometry): (&mut SceneRays, &mut GeometryBuffers),
        materials: &Materials,
        mut model: PreparedModel,
    ) -> Result<Model, SceneError> {
        let (words, deformation, placed) = step(BuildStep::Place, || {
            // Geometry beyond the device's limits fails where it is placed:
            // the ray source and the geometry slabs refuse what they cannot
            // hold.
            let mut material_words = Vec::with_capacity(model.meshes.len());
            for mesh in &model.meshes {
                let material = materials.get(mesh.material)?;
                validate_tangents(material.values.anisotropy_strength, mesh.tangents)?;
                material_words.push(material.word());
            }
            if model
                .deformation
                .as_ref()
                .is_some_and(|deformation| !deformation.dispatchable(device))
            {
                return Err(SceneError::DeviceLimit);
            }
            let words = rays.place_model(device, queue, &mut model.rays, &material_words)?;
            let vertices: Vec<u32> = words.meshes.iter().map(|mesh| mesh.vertices).collect();
            let deformation =
                match model.deformation.take().map(|prepared| {
                    ModelDeformation::place(device, queue, rays, prepared, &vertices)
                }) {
                    Some(Ok(placed)) => Some(placed),
                    Some(Err(error)) => {
                        rays.free(words.range);
                        return Err(error);
                    }
                    None => None,
                };
            let mut placed = Vec::with_capacity(model.meshes.len());
            for mesh in &model.meshes {
                match PlacedMesh::place(device, queue, geometry, mesh) {
                    Ok(mesh) => placed.push(mesh),
                    Err(error) => {
                        for mesh in &placed {
                            mesh.free(geometry);
                        }
                        if let Some((deformation, _)) = deformation {
                            deformation.free(rays);
                        }
                        rays.free(words.range);
                        return Err(error);
                    }
                }
            }
            Ok((words, deformation, placed))
        })?;
        step(BuildStep::Write, || {
            rays.write(queue, words.range.start, model.rays.words());
            if let Some((deformation, words)) = &deformation {
                rays.write(queue, deformation.word(), words);
            }
            for (placed, mesh) in placed.iter().zip(&model.meshes) {
                placed.write(queue, geometry, mesh);
            }
        });
        let meshes = model
            .meshes
            .into_iter()
            .zip(placed)
            .map(|(mesh, placed)| Mesh {
                positions: placed.positions,
                indices: placed.indices,
                count: mesh.indices.len() as u32,
                material: mesh.material,
                ranges: mesh.ranges,
                lods: Vec::new(),
                tangents: mesh.tangents,
                clusters: mesh
                    .clusters
                    .zip(placed.clusters)
                    .map(|(clustered, indices)| MeshClusters {
                        indices,
                        clusters: clustered.clusters,
                        groups: clustered.groups,
                    }),
            })
            .collect();
        let mut built = Model {
            bounds: model.bounds,
            meshes,
            geometry: super::next_generation(),
            ray: words.ray,
            ray_range: words.range,
            ray_meshes: words.meshes,
            ray_class: RayClass::None,
            ray_masked: Vec::new(),
            deformation: deformation.map(|(deformation, _)| deformation),
            instances: 0,
            lod_uses: 0,
        };
        super::ray_class::classify(&mut built, materials);
        Ok(built)
    }

    /// Frees a model's words in the ray source and its meshes' ranges of the
    /// geometry buffers.
    fn free(
        rays: &mut SceneRays,
        geometry: &mut GeometryBuffers,
        (range, deformation, meshes): (Range<u32>, Option<ModelDeformation>, &[Mesh]),
    ) {
        rays.free(range);
        if let Some(deformation) = deformation {
            deformation.free(rays);
        }
        for mesh in meshes {
            mesh.free(geometry);
        }
    }

    /// `model`'s `meshes` now use their materials.
    fn take_materials(materials: &mut Materials, model: ModelId, meshes: &[Mesh]) {
        for mesh in meshes {
            let material = materials
                .get_mut(mesh.material)
                .expect("a validated material");
            *material.users.entry(model).or_default() += 1;
            material.untangented += u32::from(!mesh.tangents);
        }
    }
}

impl Scene {
    /// Adds a prepared model (`PreparedModel::new`): an ordered list of
    /// meshes, each drawn with a material already in the scene. It only
    /// places the model's ranges and copies its geometry to the queue.
    pub fn add_model(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        model: PreparedModel,
    ) -> Result<ModelId, SceneError> {
        self.edited();
        let model = Models::place(
            device,
            queue,
            (&mut self.rays, &mut self.geometry),
            &self.materials,
            model,
        );
        self.refresh_scene_group(device);
        let id = self.models.slots.insert(model?);
        let meshes = &self
            .models
            .slots
            .get(id)
            .expect("a model just added")
            .meshes;
        Models::take_materials(&mut self.materials, id, meshes);
        Ok(id)
    }

    /// Replaces a model's whole geometry with a prepared model's, with any
    /// vertex and index counts, none included, and clears the levels of
    /// detail registered on it. A model that is a level of detail cannot be
    /// replaced.
    pub fn set_model(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: ModelId,
        model: PreparedModel,
    ) -> Result<(), SceneError> {
        self.edited();
        if self.models.get(id)?.in_use(true) {
            return Err(SceneError::ModelInUse);
        }
        if model.deformation.is_some() && self.instances.static_poses(id).next().is_some() {
            return Err(SceneError::DeformingModel);
        }
        let built = Models::place(
            device,
            queue,
            (&mut self.rays, &mut self.geometry),
            &self.materials,
            model,
        );
        // The moving instances showing it deform as the new geometry does,
        // from its bind pose.
        let posed = built.and_then(|built| {
            let meshes = self.candidate_meshes(id, &built);
            let deforms = Some(built.deformation.is_some());
            if !self.candidates_fit(&[(id, &meshes, deforms)]) {
                Models::free(
                    &mut self.rays,
                    &mut self.geometry,
                    (built.ray_range, built.deformation, &built.meshes),
                );
                return Err(SceneError::DeviceLimit);
            }
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
                            Models::free(
                                &mut self.rays,
                                &mut self.geometry,
                                (built.ray_range, built.deformation, &built.meshes),
                            );
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
        Models::take_materials(&mut self.materials, id, &built.meshes);
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
        model.ray_meshes = built.ray_meshes;
        model.ray_class = built.ray_class;
        model.ray_masked = built.ray_masked;
        let previous_range = std::mem::replace(&mut model.ray_range, built.ray_range);
        let previous_deformation = std::mem::replace(&mut model.deformation, built.deformation);
        self.models.release(&mut self.materials, id, &previous);
        Models::free(
            &mut self.rays,
            &mut self.geometry,
            (previous_range, previous_deformation, &previous),
        );
        // Its candidates name its new meshes; its alternatives are cleared.
        // Its instances' records say whether they deform now.
        self.candidates.remove_chains(id);
        self.place_candidates_of(&[id]);
        self.instances.write_of_model(queue, id);
        let model = self.models.get(id).unwrap();
        self.instances.pose_casters(model, id);
        // Rays see its instances' new geometry from their entries.
        for (instance, shown) in self.instances.slots.iter() {
            if shown.state.model == id {
                self.ray_instances
                    .set(instance.index(), model.ray, shown.state.pose);
            }
        }
        Ok(())
    }

    /// Removes a model no instance uses and that is no level of detail.
    pub fn remove_model(&mut self, id: ModelId) -> Result<(), SceneError> {
        self.edited();
        if self.models.get(id)?.in_use(false) {
            return Err(SceneError::ModelInUse);
        }
        let model = self.models.slots.remove(id).unwrap();
        self.candidates.remove_chains(id);
        self.models.release(&mut self.materials, id, &model.meshes);
        Models::free(
            &mut self.rays,
            &mut self.geometry,
            (model.ray_range, model.deformation, &model.meshes),
        );
        Ok(())
    }

    /// Adds a loaded asset whole: its materials and images, then its meshes,
    /// prepared here, as one model. Its own indices do not outlive this
    /// call. On failure nothing remains added.
    pub fn add_asset(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        asset: Asset,
    ) -> Result<AssetIds, SceneError> {
        self.edited();
        if asset
            .meshes
            .iter()
            .any(|mesh| mesh.material >= asset.materials.len())
        {
            return Err(SceneError::MissingMaterial);
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
        match PreparedModel::new(meshes).and_then(|model| self.add_model(device, queue, model)) {
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
