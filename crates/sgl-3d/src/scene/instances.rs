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
    /// Its pose and model in the last submitted frame, when it was visible
    /// in it: a moving instance's motion is measured from it, and a change
    /// no motion vector describes marks where it stood (M-23).
    drawn: Option<(Mat4, ModelId)>,
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
    /// What the frame being rendered draws of it, which a finished frame
    /// commits; none when it was added since the frame began.
    rendered: Option<Shown>,
    /// What its footprints hold (M-23).
    captured: Captured,
}

/// Which of its states an instance's footprints hold (M-23): where the
/// last finished frame drew it and where the frame being rendered draws
/// it, since those frames, and where it stands, since the last frame began.
#[derive(Clone, Copy, Default)]
struct Captured {
    drawn: bool,
    rendered: bool,
    stands: bool,
}

/// A state a frame drew of an instance, for its footprint
/// (`Instance::uncaptured`): its pose, its model, its deformed positions'
/// slot in the scene source (zero for a rigid one) and their bounds in the
/// model's space.
pub(crate) struct Drawn {
    pub pose: Mat4,
    pub model: ModelId,
    pub positions: u32,
    pub bounds: Option<[Vec3; 2]>,
}

/// What a frame draws of an instance (`Scene::begin_frame`).
#[derive(Clone, Copy)]
struct Shown {
    pose: Mat4,
    model: ModelId,
    visible: bool,
    shader_data: [f32; 4],
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
            drawn: None,
            stale: false,
            deformation,
            shader_data: [0.; 4],
            submitted_shader_data: None,
            shader_stale: false,
            rendered: None,
            captured: Captured::default(),
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

    /// Whether the next frame measures a moving instance's motion from a
    /// pose of `model`, however the frame being rendered ends: the last
    /// finished frame showed it with `model`, and the frame being rendered,
    /// if one is, draws it shown with `model` too, since finishing that frame
    /// makes its record the pose motion is measured from (F-13).
    fn last_pose_applies(&self, model: ModelId) -> bool {
        self.mobility == Mobility::Moving
            && self.drawn.is_some_and(|(_, drawn)| drawn == model)
            && self
                .rendered
                .is_none_or(|shown| shown.visible && shown.model == model)
    }

    /// Whether a frame the next one may follow drew it with `model`, or it
    /// is visible with it now.
    fn shows(&self, model: ModelId) -> bool {
        self.drawn.is_some_and(|(_, drawn)| drawn == model)
            || self
                .rendered
                .is_some_and(|shown| shown.visible && shown.model == model)
            || (self.state.visible && self.state.model == model)
    }

    /// The states the frames the next one may follow drew of it that its
    /// footprints do not hold yet (M-23), each drawn with a model `accepts`:
    /// the last finished frame's and the frame being rendered's, each where
    /// it was visible, with the deformed positions that frame showed, which
    /// `models`' deformation lays out; and which of the two its footprints
    /// hold once those states are kept. The frame being rendered's state is
    /// left out where it is the last finished frame's and that is kept in
    /// the same footprint, which then holds both.
    pub(crate) fn uncaptured(
        &self,
        models: &super::models::Models,
        accepts: impl Fn(ModelId) -> bool,
    ) -> ([Option<Drawn>; 2], [bool; 2]) {
        let shown = self.deformation.as_ref().map_or([None; 2], |deformation| {
            let model = models.slots.get(self.state.model);
            let model = model.and_then(|model| model.deformation.as_ref());
            deformation.shown(model.expect("a deforming instance's model deforms"))
        });
        let drawn = |(pose, model): (Mat4, ModelId), slot: Option<(u32, [Vec3; 2])>| Drawn {
            pose,
            model,
            positions: slot.map_or(0, |(word, _)| word),
            bounds: slot.map(|(_, bounds)| bounds),
        };
        let finished = self
            .drawn
            .filter(|&(_, model)| !self.captured.drawn && accepts(model))
            .map(|state| drawn(state, shown[0]));
        let rendered = self
            .rendered
            .filter(|state| state.visible && !self.captured.rendered && accepts(state.model))
            .map(|state| (state.pose, state.model));
        // Only a footprint kept now holds the frame being rendered's state
        // for as long as it does: one kept before the frame began retires
        // when it is finished, before the next frame that follows it.
        let folded = finished.is_some()
            && rendered.is_some_and(|state| Some(state) == self.drawn && shown[1] == shown[0]);
        let rendered = rendered
            .filter(|_| !folded)
            .map(|state| drawn(state, shown[1]));
        let held = [finished.is_some(), rendered.is_some() || folded];
        ([finished, rendered], held)
    }

