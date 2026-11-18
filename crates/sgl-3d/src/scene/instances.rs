//! Instances: each one's state, mobility, ambient cube, private motion
//! history and deformation, its object record and its ray entry, with the
//! `Scene` operations that add, read, change and remove them.
use super::deformation::InstanceDeformation;
use super::models::Model;
use super::objects::{Objects, ShaderData};
use super::origin::translated;
use super::rays::instances::RayInstances;
use super::shadow_clusters::PosedClusters;
use super::slots::Slots;
use super::static_edits::posed_bounds;
use super::{Scene, SceneError};
use crate::content::identity::{Identity, InstanceId, ModelId};
use crate::content::instance::{InstanceState, Mobility};
use crate::shading::uniforms::{
    OBJECT_CAPTURE_VISIBLE, OBJECT_DEFORMING, OBJECT_STATIC, OBJECT_VISIBLE, ObjectUniform,
};
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
    /// The data its materials' shaders read (`Scene::set_instance_shader_data`),
    /// and as it was in the last submitted frame, none before it was in one.
    pub shader_data: [f32; 4],
    submitted_shader_data: Option<[f32; 4]>,
    /// Its written shader data's last submitted frame's is from before the
    /// last submitted frame.
    shader_stale: bool,
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
            shader_data: [0.; 4],
            submitted_shader_data: None,
            shader_stale: false,
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

    /// Its object record's bits: static, `visible`, `capture_visible` and
    /// deforming, which the GPU draw lists' cull reads.
    pub fn flags(&self) -> u32 {
        let bit = |on: bool, bit: u32| if on { bit } else { 0 };
        bit(self.mobility == Mobility::Static, OBJECT_STATIC)
            | bit(self.state.visible, OBJECT_VISIBLE)
            | bit(self.state.capture_visible, OBJECT_CAPTURE_VISIBLE)
            | bit(self.deformation.is_some(), OBJECT_DEFORMING)
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

    /// Its shader data, this frame's and the last submitted frame's, from
    /// which its shaders' motion is measured: this frame's before it was in
    /// one.
    fn shader_record(&self) -> ShaderData {
        [
            self.shader_data,
            self.submitted_shader_data.unwrap_or(self.shader_data),
        ]
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
    /// Changes since the last submitted frame that no motion vector
    /// describes (M-9): appearances, departures, showing or hiding, model
    /// and geometry replacement, a static instance moved, a moving one
    /// moved where its last submitted pose does not apply, and material
    /// edits that change what their users draw (`Scene::set_material`). A
    /// moving instance's move from its submitted pose, deformation and
    /// vertex functions write motion and do not set it.
    rigid_changes: bool,
    /// The instances whose shader data was set since the last submitted
    /// frame (`set_shader_data`), repeats and removed ones included.
    shader_edits: Vec<InstanceId>,
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
            rigid_changes: false,
            shader_edits: Vec::new(),
        }
    }

    pub fn get(&self, id: InstanceId) -> Result<&Instance, SceneError> {
        self.slots.get(id).ok_or(SceneError::UnknownInstance)
    }

    fn write(&self, queue: &wgpu::Queue, id: InstanceId) {
        let instance = self.slots.get(id).expect("a live instance");
        self.objects.write(queue, id.index(), &instance.record());
    }

    fn write_shader_data(&self, queue: &wgpu::Queue, id: InstanceId) {
        let instance = self.slots.get(id).expect("a live instance");
        self.objects
            .write_shader_data(queue, id.index(), &instance.shader_record());
    }

    /// Sets instance `id`'s shader data and writes it.
    pub fn set_shader_data(&mut self, queue: &wgpu::Queue, id: InstanceId, data: [f32; 4]) {
        self.slots.get_mut(id).expect("a live instance").shader_data = data;
        self.shader_edits.push(id);
        self.write_shader_data(queue, id);
    }

    /// The live instances whose shader data differs from the last
    /// submitted frame's, each once or more; none that was in no submitted
    /// frame, which has no last submitted data.
    pub fn shader_data_edited(&self) -> impl Iterator<Item = &Instance> + '_ {
        self.shader_edits
            .iter()
            .filter_map(|&id| self.slots.get(id))
            .filter(|instance| {
                instance
                    .submitted_shader_data
                    .is_some_and(|submitted| submitted != instance.shader_data)
            })
    }

    /// Geometry replacement, or a material edit that changes what `model`'s
    /// meshes draw, can uncover old pixels without any pose motion.
    /// Include prior visible users even when the new geometry/position is culled.
    pub fn geometry_changed(&mut self, model: ModelId) {
        self.rigid_changes |= self.slots.iter().any(|(_, instance)| {
            instance.state.model == model
                && (instance.state.visible || instance.submitted.is_some())
        });
    }

    /// Rewrites the object records of `model`'s instances.
    pub fn write_of_model(&self, queue: &wgpu::Queue, model: ModelId) {
        for (id, instance) in self.slots.iter() {
            if instance.state.model == model {
                self.objects.write(queue, id.index(), &instance.record());
            }
        }
    }

    /// Room for `count` records, rewriting every record and every instance's
    /// shader data when the buffers grow.
    fn reserve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        count: usize,
    ) -> Result<(), SceneError> {
        if self.objects.reserve(device, count)? {
            for (id, instance) in self.slots.iter() {
                self.objects.write(queue, id.index(), &instance.record());
                self.objects
                    .write_shader_data(queue, id.index(), &instance.shader_record());
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
    /// visible, and each instance's shader data the one its shaders'
    /// motion is.
    pub fn finish_frame(&mut self) {
        self.rigid_changes = false;
        self.shader_edits.clear();
        for (_, instance) in self.slots.iter_mut() {
            if instance.submitted_shader_data != Some(instance.shader_data) {
                instance.shader_stale |= instance.submitted_shader_data.is_some();
                instance.submitted_shader_data = Some(instance.shader_data);
            }
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
    /// the shader data of instances whose data the last submitted frame
    /// changed, and replaces `work` with the deformations the frame writes.
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
            if instance.shader_stale {
                instance.shader_stale = false;
                self.objects
                    .write_shader_data(queue, id.index(), &instance.shader_record());
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
        let index = self.instances.slots.next_index();
        let meshes = self.candidate_meshes(state.model, self.models.get(state.model)?);
        let deformed = deformation
            .as_ref()
            .map(|deformation| deformation.mesh_bounds.as_slice());
        let mirrored = state.pose.determinant() < 0.;
        if let Err(error) = self
            .candidates
            .place(index, (state.model, &meshes), mirrored, deformed)
        {
            if let Some(deformation) = deformation {
                deformation.free(&mut self.rays);
            }
            return Err(error);
        }
        let shaded = self.shaded_bounds(self.models.get(state.model)?);
        let model = self.models.get_mut(state.model)?;
        model.instances += 1;
        if mobility == Mobility::Static {
            self.static_edits.record(posed_bounds(shaded, state.pose));
        }
        let id = self
            .instances
            .slots
            .insert(Instance::new(state, mobility, model, deformation));
        self.instances.counts[kind(mobility)] += 1;
        self.instances.rigid_changes |= state.visible;
        self.write_instance(queue, id);
        // Its index may be a removed instance's.
        self.instances.write_shader_data(queue, id);
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

    /// Changes without a motion vector, visible now or before their
    /// departure (M-9). Deliberately conservative across frustum and
    /// occlusion culling: a culled new state can still expose history at
    /// the old one. Clears only on submit.
    pub(crate) fn rigid_objects_changed(&self) -> bool {
        self.instances.rigid_changes
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
        // Its candidates name its model's meshes and, by whether its pose
        // mirrors, their sets.
        let mirrored = |pose: glam::Mat4| pose.determinant() < 0.;
        if previous != state.model || mirrored(old.pose) != mirrored(state.pose) {
            let meshes = self.candidate_meshes(state.model, self.models.get(state.model)?);
            let deformed = self
                .instances
                .get(id)?
                .deformation
                .as_ref()
                .map(|deformation| deformation.mesh_bounds.as_slice());
            self.candidates.place(
                id.index(),
                (state.model, &meshes),
                mirrored(state.pose),
                deformed,
            )?;
        }
        if previous != state.model {
            self.models.get_mut(previous)?.instances -= 1;
            self.models.get_mut(state.model)?.instances += 1;
        }
        let before = self.shaded_bounds(self.models.get(previous)?);
        let after = self.shaded_bounds(self.models.get(state.model)?);
        let instance = self.instances.slots.get_mut(id).unwrap();
        if previous != state.model {
            instance.baked_irradiance = AmbientCube::default();
        }
        if instance.mobility == Mobility::Static && instance.state != state {
            self.static_edits
                .record(posed_bounds(before, instance.state.pose));
            self.static_edits.record(posed_bounds(after, state.pose));
        }
        let placed = instance.state.pose != state.pose || previous != state.model;
        // A moving instance whose last submitted pose applies to the new
        // state writes its move as motion, which TAA follows per pixel; any
        // other change has no motion vector and re-arms the view (M-9).
        let motion_describes_it = instance.mobility == Mobility::Moving
            && state.visible
            && instance
                .submitted
                .is_some_and(|(_, model)| model == state.model);
        self.instances.rigid_changes |= (old.visible || state.visible)
            && (old.visible != state.visible || (placed && !motion_describes_it));
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
        self.instances.rigid_changes |= instance.state.visible || instance.submitted.is_some();
        self.candidates.remove(id.index());
        if let Some(deformation) = instance.deformation.take() {
            deformation.free(&mut self.rays);
        }
        let shaded = self.shaded_bounds(self.drawn_model(instance.state.model));
        let model = self
            .models
            .get_mut(instance.state.model)
            .expect("an instance's model lives");
        model.instances -= 1;
        self.instances.counts[kind(instance.mobility)] -= 1;
        if instance.mobility == Mobility::Static {
            self.static_edits
                .record(posed_bounds(shaded, instance.state.pose));
        }
        Ok(())
    }

    /// Supplies a moving instance's interpolated baked field, in world axes.
    /// A state naming another model clears it. A static instance takes baked
    /// diffuse light from lightmap charts and the irradiance atlas instead,
    /// and is refused.
    /// Returns `SceneError::InvalidIrradiance` for any channel outside the
    /// finite `0..=65504` range, retaining the instance's previous cube.
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
        if !crate::static_lighting::valid_irradiance(&cube.irradiance) {
            return Err(SceneError::InvalidIrradiance);
        }
        instance.baked_irradiance = cube;
        self.instances
            .objects
            .write_baked_irradiance(queue, id.index(), cube);
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod temporal_recovery_tests {
    use super::*;

    // A hidden or removed object, or replaced geometry, can uncover
    // zero-motion background. Observe changes through real scene mutations,
    // rather than setting the temporal signal directly.
    #[test]
    fn rigid_recovery_tracks_departures_until_submission_and_ignores_unchanged_poses() {
        let Some((device, queue)) = crate::test_support::device() else {
            return;
        };
        let mut scene = Scene::new(&device, &queue);
        let model = scene
            .add_asset(&device, &queue, crate::test_support::cube())
            .unwrap()
            .model;
        let mut state = InstanceState {
            model,
            pose: Mat4::IDENTITY,
            visible: true,
            capture_visible: true,
        };
        let id = scene
            .add_instance(&device, &queue, state, Mobility::Moving)
            .unwrap();
        assert!(
            scene.rigid_objects_changed(),
            "appearance exposes new temporal content"
        );
        scene.finish_frame();
        scene.set_instance(&queue, id, state).unwrap();
        assert!(
            !scene.rigid_objects_changed(),
            "repeated game pose writes must not suppress stabilization"
        );
        scene
            .set_model(
                &device,
                &queue,
                model,
                crate::PreparedModel::new(Vec::new()).unwrap(),
            )
            .unwrap();
        assert!(
            scene.rigid_objects_changed(),
            "same-ID geometry removal uncovers history even though instance pose and identity are unchanged"
        );
        scene.finish_frame();
        state.visible = false;
        scene.set_instance(&queue, id, state).unwrap();
        assert!(
            scene.rigid_objects_changed(),
            "hiding a previously visible object is a departure"
        );
        scene.finish_frame();
        state.pose = Mat4::IDENTITY;
        scene.set_instance(&queue, id, state).unwrap();
        assert!(
            !scene.rigid_objects_changed(),
            "fully hidden rigid motion has no visible history"
        );
        state.visible = true;
        scene.set_instance(&queue, id, state).unwrap();
        scene.finish_frame();
        scene.remove_instance(id).unwrap();
        assert!(
            scene.rigid_objects_changed(),
            "removal must retain departure evidence after its slot is gone"
        );
        scene.finish_frame();
        assert!(
            !scene.rigid_objects_changed(),
            "only submission retires departure evidence"
        );
    }

    // Plausible defects: a moving instance's move, which its motion vectors
    // describe, re-arms the whole view (so a scene with anything moving never
    // rests); or a change that writes no motion vector does not. The
    // expected values are M-9's rule: a pose change of a moving instance
    // whose last submitted pose applies (visible in that frame and now, the
    // same model) leaves the signal down, wherever it moves; every change M-5
    // and M-4 give no motion (another model, hidden, shown after a hidden
    // frame, a hidden new instance shown and moved before its first
    // submitted frame, a static instance moved) raises it until a frame is
    // submitted.
    #[test]
    fn rigid_recovery_leaves_moves_with_motion_to_the_pixels() {
        let Some((device, queue)) = crate::test_support::device() else {
            return;
        };
        let mut scene = Scene::new(&device, &queue);
        let mut add_model = || {
            scene
                .add_asset(&device, &queue, crate::test_support::cube())
                .unwrap()
                .model
        };
        let (model, other) = (add_model(), add_model());
        let at = |x: f32| Mat4::from_translation(Vec3::new(x, 0., 0.));
        let shown = |model| InstanceState {
            model,
            pose: Mat4::IDENTITY,
            visible: true,
            capture_visible: true,
        };
        let moving = scene
            .add_instance(&device, &queue, shown(model), Mobility::Moving)
            .unwrap();
        let fixed = scene
            .add_instance(&device, &queue, shown(model), Mobility::Static)
            .unwrap();
        let mut state = InstanceState {
            pose: at(0.5),
            ..shown(model)
        };
        scene.finish_frame();
        let expect = |scene: &mut Scene, change: InstanceState, id, raised: bool, what: &str| {
            scene.set_instance(&queue, id, change).unwrap();
            assert_eq!(scene.rigid_objects_changed(), raised, "{what}");
            scene.finish_frame();
            assert!(
                !scene.rigid_objects_changed(),
                "{what}: only submission retires it"
            );
        };
        for (x, what) in [
            (0.5, "a moving instance moved from its submitted pose"),
            (1000., "a moving instance moved out of view"),
            (0.75, "a moving instance moved back into view"),
        ] {
            state.pose = at(x);
            expect(&mut scene, state, moving, false, what);
        }
        state.model = other;
        expect(
            &mut scene,
            state,
            moving,
            true,
            "a moving instance given another model",
        );
        state.pose = at(1.);
        expect(
            &mut scene,
            state,
            moving,
            false,
            "the next move of the other model",
        );
        state.visible = false;
        expect(&mut scene, state, moving, true, "a moving instance hidden");
        state = InstanceState {
            pose: at(2.),
            ..shown(other)
        };
        expect(
            &mut scene,
            state,
            moving,
            true,
            "a moving instance shown elsewhere after a hidden frame",
        );
        expect(
            &mut scene,
            InstanceState {
                pose: at(0.25),
                ..shown(model)
            },
            fixed,
            true,
            "a static instance moved",
        );
        let fresh = scene
            .add_instance(
                &device,
                &queue,
                InstanceState {
                    visible: false,
                    ..shown(model)
                },
                Mobility::Moving,
            )
            .unwrap();
        assert!(
            !scene.rigid_objects_changed(),
            "a hidden instance added shows nothing"
        );
        scene
            .set_instance(
                &queue,
                fresh,
                InstanceState {
                    pose: at(3.),
                    ..shown(model)
                },
            )
            .unwrap();
        assert!(
            scene.rigid_objects_changed(),
            "an instance with no submitted frame shown and moved"
        );
    }

    // Plausible defect: a material edit that adds or removes what its
    // users draw, with nothing moving, leaves the view-wide signal down, so
    // the camera at rest fades the change in over many frames. The expected
    // values are Frame item 10's rule: an edit raises the signal, until a
    // submitted frame, exactly when it changes what a visible instance's
    // meshes draw; shading alone, or a material nothing visible draws with,
    // does not.
    #[test]
    fn material_edits_that_change_what_is_drawn_rearm_until_submission() {
        use crate::{AlphaMode, MaterialShader, SurfaceMaterial};
        let Some((device, queue)) = crate::test_support::device() else {
            return;
        };
        let mut scene = Scene::new(&device, &queue);
        let shader = crate::test_support::add_test_shader(&mut scene);
        let mut material = |visible: bool| {
            let ids = scene
                .add_asset(&device, &queue, crate::test_support::cube())
                .unwrap();
            let state = InstanceState {
                visible,
                ..InstanceState::new(ids.model)
            };
            scene
                .add_instance(&device, &queue, state, Mobility::Moving)
                .unwrap();
            ids.materials[0]
        };
        let (shown, hidden) = (material(true), material(false));
        let base = scene.material(shown).unwrap();
        let masked = |cutoff| SurfaceMaterial {
            alpha: AlphaMode::Mask { cutoff },
            ..base
        };
        let blended = |receives_screen_space_reflections, keeps_specular| SurfaceMaterial {
            alpha: AlphaMode::Blend {
                receives_screen_space_reflections,
                keeps_specular,
            },
            ..base
        };
        let edits = [
            (
                "visibility group",
                base,
                SurfaceMaterial {
                    visibility_group: base.visibility_group ^ 2,
                    ..base
                },
                true,
            ),
            ("opaque to masked", base, masked(0.5), true),
            ("mask cutoff", masked(0.5), masked(0.75), true),
            (
                "masked base alpha",
                masked(0.5),
                SurfaceMaterial {
                    base: [base.base[0], base.base[1], base.base[2], 0.25],
                    ..masked(0.5)
                },
                true,
            ),
            ("opaque to blended", base, blended(false, false), true),
            (
                "masked to blended",
                masked(0.5),
                blended(false, false),
                true,
            ),
            (
                "transmissive",
                base,
                SurfaceMaterial {
                    transmission: 1.,
                    ..base
                },
                true,
            ),
            (
                "blended receiver",
                blended(false, false),
                blended(true, false),
                true,
            ),
            (
                "double-sided",
                base,
                SurfaceMaterial {
                    double_sided: !base.double_sided,
                    ..base
                },
                true,
            ),
            (
                "shader",
                base,
                SurfaceMaterial {
                    shader: Some(MaterialShader {
                        shader,
                        displacement_bound: 0.,
                    }),
                    ..base
                },
                true,
            ),
            (
                "colour",
                base,
                SurfaceMaterial {
                    base: [0.25, 0.5, 0.75, base.base[3]],
                    ..base
                },
                false,
            ),
            (
                "blended specular",
                blended(false, false),
                blended(false, true),
                false,
            ),
        ];
        for (name, before, after, draws) in edits {
            for (id, users) in [(shown, "a visible instance"), (hidden, "nothing visible")] {
                scene.set_material(&queue, id, before).unwrap();
                scene.finish_frame();
                scene.set_material(&queue, id, after).unwrap();
                let expected = draws && id == shown;
                assert_eq!(
                    scene.rigid_objects_changed(),
                    expected,
                    "{name} edit of a material {users} draws with"
                );
                scene.finish_frame();
                assert!(
                    !scene.rigid_objects_changed(),
                    "{name}: only submission retires the edit"
                );
            }
        }
    }
}
