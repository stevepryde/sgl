//! Instances: each one's state, mobility, ambient cube, private motion
//! history and deformation, its object record and its ray entry, with the
//! `Scene` operations that add, read, change and remove them.
use super::deformation::InstanceDeformation;
use super::models::Model;
use super::objects::Objects;
use super::origin::translated;
use super::rays::instances::RayInstances;
use super::shadow_clusters::PosedClusters;
use super::slots::Slots;
use super::static_edits::posed_bounds;
use super::{Scene, SceneError};
use crate::content::identity::{Identity, InstanceId, ModelId};
use crate::content::instance::{InstanceState, Mobility};
use crate::shading::uniforms::{OBJECT_STATIC, ObjectUniform};
use crate::static_lighting::AmbientCube;
use glam::{Mat4, Vec3};

pub(crate) struct Instance {
    pub state: InstanceState,
    pub mobility: Mobility,
    pub baked_irradiance: AmbientCube,
    /// A static instance's caster clusters' and groups' world bounds, in
    /// its model's mesh order, which local-light shadow faces test; empty
    /// for a moving instance, which they test whole.
    pub casters: Vec<PosedClusters>,
    /// A moving instance's pose and model in the last submitted frame, when
    /// it was visible in it.
    submitted: Option<(Mat4, ModelId)>,
    /// Its written record's motion is from before the last submitted frame.
    stale: bool,
    /// Its pose of its model's deformation, when the model deforms.
    pub deformation: Option<InstanceDeformation>,
}

impl Instance {
    fn new(
        state: InstanceState,
        mobility: Mobility,
        model: &Model,
        deformation: Option<InstanceDeformation>,
    ) -> Self {
        let mut instance = Self {
            state,
            mobility,
            baked_irradiance: AmbientCube::default(),
            casters: Vec::new(),
            submitted: None,
            stale: false,
            deformation,
        };
        instance.pose_casters(model);
        instance
    }

    /// Places a static instance's caster bounds at its pose on `model`, its
    /// model's current geometry.
    fn pose_casters(&mut self, model: &Model) {
        if self.mobility == Mobility::Static {
            self.casters = model.caster_bounds(self.state.pose);
        }
    }

    pub fn flags(&self) -> u32 {
        match self.mobility {
            Mobility::Static => OBJECT_STATIC,
            Mobility::Moving => 0,
        }
    }

    /// The pose motion is measured from: a moving instance's pose in the
    /// last submitted frame while that pose applies, else its current pose,
    /// which writes no motion.
    fn previous_pose(&self) -> Mat4 {
        match (self.mobility, self.submitted) {
            (Mobility::Moving, Some((pose, model))) if model == self.state.model => pose,
            _ => self.state.pose,
        }
    }

    /// Its object record.
    fn record(&self) -> ObjectUniform {
        let [deformed_positions, previous_positions, deformed_normals] = self
            .deformation
            .as_ref()
            .map_or([0; 3], |deformation| deformation.record);
        ObjectUniform {
            model: self.state.pose.to_cols_array_2d(),
            previous_model: self.previous_pose().to_cols_array_2d(),
            baked_irradiance: self.baked_irradiance.packed(),
            flags: self.flags(),
            deformed_positions,
            previous_positions,
            deformed_normals,
        }
    }

    /// Its bounds in its model's space: as deformed, when it deforms.
    pub fn bounds(&self, model: &Model) -> [Vec3; 2] {
        self.deformation
            .as_ref()
            .map_or(model.bounds, |deformation| deformation.bounds)
    }

    /// Mesh `mesh`'s bounds in its model's space as the instance shows it.
    pub fn mesh_bounds(&self, model: &Model, mesh: usize) -> Option<[Vec3; 2]> {
        match &self.deformation {
            Some(deformation) => Some(deformation.mesh_bounds[mesh]),
            None => model.meshes[mesh].ranges.bounds(),
        }
    }
}

/// A pose a record and the ray source can take.
fn validate_pose(pose: Mat4) -> Result<(), SceneError> {
    if !pose.is_finite() || !pose.inverse().is_finite() {
        return Err(SceneError::InvalidPose);
    }
    Ok(())
}

pub(crate) struct Instances {
    pub slots: Slots<InstanceId, Instance>,
    pub objects: Objects,
    /// Live static and moving instances.
    counts: [usize; 2],
}

/// `Instances::counts`' index of a mobility.
fn kind(mobility: Mobility) -> usize {
    match mobility {
        Mobility::Static => 0,
        Mobility::Moving => 1,
    }
}

