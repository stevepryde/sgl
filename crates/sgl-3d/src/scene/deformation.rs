//! Deformation (the architecture's "Deformation"): a deforming model's
//! influences and morph targets in the scene source, and each deforming
//! instance's joint matrices, morph weights, deformed vertices and bounds,
//! with the `Scene` operation that poses it. Each frame the deform stage
//! morphs and skins the instances whose deformation changed since the last
//! submitted frame (`Scene::deformations`) into the position slot that does
//! not hold that frame's positions; every geometry pass reads the result,
//! and motion is measured from the other slot while the last submitted
//! frame's pose applies.
use super::rays::SceneRays;
use super::static_edits::posed_bounds;
use super::{Scene, SceneError};
use crate::content::deformation::{MAX_INDEX, MAX_MORPH_TARGETS, MeshDeformation};
use crate::content::identity::InstanceId;
use crate::content::model::ModelMesh;
use crate::shading::deformation::{
    DEFORMED_NORMAL_WORDS, DEFORMED_POSITION_WORDS, DeformDispatch, InfluenceRecord, JOINT_WORDS,
    MorphDeltaRecord, words,
};
use glam::{Mat4, Vec3};
use std::ops::Range;

const EMPTY: [Vec3; 2] = [Vec3::INFINITY, Vec3::NEG_INFINITY];

fn union(a: [Vec3; 2], b: [Vec3; 2]) -> [Vec3; 2] {
    [a[0].min(b[0]), a[1].max(b[1])]
}

/// One mesh of a deforming model.
struct DeformedMesh {
    /// Its first vertex among the model's, and its vertex count.
    first_vertex: u32,
    vertex_count: u32,
    /// Its first vertex record, influence and morph target word; zero
    /// influences when unskinned.
    vertices: u32,
    influences: u32,
    /// Whether it is skinned, so its influence word names its influences.
    skinned: bool,
    morph_targets: u32,
    morph_target_count: u32,
    /// Its bounds at bind, unmorphed.
    bounds: [Vec3; 2],
    /// Each joint's bounds of the vertices it influences, at bind: Bevy
    /// 9d12036's `SkinnedMeshBounds` (crates/bevy_mesh/src/skinning.rs).
    joints: Vec<(u32, [Vec3; 2])>,
    /// Each target's weight index and the bounds of its displacements.
    targets: Vec<(u32, [Vec3; 2])>,
}

impl DeformedMesh {
    /// The bounds inputs of a mesh with `vertices` deformed by
    /// `deformation`: its bind bounds, each joint's bounds of the vertices
    /// it influences with positive weight, and each target's displacement
    /// bounds. Its words are set where it is written.
    fn new(vertices: &[crate::asset::Vertex], deformation: &MeshDeformation) -> Self {
        let positions = || vertices.iter().map(|v| Vec3::from_array(v.position));
        let mut joints: Vec<[Vec3; 2]> = Vec::new();
        for (influence, position) in deformation.influences.iter().zip(positions()) {
            for (&joint, &weight) in influence.joints.iter().zip(&influence.weights) {
                if weight > 0. {
                    if joints.len() <= joint as usize {
                        joints.resize(joint as usize + 1, EMPTY);
                    }
                    joints[joint as usize] = union(joints[joint as usize], [position, position]);
                }
            }
        }
        Self {
            first_vertex: 0,
            vertex_count: vertices.len() as u32,
            vertices: 0,
            influences: 0,
            skinned: !deformation.influences.is_empty(),
            morph_targets: 0,
            morph_target_count: deformation.morph_targets.len() as u32,
            bounds: positions().fold(EMPTY, |b, p| union(b, [p, p])),
            joints: joints
                .into_iter()
                .enumerate()
                .filter(|(_, b)| b[0].cmple(b[1]).all())
                .map(|(joint, b)| (joint as u32, b))
                .collect(),
            targets: deformation
                .morph_targets
                .iter()
                .map(|target| {
                    let reach = target.deltas.iter().fold([Vec3::ZERO; 2], |b, delta| {
                        let d = Vec3::from_array(delta.position);
                        union(b, [d, d])
                    });
                    (target.weight, reach)
                })
                .collect(),
        }
    }

    /// Its bounds in the model's space under `joints` and `weights`:
    /// Bevy's skinned bounds, each joint's bounds under its matrix, with
    /// each joint's bounds first grown by the furthest the weighted morph
    /// targets displace any vertex, since morphing precedes skinning.
    fn bounds(&self, joints: &[Mat4], weights: &[f32]) -> [Vec3; 2] {
        let mut reach = [Vec3::ZERO; 2];
        for &(weight, [low, high]) in &self.targets {
            let weight = weights[weight as usize];
            reach[0] += (low * weight).min(high * weight);
            reach[1] += (low * weight).max(high * weight);
        }
        let grown = |bounds: [Vec3; 2]| [bounds[0] + reach[0], bounds[1] + reach[1]];
        if self.joints.is_empty() {
            return grown(self.bounds);
        }
        self.joints
            .iter()
            .fold(EMPTY, |bounds, &(joint, joint_bounds)| {
                union(
                    bounds,
                    posed_bounds(grown(joint_bounds), joints[joint as usize]),
                )
            })
    }
}

