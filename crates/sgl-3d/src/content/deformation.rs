//! What deforms a mesh (a skin, morph targets or both), and the rig and
//! animation clips a loaded asset is posed by, as plain data. The game poses
//! the rig's nodes (sampling and blending clips with `animation::Pose`, or
//! by its own means) and gives each deforming instance its joint matrices
//! and morph weights every frame (`Scene::set_instance_deformation`); SGL3D
//! renders the pose it is given.
use glam::{Mat4, Quat, Vec3};

/// The largest joint or morph weight index a mesh may name.
pub(crate) const MAX_INDEX: u32 = u16::MAX as u32;
/// The most morph targets a mesh may have: Bevy's `MAX_MORPH_WEIGHTS`
/// (9d12036 `crates/bevy_mesh/src/morph.rs`), beyond which Bevy refuses a
/// mesh too. The deform stage's loop over a vertex's targets stops there
/// (`DEFORM_MOST_MORPH_TARGETS`, AR-12).
pub(crate) const MAX_MORPH_TARGETS: usize = 256;

/// One vertex's skin: four of its model's joints and their weights. Unused
/// slots have weight zero. The scene normalizes the weights.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Influence {
    /// Indices into the instance's joint matrices, at most 65535.
    pub joints: [u32; 4],
    /// Finite and nonnegative, with a finite positive f32 sum.
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
    /// At most 256, as Bevy allows.
    pub morph_targets: Vec<MorphTarget>,
}

impl MeshDeformation {
    /// Whether nothing deforms the mesh.
    pub fn is_rigid(&self) -> bool {
        self.influences.is_empty() && self.morph_targets.is_empty()
    }
}

/// A node's transform relative to its parent: scaled, then rotated, then
/// translated, as glTF composes a node's TRS properties.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub translation: Vec3,
    /// A unit quaternion.
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Transform {
    /// No translation, rotation or scale.
    pub const IDENTITY: Self = Self {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    /// Its matrix, `T * R * S`.
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// A node of a loaded asset's hierarchy, with its rest transform relative
/// to its parent (or to the asset's space for a root).
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub name: Option<String>,
    pub parent: Option<usize>,
    pub rest: Transform,
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

/// One animated property of one node. A loaded channel's values match its
/// keyframes: one per keyframe, three (in-tangent, value, out-tangent) for
/// a cubic spline, each a node's whole set of morph weights for weights.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub node: usize,
    pub interpolation: Interpolation,
    /// Keyframe times in seconds, strictly increasing.
    pub times: Vec<f32>,
    pub values: ChannelValues,
}

/// An animation clip, sampled and blended by `animation::Pose`.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub name: Option<String>,
    pub channels: Vec<Channel>,
}

impl Clip {
    /// Its latest keyframe time, zero for a clip without keyframes.
    pub fn duration(&self) -> f32 {
        self.channels
            .iter()
            .filter_map(|channel| channel.times.last().copied())
            .fold(0., f32::max)
    }
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
    /// transform relative to its parent, in node order), into `joints`:
    /// its node's transform in the asset's space times its inverse bind
    /// matrix, glTF's joint matrix, one per joint in joint order. Nodes may
    /// come in any order. `joints` keeps its buffers between calls, so a
    /// rig posed every frame allocates nothing once they have their size.
    ///
    /// `locals` must hold exactly one matrix per node, and every joint and
    /// the parent of each node above one must name a node of the rig (other
    /// nodes are not read); otherwise nothing is composed,
    /// `joints` is left empty and the error says why. A loaded rig's
    /// parents form trees; a built one whose parents form a cycle composes
    /// a node's ancestors only as far as there are nodes, so its matrices
    /// are wrong but its evaluation ends.
    pub fn joint_matrices(
        &self,
        locals: &[Mat4],
        joints: &mut JointMatrices,
    ) -> Result<(), RigError> {
        let JointMatrices {
            matrices,
            globals,
            chain,
        } = joints;
        matrices.clear();
        if locals.len() != self.nodes.len() {
            return Err(RigError::LocalCount {
                nodes: self.nodes.len(),
                locals: locals.len(),
            });
        }
        globals.clear();
        globals.resize(self.nodes.len(), None);
        for joint in &self.joints {
            match self.global(joint.node, locals, globals, chain) {
                Ok(global) => matrices.push(global * joint.inverse_bind),
                Err(error) => {
                    matrices.clear();
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// `node`'s transform in the asset's space, each node's composed once:
    /// up from it to the nearest composed ancestor or its root, then down.
    /// A chain of ancestors is at most as long as there are nodes, the cap
    /// that ends a cycle's walk. `locals` and `globals` hold one per node.
    /// `spring_bones` composes its chains through it, clearing a node's
    /// entry in `globals` when it changes that node's local matrix.
    pub(crate) fn global(
        &self,
        node: usize,
        locals: &[Mat4],
        globals: &mut [Option<Mat4>],
        chain: &mut Vec<usize>,
    ) -> Result<Mat4, RigError> {
        chain.clear();
        let mut above = Mat4::IDENTITY;
        let mut at = node;
        loop {
            let composed = globals.get(at).ok_or(RigError::MissingNode { node: at })?;
            if let Some(global) = *composed {
                above = global;
                break;
            }
            chain.push(at);
            match self.nodes[at].parent {
                // Checked before the cap, which would otherwise end the
                // walk at a node whose parent is missing.
                Some(parent) if parent >= self.nodes.len() => {
                    return Err(RigError::MissingNode { node: parent });
                }
                Some(parent) if chain.len() < self.nodes.len() => at = parent,
                _ => break,
            }
        }
        for &at in chain.iter().rev() {
            above *= locals[at];
            globals[at] = Some(above);
        }
        Ok(above)
    }
}

/// A rig's joint matrices as `Rig::joint_matrices` last composed them, and
/// the buffers it composes them in. Keep one between frames (one serves
/// every instance, since `Scene::set_instance_deformation` copies what it
/// is given) and it allocates nothing once its buffers have their size.
#[derive(Clone, Debug, Default)]
pub struct JointMatrices {
    matrices: Vec<Mat4>,
    /// Each node's transform in the asset's space, once composed.
    globals: Vec<Option<Mat4>>,
    /// The ancestors of a node being composed.
    chain: Vec<usize>,
}

impl JointMatrices {
    /// Empty buffers, which the first composition sizes.
    pub fn new() -> Self {
        Self::default()
    }

    /// One matrix per joint, in the rig's joint order: what
    /// `Scene::set_instance_deformation` takes. Empty after an error.
    pub fn matrices(&self) -> &[Mat4] {
        &self.matrices
    }
}

/// Why `Rig::joint_matrices` composed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RigError {
    /// The local matrices are not one per node: a pose of another rig, or
    /// a truncated buffer.
    LocalCount { nodes: usize, locals: usize },
    /// A joint, or the parent of a node above one, names a node the rig
    /// lacks (a rig built by hand; a loaded one has none).
    MissingNode { node: usize },
}

impl std::fmt::Display for RigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalCount { nodes, locals } => write!(
                f,
                "{locals} local matrices for a rig of {nodes} nodes; it takes one per node"
            ),
            Self::MissingNode { node } => write!(f, "the rig names node {node}, which it lacks"),
        }
    }
}

impl std::error::Error for RigError {}