impl Instances {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            slots: Slots::default(),
            objects: Objects::new(device),
            counts: [0; 2],
        }
    }

    pub fn get(&self, id: InstanceId) -> Result<&Instance, SceneError> {
        self.slots.get(id).ok_or(SceneError::UnknownInstance)
    }

    fn write(&self, queue: &wgpu::Queue, id: InstanceId) {
        let instance = self.slots.get(id).expect("a live instance");
        self.objects.write(queue, id.index(), &instance.record());
    }

    /// Room for `count` records, rewriting every record when the buffer
    /// grows.
    fn reserve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        count: usize,
    ) -> Result<(), SceneError> {
        if self.objects.reserve(device, count)? {
            for (id, instance) in self.slots.iter() {
                self.objects.write(queue, id.index(), &instance.record());
            }
        }
        Ok(())
    }

    /// Places the caster bounds of the static instances of `model` (`id`)
    /// after its geometry was replaced.
    pub fn pose_casters(&mut self, model: &Model, id: ModelId) {
        for (_, instance) in self.slots.iter_mut() {
            if instance.state.model == id {
                instance.pose_casters(model);
            }
        }
    }

    /// The poses of the static instances of `model`.
    pub fn static_poses(&self, model: ModelId) -> impl Iterator<Item = Mat4> + '_ {
        self.slots
            .iter()
            .filter(move |(_, instance)| {
                instance.mobility == Mobility::Static && instance.state.model == model
            })
            .map(|(_, instance)| instance.state.pose)
    }

    /// Moves the render origin by `by` (`Scene::move_origin`): each
    /// instance's pose and the pose its motion is measured from, so a
    /// static instance still writes no motion and a moving one the same, a
    /// static instance's caster bounds, every object record in one write,
    /// and each rigid instance's ray entry. The write zeroes the records of
    /// removed indices below the highest live one, which no draw or ray
    /// reads until an instance reuses the index and writes its own.
    pub fn move_origin(
        &mut self,
        queue: &wgpu::Queue,
        by: Vec3,
        models: &super::models::Models,
        rays: &mut RayInstances,
    ) {
        let mut records = Vec::new();
        for (id, instance) in self.slots.iter_mut() {
            instance.state.pose = translated(instance.state.pose, by);
            if let Some((pose, _)) = &mut instance.submitted {
                *pose = translated(*pose, by);
            }
            let model = models
                .slots
                .get(instance.state.model)
                .expect("an instance's model lives");
            instance.pose_casters(model);
            rays.set(id.index(), model.ray, instance.state.pose);
            if records.len() <= id.index() {
                records.resize(id.index() + 1, bytemuck::Zeroable::zeroed());
            }
            records[id.index()] = instance.record();
        }
        self.objects.write_all(queue, &records);
    }

    /// Commits a submitted frame: each moving instance's pose and
    /// deformation become the ones its motion is measured from while it was
    /// visible.
    pub fn finish_frame(&mut self) {
        for (_, instance) in self.slots.iter_mut() {
            if let Some(deformation) = &mut instance.deformation {
                deformation.finish(instance.state.visible);
            }
            if instance.mobility == Mobility::Moving {
                let moved = instance.previous_pose() != instance.state.pose;
                instance.submitted = instance
                    .state
                    .visible
                    .then_some((instance.state.pose, instance.state.model));
                instance.stale |= moved;
            }
        }
    }

    /// Begins a frame: rewrites the records of moving instances not posed
    /// since the last submitted frame, whose motion it ended, and of
    /// deforming instances, whose deformed vertices this frame shows, and
    /// replaces `work` with the deformations the frame writes.
    pub fn prepare_frame(
        &mut self,
        queue: &wgpu::Queue,
        models: &super::models::Models,
        work: &mut Vec<crate::shading::deformation::DeformDispatch>,
    ) {
        work.clear();
        for (id, instance) in self.slots.iter_mut() {
            let deforms = if let Some(deformation) = &mut instance.deformation {
                let model = models.slots.get(instance.state.model);
                let model = model.and_then(|model| model.deformation.as_ref());
                deformation.prepare(model.expect("a deforming instance's model deforms"), work);
                true
            } else {
                false
            };
            if instance.stale || deforms {
                instance.stale = false;
                self.objects.write(queue, id.index(), &instance.record());
            }
        }
    }
}

impl Scene {
    /// Places a model. A static instance is expected to stay as added; a
    /// moving one to be posed every frame. Mobility is fixed for its life.
    /// An instance of a deforming model moves, and starts at its bind pose.
    pub fn add_instance(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        state: InstanceState,
        mobility: Mobility,
    ) -> Result<InstanceId, SceneError> {
        self.edited();
        let deforms = self.models.get(state.model)?.deformation.is_some();
        validate_pose(state.pose)?;
        if deforms && mobility == Mobility::Static {
            return Err(SceneError::DeformingModel);
        }
        let count = self.instances.slots.next_index() + 1;
        let kind_count = self.instances.counts[kind(mobility)] + 1;
        let reserved = self
            .ray_instances
            .reserve(device, queue, &mut self.rays, count, mobility, kind_count)
            .and_then(|_| self.instances.reserve(device, queue, count))
            .and_then(|_| {
                let model = self.models.get(state.model)?;
                model
                    .deformation
                    .as_ref()
                    .map(|model| InstanceDeformation::add(device, queue, &mut self.rays, model))
                    .transpose()
            });
        self.refresh_scene_group(device);
        let deformation = reserved?;
        let model = self.models.get_mut(state.model)?;
        model.instances += 1;
        if mobility == Mobility::Static {
            self.static_edits
                .record(posed_bounds(model.bounds, state.pose));
        }
        let id = self
            .instances
            .slots
            .insert(Instance::new(state, mobility, model, deformation));
        self.instances.counts[kind(mobility)] += 1;
        self.write_instance(queue, id);
        Ok(id)
    }