/// A deforming model's inputs: how many joint matrices and morph weights
/// its instances take, one more than the largest index its meshes name, and
/// its meshes' words in the scene source.
pub(crate) struct ModelDeformation {
    pub joints: u32,
    pub morph_weights: u32,
    /// Its vertices over all its meshes.
    vertices: u32,
    meshes: Vec<DeformedMesh>,
    /// Its words in the scene source.
    range: Range<u32>,
}

/// Whether `meshes`' deformations fit their vertices: as many influences as
/// vertices or none, each with finite nonnegative weights of positive sum,
/// and each morph target a finite displacement of every vertex, with
/// indices of at most `MAX_INDEX` and at most `MAX_MORPH_TARGETS` of them.
pub(crate) fn validate(meshes: &[ModelMesh]) -> Result<(), SceneError> {
    for mesh in meshes {
        let deformation = &mesh.deformation;
        let count = mesh.vertices.len();
        if !deformation.influences.is_empty() && deformation.influences.len() != count {
            return Err(SceneError::InvalidDeformation);
        }
        if deformation.influences.iter().any(|influence| {
            influence.joints.iter().any(|&joint| joint > MAX_INDEX)
                || influence.weights.iter().any(|w| !w.is_finite() || *w < 0.)
                || influence.weights.iter().sum::<f32>() <= 0.
        }) {
            return Err(SceneError::InvalidDeformation);
        }
        if deformation.morph_targets.len() > MAX_MORPH_TARGETS
            || deformation.morph_targets.iter().any(|target| {
                target.weight > MAX_INDEX
                    || target.deltas.len() != count
                    || !target.deltas.iter().all(|delta| {
                        [delta.position, delta.normal, delta.tangent]
                            .iter()
                            .flatten()
                            .all(|v| v.is_finite())
                    })
            })
        {
            return Err(SceneError::InvalidDeformation);
        }
    }
    Ok(())
}

/// A deforming model's influences and morph targets prepared without the
/// source (`PreparedDeformation::new`): its words, and each mesh's record
/// with the words where its influences and morph targets start counted from
/// zero.
pub(crate) struct PreparedDeformation {
    joints: u32,
    morph_weights: u32,
    vertices: u32,
    meshes: Vec<DeformedMesh>,
    words: Vec<u32>,
}

impl PreparedDeformation {
    /// The deformation of validated `meshes`; none when every mesh is rigid.
    pub fn new(meshes: &[ModelMesh]) -> Option<Self> {
        if meshes.iter().all(|mesh| mesh.deformation.is_rigid()) {
            return None;
        }
        let influence_words = words::<InfluenceRecord>() as usize;
        let delta_words = words::<MorphDeltaRecord>() as usize;
        let len: usize = meshes
            .iter()
            .map(|mesh| {
                let count = mesh.vertices.len();
                let targets = mesh.deformation.morph_targets.len();
                mesh.deformation.influences.len() * influence_words
                    + targets
                    + targets * count * delta_words
            })
            .sum();
        let mut block: Vec<u32> = Vec::with_capacity(len);
        let mut deformed = Vec::with_capacity(meshes.len());
        let (mut joints, mut morph_weights, mut first_vertex) = (0, 0, 0);
        for mesh in meshes {
            let deformation = &mesh.deformation;
            let mut packed = DeformedMesh::new(&mesh.vertices, deformation);
            packed.first_vertex = first_vertex;
            if !deformation.influences.is_empty() {
                packed.influences = block.len() as u32;
                for influence in &deformation.influences {
                    let sum: f32 = influence.weights.iter().sum();
                    let record = InfluenceRecord {
                        joints: influence.joints,
                        weights: influence.weights.map(|w| w / sum),
                    };
                    block.extend_from_slice(bytemuck::cast_slice(&[record]));
                    joints = influence.joints.iter().fold(joints, |n, &j| n.max(j + 1));
                }
            }
            packed.morph_targets = block.len() as u32;
            block.extend(deformation.morph_targets.iter().map(|target| target.weight));
            for target in &deformation.morph_targets {
                morph_weights = morph_weights.max(target.weight + 1);
                block.extend(target.deltas.iter().flat_map(|delta| {
                    let record = MorphDeltaRecord {
                        position: delta.position,
                        normal: delta.normal,
                        tangent: delta.tangent,
                    };
                    bytemuck::cast::<_, [u32; 9]>(record)
                }));
            }
            first_vertex += packed.vertex_count;
            deformed.push(packed);
        }
        debug_assert_eq!(block.len(), len, "a deformation fills its words");
        Some(Self {
            joints,
            morph_weights,
            vertices: first_vertex,
            meshes: deformed,
            words: block,
        })
    }