    /// Its footprints now hold the last finished frame's state where
    /// `drawn` and the frame being rendered's where `rendered`.
    pub(crate) fn captured(&mut self, [drawn, rendered]: [bool; 2]) {
        self.captured.drawn |= drawn;
        self.captured.rendered |= rendered;
    }

    /// Whether its footprints do not yet mark where it stands.
    pub(crate) fn stands_uncaptured(&self) -> bool {
        !self.captured.stands
    }

    /// Its footprints mark where it stands until the next frame begins.
    pub(crate) fn stands_captured(&mut self) {
        self.captured.stands = true;
    }

    /// The pose motion is measured from: a moving instance's pose in the
    /// last submitted frame while that pose applies, else its current pose,
    /// which writes no motion.
    fn previous_pose(&self) -> Mat4 {
        match (self.mobility, self.drawn) {
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
    /// Changes that no motion vector describes and no footprint marks
    /// (M-9, `Scene::footprint_stood`), counted as raised: those past the
    /// footprints' cap, and those of an instance drawn with a shader that
    /// displaces its vertices and is not marked every frame. The count the
    /// frame being rendered saw, and the count the last finished frame saw:
    /// a finished frame retires what was raised before it began.
    rigid: [u64; 3],
    /// The instances whose shader data was set since the last submitted
    /// frame (`set_shader_data`), repeats and removed ones included, and
    /// how many of them the frame being rendered saw.
    shader_edits: Vec<InstanceId>,
    shader_edits_rendered: usize,
}

/// `Instances::rigid`'s counts.
const RAISED: usize = 0;
const RENDERED: usize = 1;
const COMMITTED: usize = 2;

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
            rigid: [0; 3],
            shader_edits: Vec::new(),
            shader_edits_rendered: 0,
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

    /// Raises the view-wide signal (M-9) for a change no footprint marks.
    pub fn raise_view_wide(&mut self) {
        self.rigid[RAISED] = self.rigid[RAISED].wrapping_add(1);
    }

    /// The instances a frame the next one may follow drew with `model`, or
    /// that are visible with it now, culled or not: those whose pixels a
    /// replacement of its geometry, or an edit of what a material draws
    /// on it, changes without a motion vector.
    pub fn showing(&self, model: ModelId) -> Vec<InstanceId> {
        self.slots
            .iter()
            .filter(|(_, instance)| instance.shows(model))
            .map(|(id, _)| id)
            .collect()
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
            if let Some((pose, _)) = &mut instance.drawn {
                *pose = translated(*pose, by);
            }
            if let Some(shown) = &mut instance.rendered {
                shown.pose = translated(shown.pose, by);
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

    /// Commits a submitted frame, as it drew each instance it drew
    /// (`prepare_frame`): its pose and model become where it was drawn
    /// while it was visible, which a moving instance's motion is measured
    /// from and its footprints mark (M-23), its deformation the one its
    /// motion is measured from, and its shader data the one its shaders'
    /// motion is. The view-wide changes and shader-data edits made before the
    /// frame began are retired. An instance added since commits nothing,
    /// and edits made since stay for the next frame.
    pub fn finish_frame(&mut self) {
        self.rigid[COMMITTED] = self.rigid[RENDERED];
        self.shader_edits.drain(..self.shader_edits_rendered);
        self.shader_edits_rendered = 0;
        for (_, instance) in self.slots.iter_mut() {
            let Some(shown) = instance.rendered.take() else {
                continue;
            };
            // What the written records hold as previous, which a later
            // write keeps current: a rewrite is due where it changes.
            let shader_on_gpu = instance
                .submitted_shader_data
                .unwrap_or(instance.shader_data);
            instance.shader_stale |= shader_on_gpu != shown.shader_data;
            instance.submitted_shader_data = Some(shown.shader_data);
            if let Some(deformation) = &mut instance.deformation {
                deformation.finish(shown.visible);
            }
            let on_gpu = instance.previous_pose();
            instance.drawn = shown.visible.then_some((shown.pose, shown.model));
            instance.stale |= instance.previous_pose() != on_gpu;
            // The frame's state is where it was drawn now.
            instance.captured.drawn = instance.captured.rendered;
            instance.captured.rendered = false;
        }
    }

    /// Begins a frame: rewrites the records of moving instances not posed
    /// since the last submitted frame, whose motion it ended, and of
    /// deforming instances, whose deformed vertices this frame shows, and
    /// the shader data of instances whose data the last submitted frame
    /// changed, replaces `work` with the deformations the frame writes,
    /// and records what the frame draws of each instance, with the rigid
    /// changes and shader-data edits made so far, for `finish_frame`.
    pub fn prepare_frame(
        &mut self,
        queue: &wgpu::Queue,
        models: &super::models::Models,
        work: &mut Vec<crate::shading::deformation::DeformDispatch>,
    ) {
        work.clear();
        self.rigid[RENDERED] = self.rigid[RAISED];
        self.shader_edits_rendered = self.shader_edits.len();
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
            instance.rendered = Some(Shown {
                pose: instance.state.pose,
                model: instance.state.model,
                visible: instance.state.visible,
                shader_data: instance.shader_data,
            });
            // The frame draws its own state, and marks where it stands from
            // the edits before it.
            instance.captured.rendered = false;
            instance.captured.stands = false;
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
        self.write_instance(queue, id);
        // Its index may be a removed instance's.
        self.instances.write_shader_data(queue, id);
        // An appearance writes no motion (M-5): its footprint marks it.
        self.footprint_stands(id);
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

    /// Changes without a motion vector that no footprint marks (M-9): past
    /// the footprints' cap, or under a displacing shader that is not
    /// marked every frame. Deliberately conservative across frustum and
    /// occlusion culling: a culled new state can still expose history at
    /// the old one. A finished frame clears what was raised before it
    /// began.
    pub(crate) fn rigid_objects_changed(&self) -> bool {
        self.instances.rigid[RAISED] != self.instances.rigid[COMMITTED]
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
        // other change has no motion vector and marks where the instance
        // stood and stands (M-23). Between a frame's submission and
        // `finish_frame` that pose is the frame's own record as much as the
        // last finished frame's.
        let motion_describes_it = state.visible && instance.last_pose_applies(state.model);
        let unmoving = (old.visible || state.visible)
            && (old.visible != state.visible || (placed && !motion_describes_it));
        if unmoving {
            self.footprint_stood(id, None);
        }
        let instance = self.instances.slots.get_mut(id).unwrap();
        instance.state = state;
        instance.stale = false;
        if placed {
            instance.pose_casters(self.models.get(state.model)?);
        }
        self.write_instance(queue, id);
        if unmoving {
            self.footprint_stands(id);
        }
        Ok(())
    }

    /// Removes an instance. Its index is reused under a new identity. Where
    /// a frame drew it, the next marks where it stood (M-23), and a
    /// deforming instance's positions stay until that frame is finished.
    pub fn remove_instance(&mut self, id: InstanceId) -> Result<(), SceneError> {
        self.edited();
        self.instances.get(id)?;
        self.footprint_stood(id, None);
        let mut instance = self
            .instances
            .slots
            .remove(id)
            .ok_or(SceneError::UnknownInstance)?;
        self.candidates.remove(id.index());
        if let Some(deformation) = instance.deformation.take() {
            self.footprints
                .free(&mut self.rays, deformation.into_words());
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