    /// Writes instance `id`'s object record and its ray entry, which the
    /// hardware path reads for a deforming instance too.
    fn write_instance(&mut self, queue: &wgpu::Queue, id: InstanceId) {
        self.instances.write(queue, id);
        let instance = self.instances.slots.get(id).expect("a live instance");
        let (ray, pose) = (
            self.drawn_model(instance.state.model).ray,
            instance.state.pose,
        );
        self.ray_instances.set(id.index(), ray, pose);
    }

    /// An instance's current state.
    pub fn instance(&self, id: InstanceId) -> Result<&InstanceState, SceneError> {
        Ok(&self.instances.get(id)?.state)
    }

    /// Replaces an instance's state. A state naming another model clears its
    /// ambient cube, and a moving instance then has no motion until a frame
    /// shows it. A deforming instance keeps its model, as does one of a
    /// rigid model with a deforming one: remove and add it instead.
    pub fn set_instance(
        &mut self,
        queue: &wgpu::Queue,
        id: InstanceId,
        state: InstanceState,
    ) -> Result<(), SceneError> {
        let previous = self.instances.get(id)?.state.model;
        let deforms = self.models.get(state.model)?.deformation.is_some();
        // A game may set every pose each frame: only a change rays see is
        // an edit, a deforming instance's only while the hardware path
        // traces it.
        let old = self.instances.get(id)?.state;
        if old != state && (old.capture_visible || state.capture_visible) {
            if deforms {
                self.deformation_edited();
            } else {
                self.edited();
            }
        }
        validate_pose(state.pose)?;
        if previous != state.model && (deforms || self.instances.get(id)?.deformation.is_some()) {
            return Err(SceneError::DeformingModel);
        }
        if previous != state.model {
            self.models.get_mut(previous)?.instances -= 1;
            self.models.get_mut(state.model)?.instances += 1;
        }
        let instance = self.instances.slots.get_mut(id).unwrap();
        if previous != state.model {
            instance.baked_irradiance = AmbientCube::default();
        }
        if instance.mobility == Mobility::Static && instance.state != state {
            let before = self.models.get(previous)?.bounds;
            let after = self.models.get(state.model)?.bounds;
            self.static_edits
                .record(posed_bounds(before, instance.state.pose));
            self.static_edits.record(posed_bounds(after, state.pose));
        }
        let placed = instance.state.pose != state.pose || previous != state.model;
        instance.state = state;
        instance.stale = false;
        if placed {
            instance.pose_casters(self.models.get(state.model)?);
        }
        self.write_instance(queue, id);
        Ok(())
    }

    /// Removes an instance. Its index is reused under a new identity.
    pub fn remove_instance(&mut self, id: InstanceId) -> Result<(), SceneError> {
        self.edited();
        let mut instance = self
            .instances
            .slots
            .remove(id)
            .ok_or(SceneError::UnknownInstance)?;
        if let Some(deformation) = instance.deformation.take() {
            deformation.free(&mut self.rays);
        }
        let model = self
            .models
            .get_mut(instance.state.model)
            .expect("an instance's model lives");
        model.instances -= 1;
        self.instances.counts[kind(instance.mobility)] -= 1;
        if instance.mobility == Mobility::Static {
            self.static_edits
                .record(posed_bounds(model.bounds, instance.state.pose));
        }
        Ok(())
    }

    /// Supplies a moving instance's interpolated baked field, in world axes.
    /// A state naming another model clears it. A static instance takes baked
    /// diffuse light from lightmap charts and the irradiance atlas instead,
    /// and is refused.
    pub fn set_instance_baked_irradiance(
        &mut self,
        queue: &wgpu::Queue,
        id: InstanceId,
        cube: AmbientCube,
    ) -> Result<(), SceneError> {
        self.edited();
        let instance = self
            .instances
            .slots
            .get_mut(id)
            .ok_or(SceneError::UnknownInstance)?;
        if instance.mobility == Mobility::Static {
            return Err(SceneError::StaticInstance);
        }
        instance.baked_irradiance = cube;
        self.instances
            .objects
            .write_baked_irradiance(queue, id.index(), cube);
        Ok(())
    }
}