    /// Whether the deform stage can dispatch each deforming mesh's vertices
    /// on `device`.
    pub fn dispatchable(&self, device: &wgpu::Device) -> bool {
        let limit = u64::from(device.limits().max_compute_workgroups_per_dimension) * 64;
        self.meshes
            .iter()
            .all(|mesh| u64::from(mesh.vertex_count) <= limit)
    }
}

impl ModelDeformation {
    /// Places `prepared`, whose meshes' vertex records start at `vertices`:
    /// allocates its range and adds its start to the words where each
    /// mesh's influences and morph targets start. Returns it and the words
    /// to write at its range's start.
    pub fn place(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &mut SceneRays,
        prepared: PreparedDeformation,
        vertices: &[u32],
    ) -> Result<(Self, Vec<u32>), SceneError> {
        let range = rays.allocate(device, queue, prepared.words.len())?;
        let mut meshes = prepared.meshes;
        for (mesh, &vertex_word) in meshes.iter_mut().zip(vertices) {
            mesh.vertices = vertex_word;
            if mesh.skinned {
                mesh.influences += range.start;
            }
            mesh.morph_targets += range.start;
        }
        Ok((
            Self {
                joints: prepared.joints,
                morph_weights: prepared.morph_weights,
                vertices: prepared.vertices,
                meshes,
                range,
            },
            prepared.words,
        ))
    }

    /// Where its words start.
    pub fn word(&self) -> u32 {
        self.range.start
    }

    /// Frees its words.
    pub fn free(self, rays: &mut SceneRays) {
        rays.free(self.range);
    }

    /// Each mesh's bounds in the model's space under `joints` and `weights`.
    fn bounds(&self, joints: &[Mat4], weights: &[f32]) -> Vec<[Vec3; 2]> {
        self.meshes
            .iter()
            .map(|mesh| mesh.bounds(joints, weights))
            .collect()
    }

    /// An instance's words: its joint matrices, morph weights, two slots of
    /// positions, and normals and tangents.
    fn instance_words(&self) -> u32 {
        self.joints * JOINT_WORDS
            + self.morph_weights
            + self.vertices * (2 * DEFORMED_POSITION_WORDS + DEFORMED_NORMAL_WORDS)
    }
}

/// A deforming instance's pose of its joints and morph targets, its words
/// in the scene source, and which of its position slots hold what.
pub(crate) struct InstanceDeformation {
    joints: Vec<Mat4>,
    weights: Vec<f32>,
    range: Range<u32>,
    /// Each mesh's bounds in the model's space as deformed, and their union.
    pub mesh_bounds: Vec<[Vec3; 2]>,
    pub bounds: [Vec3; 2],
    /// Changes whenever its deformation is set.
    pub revision: u64,
    /// The slot holding its positions as of the last submitted frame.
    written: Option<u32>,
    /// The slot motion is measured from: `written` while it was visible in
    /// the last submitted frame.
    motion: Option<u32>,
    /// Set since the last submitted frame.
    changed: bool,
    /// The frame being rendered's slot, and whether the frame writes it.
    frame: Option<(u32, bool)>,
    /// Its object record's deformed position, previous position and normal
    /// words this frame.
    pub record: [u32; 3],
}

impl InstanceDeformation {
    /// An instance of `model` at its bind pose: identity joint matrices and
    /// zero morph weights.
    pub fn add(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &mut SceneRays,
        model: &ModelDeformation,
    ) -> Result<Self, SceneError> {
        let range = rays.allocate(device, queue, model.instance_words() as usize)?;
        let mut deformation = Self {
            joints: Vec::new(),
            weights: Vec::new(),
            range,
            mesh_bounds: Vec::new(),
            bounds: EMPTY,
            revision: 0,
            written: None,
            motion: None,
            changed: true,
            frame: None,
            record: [0; 3],
        };
        let joints = vec![Mat4::IDENTITY; model.joints as usize];
        let weights = vec![0.; model.morph_weights as usize];
        deformation.set(queue, rays, model, &joints, &weights)?;
        Ok(deformation)
    }

    /// Frees its words.
    pub fn free(self, rays: &mut SceneRays) {
        rays.free(self.range);
    }

