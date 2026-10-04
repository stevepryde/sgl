//! What deforms a mesh (a skin, morph targets or both), and the rig and
//! animation clips a loaded asset is posed by, as plain data. The game
//! samples and blends clips, poses the rig's nodes and gives each deforming
//! instance its joint matrices and morph weights every frame
//! (`Scene::set_instance_deformation`); SGL3D renders the pose it is given.
use glam::{Mat4, Quat, Vec3};

/// The largest joint or morph weight index a mesh may name.
pub(crate) const MAX_INDEX: u32 = u16::MAX as u32;

/// One vertex's skin: four of its model's joints and their weights. Unused
/// slots have weight zero. The scene normalizes the weights.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Influence {
    /// Indices into the instance's joint matrices, at most 65535.
    pub joints: [u32; 4],
    /// Finite and nonnegative, with a positive sum.
    pub weights: [f32; 4],
}

/// One vertex's displacement by a morph target at weight one.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MorphDelta {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    /// Added to the tangent's direction; its handedness is unchanged.
    pub tangent: [f32; 3],
}

/// A morph target: a displacement of each vertex of its mesh, scaled by one
/// of the instance's morph weights.
#[derive(Clone, Debug, PartialEq)]
pub struct MorphTarget {
    /// The index of the morph weight that scales it, at most 65535.
    pub weight: u32,
    /// One per vertex, in the mesh's vertex order.
    pub deltas: Vec<MorphDelta>,
}

/// What deforms a mesh. The default deforms nothing: a rigid mesh. A mesh
/// is morphed first, then skinned, in its model's space, as glTF defines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshDeformation {
    /// One per vertex for a skinned mesh, in vertex order; empty otherwise.
    pub influences: Vec<Influence>,
    pub morph_targets: Vec<MorphTarget>,
}

impl MeshDeformation {
    /// Whether nothing deforms the mesh.
    pub fn is_rigid(&self) -> bool {
        self.influences.is_empty() && self.morph_targets.is_empty()
    }
}

/// A node of a loaded asset's hierarchy, with its rest transform relative
/// to its parent (or to the asset's space for a root).
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub name: Option<String>,
    pub parent: Option<usize>,
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Node {
    /// Its rest transform relative to its parent.
    pub fn rest(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

/// A joint: the node that poses it and its inverse bind matrix, which takes
/// the asset's space to the joint's at bind time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Joint {
    pub node: usize,
    pub inverse_bind: Mat4,
}

/// One morph weight: the node whose animation channels set it, and its
/// value at rest. A node's weights are consecutive, in its targets' order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MorphWeight {
    pub node: usize,
    pub rest: f32,
}

/// How a channel's value varies between its keyframes (glTF's sampler
/// interpolation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    Step,
    Linear,
    /// Each keyframe holds an in-tangent, a value and an out-tangent, in
    /// that order.
    CubicSpline,
}

/// A channel's keyframe values. Morph weights hold each keyframe's weights
/// of the node, in its targets' order.
#[derive(Clone, Debug, PartialEq)]
pub enum ChannelValues {
    Translation(Vec<Vec3>),
    Rotation(Vec<Quat>),
    Scale(Vec<Vec3>),
    MorphWeights(Vec<f32>),
}

/// One animated property of one node.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub node: usize,
    pub interpolation: Interpolation,
    /// Keyframe times in seconds, increasing.
    pub times: Vec<f32>,
    pub values: ChannelValues,
}

/// An animation clip.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub name: Option<String>,
    pub channels: Vec<Channel>,
}

/// What poses a loaded asset's deforming meshes: its nodes, the joints its
/// influences name (by index) and the morph weights its targets name (by
/// index), and its animation clips. Empty for an asset that does not deform.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rig {
    pub nodes: Vec<Node>,
    pub joints: Vec<Joint>,
    pub morph_weights: Vec<MorphWeight>,
    pub clips: Vec<Clip>,
}

impl Rig {
    /// Each joint's matrix with the nodes posed at `locals` (each node's
    /// transform relative to its parent, in node order): its node's
    /// transform in the asset's space times its inverse bind matrix, glTF's
    /// joint matrix. Nodes may come in any order.
    pub fn joint_matrices(&self, locals: &[Mat4]) -> Vec<Mat4> {
        let mut globals: Vec<Option<Mat4>> = vec![None; self.nodes.len()];
        self.joints
            .iter()
            .map(|joint| self.global(joint.node, locals, &mut globals) * joint.inverse_bind)
            .collect()
    }

    fn global(&self, node: usize, locals: &[Mat4], globals: &mut [Option<Mat4>]) -> Mat4 {
        if let Some(global) = globals[node] {
            return global;
        }
        let global = match self.nodes[node].parent {
            Some(parent) => self.global(parent, locals, globals) * locals[node],
            None => locals[node],
        };
        globals[node] = Some(global);
        global
    }
}