    /// Poses it with the first of `joints` and `weights` that `model` takes.
    fn set(
        &mut self,
        queue: &wgpu::Queue,
        rays: &SceneRays,
        model: &ModelDeformation,
        joints: &[Mat4],
        weights: &[f32],
    ) -> Result<(), SceneError> {
        let joints = joints
            .get(..model.joints as usize)
            .ok_or(SceneError::DeformationMismatch)?;
        let weights = weights
            .get(..model.morph_weights as usize)
            .ok_or(SceneError::DeformationMismatch)?;
        if !joints.iter().all(Mat4::is_finite) || !weights.iter().all(|w| w.is_finite()) {
            return Err(SceneError::DeformationMismatch);
        }
        self.joints.clear();
        self.joints.extend_from_slice(joints);
        self.weights.clear();
        self.weights.extend_from_slice(weights);
        let mut words: Vec<u32> =
            Vec::with_capacity((model.joints * JOINT_WORDS) as usize + weights.len());
        for joint in joints {
            words.extend_from_slice(bytemuck::cast_slice(&joint.to_cols_array()));
        }
        words.extend_from_slice(bytemuck::cast_slice(weights));
        rays.write(queue, self.range.start, &words);
        self.mesh_bounds = model.bounds(joints, weights);
        self.bounds = self.mesh_bounds.iter().fold(EMPTY, |b, m| union(b, *m));
        self.revision = super::next_generation();
        self.changed = true;
        Ok(())
    }

    fn weights_word(&self, model: &ModelDeformation) -> u32 {
        self.range.start + model.joints * JOINT_WORDS
    }

    /// The first word of position slot `slot`, or of the normals for slot 2.
    fn slot_word(&self, model: &ModelDeformation, slot: u32) -> u32 {
        self.weights_word(model)
            + model.morph_weights
            + slot * model.vertices * DEFORMED_POSITION_WORDS
    }

    /// Begins a frame: shows the deformation as of the last submitted frame
    /// unless it changed since, in which case `work` gains the dispatches
    /// that write it into the other slot. Motion is measured from the last
    /// submitted frame's slot while its pose applies.
    pub fn prepare(&mut self, model: &ModelDeformation, work: &mut Vec<DeformDispatch>) {
        let (slot, writes) = match self.written {
            Some(written) if !self.changed => (written, false),
            Some(written) => (1 - written, true),
            None => (0, true),
        };
        self.frame = Some((slot, writes));
        let positions = self.slot_word(model, slot);
        let normals = self.slot_word(model, 2);
        self.record = [
            positions,
            self.motion
                .map_or(positions, |previous| self.slot_word(model, previous)),
            normals,
        ];
        if writes {
            work.extend(model.meshes.iter().map(|mesh| DeformDispatch {
                vertices: mesh.vertices,
                vertex_count: mesh.vertex_count,
                influences: mesh.influences,
                morph_targets: mesh.morph_targets,
                morph_target_count: mesh.morph_target_count,
                joints: self.range.start,
                weights: self.weights_word(model),
                positions: positions + mesh.first_vertex * DEFORMED_POSITION_WORDS,
                normals: normals + mesh.first_vertex * DEFORMED_NORMAL_WORDS,
            }));
        }
    }

    /// The first word of mesh `mesh`'s positions in the frame being
    /// rendered, which shadow casters read.
    pub fn positions(&self, model: &ModelDeformation, mesh: usize) -> u32 {
        self.record[0] + model.meshes[mesh].first_vertex * DEFORMED_POSITION_WORDS
    }

    /// Commits a submitted frame in which the instance was `visible`.
    pub fn finish(&mut self, visible: bool) {
        if let Some((slot, writes)) = self.frame.take() {
            if writes {
                self.written = Some(slot);
                self.changed = false;
            }
            self.motion = self.written.filter(|_| visible);
        }
    }
}

impl Scene {
    /// Poses a deforming instance: one joint matrix for each joint its
    /// model's influences name, by index (glTF's joint matrix: the joint's
    /// transform in the model's space times its inverse bind matrix;
    /// `deformation::Rig::joint_matrices`), and one weight for each morph
    /// weight its targets name. Further matrices and weights are ignored.
    /// The instance keeps them until they are set again; one never set is
    /// at its bind pose. Its motion is measured from its deformation in the
    /// last submitted frame.
    pub fn set_instance_deformation(
        &mut self,
        queue: &wgpu::Queue,
        id: InstanceId,
        joints: &[Mat4],
        morph_weights: &[f32],
    ) -> Result<(), SceneError> {
        let instance = self
            .instances
            .slots
            .get_mut(id)
            .ok_or(SceneError::UnknownInstance)?;
        let model = self
            .models
            .slots
            .get(instance.state.model)
            .expect("an instance's model lives");
        let (Some(deformation), Some(model)) = (&mut instance.deformation, &model.deformation)
        else {
            return Err(SceneError::DeformationMismatch);
        };
        deformation.set(queue, &self.rays, model, joints, morph_weights)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
