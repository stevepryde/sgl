//! Secondary motion on the CPU: hair, tails, straps and loose clothing that
//! lag, swing and settle as the body moves, and stay out of it. A game names
//! chains of a rig's nodes (`Chain`) and sphere and capsule colliders on its
//! body's nodes (`Collider`), and each frame, after sampling its animation
//! into a `Pose`, calls `SpringBones::step` with that pose, the instance's
//! world matrix, a world-space force and its `dt`. The step turns each
//! chain's nodes in the pose and changes nothing else; the pose then goes
//! to `Rig::joint_matrices` and `Scene::set_instance_deformation` as any
//! pose does. It is presentation: it holds no authority, and a server does
//! not run it.
//!
//! Ported from Godot Engine's `SpringBoneSimulator3D`, which implements
//! [VRM's spring bone](https://github.com/vrm-c/vrm-specification/tree/master/specification/VRMC_springBone-1.0),
//! at revision 8f2e46bca3c1cca8f3b759a0eaf67e44082b81a4
//! (<https://github.com/godotengine/godot>, MIT, `LICENSE-godot.txt`):
//! `_init_joints`, `_update_bone_axis`, `get_end_bone_axis` and
//! `_process_joints` of scene/3d/spring_bone_simulator_3d.cpp;
//! `limit_length`, `get_from_to_rotation` and `get_local_pose_rotation` of
//! scene/3d/skeleton_modifier_3d.cpp; and `_collide` of
//! scene/3d/spring_bone_collision_sphere_3d.cpp and
//! scene/3d/spring_bone_collision_capsule_3d.cpp. Modified: translated to
//! Rust over SGL3D's rig and pose, which `deformation` composes; one set of
//! settings per chain, Godot's defaults, with gravity down the world's −Y;
//! every collider acts on every chain; the simulation's centre is the world
//! origin (Godot's default `CENTER_FROM_WORLD_ORIGIN`), and bone axes follow
//! the pose (its default `mutable_bone_axes`). Lengths scale: a bone's
//! length, `tip` and every radius are in their node's space and reach the
//! world through its composed scale and the instance's, where Godot keeps
//! them unscaled, so a rig authored in centimetres under a 0.01 root, or a
//! scaled instance, moves as its world size. Not ported: rotation-axis
//! locks, angle limits, per-joint settings and curves, plane and inside
//! colliders, and a centre node or bone.
//!
//! Two departures (D-100). Godot and VRM integrate once per rendered
//! frame, so drag and stiffness act per frame and the same hair is looser
//! at 144 Hz than at 60. `step` runs fixed steps of `STEP_SECONDS`, carrying
//! the remainder to the next call and placing each step's world matrix
//! between the last call's and this one's at the step's time, runs at most
//! `MAX_STEPS_PER_CALL` a call (dropping the surplus time, so a hitch cannot
//! spiral), and writes each bone's turn between its last two steps' by the
//! remainder. And Godot restores a bone's length after pushing its tail out
//! of a collider, which can put the tail back inside; where it does, SGL
//! places the tail on the circle where the bone's reach meets the
//! collider's surface (`ColliderShape::onto_reach`), and repeats the
//! collider pass `COLLIDER_ROUNDS` times; the pose written between two
//! steps goes through the same pass where it is drawn, so what is drawn
//! stays outside every collider at any call rate.
//!
//! Sheets (`Sheet`: a cape, a skirt) tie side-by-side chains together with
//! links between neighbouring chains' tails at the same depth, each held at
//! the distance the pose gives it, both ways: position-based dynamics with
//! the tails as particles of equal mass (Müller et al. 2007, "Position
//! Based Dynamics", its distance constraint and its collision projection,
//! eq. 5), as Jolt Physics' edge constraint at zero compliance and its
//! `SHEET_ITERATIONS` rounds a step
//! (`SoftBodyMotionProperties::ApplyEdgeConstraints` and
//! `SoftBodyCreationSettings::mNumIterations` at revision
//! cb1a6244912555d12b580265ce7540de262eab44, MIT). Written from the paper's
//! formulas; no Jolt or SPCRJointDynamics code is copied. Each round the
//! links are put back to their lengths, the links collide, and every chain
//! in a sheet is put back root to tip through its bones' lengths and the
//! tail collider pass, so bones never stretch and tails never enter the
//! body. Two departures (D-100). There are no shear links across the grid's
//! cells, which Jolt and SPCRJointDynamics add: bones do not stretch, so a
//! cell with both diagonals fixed is a rigid plate, and a closed sheet of
//! them cannot move. Bones and links alone are the two thread directions of
//! a woven cloth, neither stretching, with nothing across the diagonals (a
//! Chebyshev net; Mack and Taylor 1956, "The fitting of woven cloth to
//! surfaces"), so the sheet shears and folds as cloth does, and each
//! chain's stiffness back to its animated direction is what resists shear.
//! And each link's segment is pushed out of every collider, as
//! SPCRJointDynamics collides its links (revision
//! 7ebe63eb48e7737f3c443511ad2dc2caf1bf96b9, MIT), so a collider wider
//! than the gap between two chains does not wedge them apart and go
//! between them; one narrower than the gap meets only the links and can
//! pass through a cell. The nearest points of a link and a
//! capsule's segment are Godot's `Geometry3D::get_closest_points_between_segments`
//! (core/math/geometry_3d.cpp, at Godot's revision above), ported.
use glam::{Mat4, Quat, Vec3};

use super::animation::Pose;
use super::deformation::{Rig, Transform};

/// The fixed simulation step, in seconds (D-100): drag and stiffness act
/// per step, so a chain moves alike at every frame rate.
const STEP_SECONDS: f32 = 1. / 60.;
/// The most steps one `step` call runs: eight cover a frame down to 7.5 Hz.
/// A longer frame (a hitch, a breakpoint) drops the surplus time instead of
/// catching up, so a slow frame cannot make the next one slower.
const MAX_STEPS_PER_CALL: u32 = 8;
/// How many times each step runs every collider's push over a tail. One
/// pass places a tail on a sphere's surface exactly. A capsule's nearest
/// point moves along its segment as the tail is placed on the circle, so a
/// pass leaves the tail within the capsule by about the square of that
/// movement over the radius; a second pass takes it from there, its own
/// residue the square again (0.7 mm, then 1 µm, in the capsule test), and
/// a third reaches f32 rounding. The same passes let a tail pushed into one
/// collider by the next settle against both.
const COLLIDER_ROUNDS: usize = 3;
/// How many rounds each step runs over a sheet: its links put back to
/// their lengths and collided, then its chains' length restore and collider pass. Jolt's
/// default solver iteration count (`SoftBodyCreationSettings::mNumIterations`),
/// so a sheet's cost per step is fixed by its content.
const SHEET_ITERATIONS: usize = 5;
/// Godot's `CMP_EPSILON`, the tolerance of its `is_zero_approx` and
/// `is_equal_approx`, which the ported code tests against.
const CMP_EPSILON: f32 = 0.00001;

/// A chain of a rig's nodes that swings: a ponytail, a tail, a scarf end, a
/// strap or a coat tail. Each bone runs from a node to the next, and the
/// last from the last node by `tip`; each turns its node. The first node
/// stays where its parent puts it. The four strengths mean what VRM's
/// `stiffness`, `dragForce`, `gravityPower` and `hitRadius` mean, and
/// default to Godot's. `tip` and `radius` are lengths in the node's space
/// and scale into the world with it (its ancestors' scale and the
/// instance's), as the bones' own lengths do; `stiffness` and `gravity` are
/// in world units, as VRM's are, so a scaled character swings at the same
/// rates. A radius scales by the node's uniform scale (the cube root of its
/// determinant); a non-uniform scale is not supported beyond that.
#[derive(Clone, Debug, PartialEq)]
pub struct Chain {
    /// Rig nodes, each the parent of the next. A node belongs to one
    /// chain: forked parts (two pigtails) are chains from the fork's
    /// children. A chain may hang from another chain's node, in any order:
    /// it follows that chain's solved motion.
    pub nodes: Vec<usize>,
    /// The length of a last bone beyond the last node, in the last node's
    /// space, pointing the way the last node lies from its parent (Godot's
    /// `end_bone_length`); zero for none, since glTF does not keep a leaf
    /// bone's tail. Without it the last node does not turn.
    pub tip: f32,
    /// How strongly each bone returns to its animated direction, in world
    /// units. 1 by default.
    pub stiffness: f32,
    /// How much of each bone's motion is lost every step, `0..=1`: 0 swings
    /// on, 1 stops at once. 0.4 by default.
    pub drag: f32,
    /// The strength of the pull down the world's −Y, in world units, as
    /// stiffness's:
    /// a bone held out level with `gravity` equal to `stiffness` settles
    /// halfway between level and straight down. 0 by default.
    pub gravity: f32,
    /// How far each bone's tail stays from every collider's surface, in
    /// the bone's node's space. 0.02 by default.
    pub radius: f32,
}

impl Default for Chain {
    /// No nodes, with Godot's defaults.
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            tip: 0.,
            stiffness: 1.,
            drag: 0.4,
            gravity: 0.,
            radius: 0.02,
        }
    }
}

impl Chain {
    /// The chain from `node` down through only children, until a node has
    /// none or several, with the default settings. A built rig's cycle ends
    /// it once it is as long as there are nodes. At a fork the parts below
    /// are chains of their own, from the fork's children, since a node
    /// belongs to one chain.
    pub fn from_root(rig: &Rig, node: usize) -> Self {
        let mut nodes = vec![node];
        let mut at = node;
        while nodes.len() < rig.nodes.len() {
            let mut children = rig
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, child)| child.parent == Some(at))
                .map(|(index, _)| index);
            match (children.next(), children.next()) {
                (Some(child), None) => {
                    nodes.push(child);
                    at = child;
                }
                _ => break,
            }
        }
        Self {
            nodes,
            ..Self::default()
        }
    }
}

/// Chains that lie side by side and hold together as one surface: a cape,
/// cloak or coat (open), or a skirt (closed). Each chain's bone tails are
/// linked to the next chain's at the same depth, at their distance in the
/// pose `step` is given, as far down as the shorter of the two chains goes;
/// the links keep that distance and collide with the colliders. A chain
/// belongs to at most one sheet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sheet {
    /// Indices into the chains given to `SpringBones::new`, in the order
    /// they lie side by side (whatever their order there).
    pub chains: Vec<usize>,
    /// Whether the last chain is linked to the first, round a body: a
    /// skirt. Needs three chains or more to add a link.
    pub closed: bool,
}

/// A shape on a node of the body that keeps every chain's tails out of it,
/// moving with the node.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Collider {
    pub node: usize,
    pub shape: ColliderShape,
}

/// A collider's shape in its node's space: the node's composed scale and
/// the instance's move its points and grow its radius, by the uniform
/// scale (the cube root of the determinant); a non-uniform scale is not
/// supported beyond that, so a sphere stays a sphere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColliderShape {
    Sphere {
        center: Vec3,
        radius: f32,
    },
    /// The points within `radius` of the segment from `head` to `tail`.
    Capsule {
        head: Vec3,
        tail: Vec3,
        radius: f32,
    },
}

impl ColliderShape {
    /// The shape taken through `matrix`: its points, and its radius by the
    /// matrix's uniform scale.
    fn placed(self, matrix: Mat4) -> Self {
        let scale = uniform_scale(matrix);
        match self {
            Self::Sphere { center, radius } => Self::Sphere {
                center: matrix.transform_point3(center),
                radius: radius * scale,
            },
            Self::Capsule { head, tail, radius } => Self::Capsule {
                head: matrix.transform_point3(head),
                tail: matrix.transform_point3(tail),
                radius: radius * scale,
            },
        }
    }

    /// `point` moved out of the shape grown by `bone_radius`, to its surface
    /// along the line from the nearest point of the sphere's centre or the
    /// capsule's segment (Godot's `_collide`).
    fn collide(self, bone_radius: f32, point: Vec3) -> Vec3 {
        match self {
            Self::Sphere { center, radius } => collide_sphere(center, radius, bone_radius, point),
            Self::Capsule { head, tail, radius } => {
                let axis = tail - head;
                let dot = axis.dot(point - head);
                if dot <= 0. {
                    return collide_sphere(head, radius, bone_radius, point);
                }
                let length_squared = axis.length_squared();
                if length_squared.abs() < CMP_EPSILON {
                    return point;
                }
                if length_squared <= dot {
                    return collide_sphere(tail, radius, bone_radius, point);
                }
                collide_sphere(
                    head + axis * (dot / length_squared),
                    radius,
                    bone_radius,
                    point,
                )
            }
        }
    }

    /// SGL's second departure (D-100): where `tail`, `length` from `head`,
    /// is still within `bone_radius` of the shape after Godot's push and
    /// length restore, the nearest point to it of the circle where the
    /// bone's length sphere meets the shape's surface grown by
    /// `bone_radius`, which satisfies both at once. A capsule takes the
    /// sphere at its segment's point nearest the tail. Where no circle
    /// exists (the head within the grown shape, or the shape beyond the
    /// bone's reach) `tail` stands, as Godot leaves it.
    fn onto_reach(
        self,
        bone_radius: f32,
        head: Vec3,
        length: f32,
        tail: Vec3,
        previous: Vec3,
    ) -> Vec3 {
        let (center, radius) = self.sphere_at(tail);
        let reach = radius + bone_radius;
        if tail.distance(center) >= reach {
            return tail;
        }
        let axis = center - head;
        let distance = axis.length();
        // A head the bone before placed on the surface lies within rounding
        // of it: within Godot's tolerance it counts as outside.
        if !(distance > 0. && distance >= reach - CMP_EPSILON) {
            return tail;
        }
        let axis = axis / distance;
        // The circle's plane lies `along` from the head towards the centre,
        // and the circle has radius `across` in it.
        let along = (distance * distance + length * length - reach * reach) / (2. * distance);
        if along.is_nan() || along.abs() > length {
            return tail;
        }
        let across = (length * length - along * along).max(0.).sqrt();
        let perpendicular = |point: Vec3| {
            let offset = point - head;
            (offset - axis * offset.dot(axis)).normalize_or_zero()
        };
        // A bone pointing straight at the centre has no nearest point: take
        // the previous tail's side, and failing that a fixed one.
        let mut side = perpendicular(tail);
        if side == Vec3::ZERO {
            side = perpendicular(previous);
        }
        if side == Vec3::ZERO {
            side = axis.any_orthonormal_vector();
        }
        head + axis * along + side * across
    }

    /// The sphere that stands for the shape at `point`: a sphere's own, or
    /// for a capsule the sphere of its radius at its segment's point nearest
    /// `point`.
    fn sphere_at(self, point: Vec3) -> (Vec3, f32) {
        match self {
            Self::Sphere { center, radius } => (center, radius),
            Self::Capsule { head, tail, radius } => (
                head + (tail - head) * nearest_along(head, tail, point),
                radius,
            ),
        }
    }

    /// A sheet link's segment from `start` to `end`, `radius` thick, against
    /// the shape: where it enters, the fraction of the way along the
    /// segment of its point deepest in the shape, and the move that takes
    /// that point out to the shape's surface grown by `radius`. A capsule
    /// is the sphere at the point of its segment nearest the link's. `None`
    /// where the link is clear, or runs through the very centre, which
    /// gives no way out (the tails' own collider pass still holds them).
    fn push_segment(self, radius: f32, start: Vec3, end: Vec3) -> Option<(f32, Vec3)> {
        // A cheap reject: the spheres round the link and round the shape
        // grown by `radius` do not meet.
        let (middle, half) = ((start + end) * 0.5, start.distance(end) * 0.5);
        let (bound_center, bound_radius) = match self {
            Self::Sphere { center, radius } => (center, radius),
            Self::Capsule { head, tail, radius } => {
                ((head + tail) * 0.5, head.distance(tail) * 0.5 + radius)
            }
        };
        if middle.distance_squared(bound_center) >= (half + bound_radius + radius).powi(2) {
            return None;
        }
        let (along, center, shape_radius) = match self {
            Self::Sphere { center, radius } => (nearest_along(start, end, center), center, radius),
            Self::Capsule { head, tail, radius } => {
                let (along, at) = closest_between_segments(start, end, head, tail);
                (along, head.lerp(tail, at), radius)
            }
        };
        let offset = start.lerp(end, along) - center;
        let distance = offset.length();
        let reach = shape_radius + radius;
        if !(distance > 0. && distance < reach) {
            return None;
        }
        Some((along, offset * ((reach - distance) / distance)))
    }

    fn is_valid(self) -> bool {
        let (points, radius) = match self {
            Self::Sphere { center, radius } => ([center, center], radius),
            Self::Capsule { head, tail, radius } => ([head, tail], radius),
        };
        points.iter().all(|point| point.is_finite()) && radius.is_finite() && radius >= 0.
    }
}

/// Godot's `SpringBoneCollisionSphere3D::_collide_sphere`, outside only.
fn collide_sphere(origin: Vec3, radius: f32, bone_radius: f32, point: Vec3) -> Vec3 {
    let offset = point - origin;
    let reach = bone_radius + radius;
    if offset.length() - reach > 0. {
        point
    } else {
        origin + offset.normalize_or_zero() * reach
    }
}

/// How far along the segment from `start` to `end` its point nearest
/// `point` lies, `0..=1`; 0 for a segment of no length.
fn nearest_along(start: Vec3, end: Vec3, point: Vec3) -> f32 {
    let axis = end - start;
    let length_squared = axis.length_squared();
    if length_squared > 0. {
        (axis.dot(point - start) / length_squared).clamp(0., 1.)
    } else {
        0.
    }
}

/// Godot's `Geometry3D::get_closest_points_between_segments`
/// (core/math/geometry_3d.cpp at revision
/// 8f2e46bca3c1cca8f3b759a0eaf67e44082b81a4, based, as Godot says, on David
/// Eberly's computation of the distance between line segments): the
/// nearest points of the segment from `p0` to `p1` and the segment from
/// `q0` to `q1`, as the fractions of the way along each. Modified: returns
/// the fractions, from which Godot computes the points; and takes segments
/// as parallel by their angle (the squared sine under `CMP_EPSILON`),
/// where Godot compares the determinant, a length to the fourth power,
/// with `CMP_EPSILON` itself, which takes every pair of short segments as
/// parallel whatever their angle: a link and a capsule of a character at a
/// quarter of its size.
fn closest_between_segments(p0: Vec3, p1: Vec3, q0: Vec3, q1: Vec3) -> (f32, f32) {
    let p = p1 - p0;
    let q = q1 - q0;
    let r = p0 - q0;
    let a = p.dot(p);
    let b = p.dot(q);
    let c = q.dot(q);
    let d = p.dot(r);
    let e = q.dot(r);
    let det = a * c - b * b;
    // The fraction along the first segment nearest the second's point at
    // `numerator / a` clamped: Godot's repeated ternaries.
    let clamped = |numerator: f32| {
        if numerator <= 0. {
            0.
        } else if numerator >= a {
            1.
        } else {
            numerator / a
        }
    };
    if det > CMP_EPSILON * a * c {
        // Non-parallel segments.
        let bte = b * e;
        let ctd = c * d;
        if bte <= ctd {
            // s <= 0
            if e <= 0. {
                (clamped(-d), 0.)
            } else if e < c {
                (0., e / c)
            } else {
                (clamped(b - d), 1.)
            }
        } else {
            let s = bte - ctd;
            if s >= det {
                // s >= 1
                if b + e <= 0. {
                    (clamped(-d), 0.)
                } else if b + e < c {
                    (1., (b + e) / c)
                } else {
                    (clamped(b - d), 1.)
                }
            } else {
                // 0 < s < 1
                let ate = a * e;
                let btd = b * d;
                if ate <= btd {
                    (clamped(-d), 0.)
                } else {
                    let t = ate - btd;
                    if t >= det {
                        (clamped(b - d), 1.)
                    } else {
                        (s / det, t / det)
                    }
                }
            }
        }
    } else if e <= 0. {
        // Parallel segments.
        (clamped(-d), 0.)
    } else if e >= c {
        (clamped(b - d), 1.)
    } else {
        (0., e / c)
    }
}

/// A chain's setting that `SpringBones::new` refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainField {
    Tip,
    Stiffness,
    Drag,
    Gravity,
    Radius,
}

/// Why `SpringBones::new` refused its chains or colliders. Indices count
/// from zero, in the order they were given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpringBonesError {
    /// Chain `chain` names `node`, which the rig lacks.
    ChainNode { chain: usize, node: usize },
    /// Chain `chain`'s `node` is not the child of the node before it.
    NotChild { chain: usize, node: usize },
    /// Chains `first` and `second` both name `node`: a node is turned by
    /// one chain, and forked parts start at the fork's children.
    SharedNode {
        first: usize,
        second: usize,
        node: usize,
    },
    /// Chain `chain`'s `field` is not finite or is negative, or for `drag`
    /// is above 1.
    ChainValue { chain: usize, field: ChainField },
    /// Sheet `sheet` names chain `chain`, which was not given.
    SheetChain { sheet: usize, chain: usize },
    /// Sheets `first` and `second` both name chain `chain`, or one sheet
    /// names it twice (`first == second`): a chain belongs to one sheet.
    SharedChain {
        first: usize,
        second: usize,
        chain: usize,
    },
    /// Collider `collider` names `node`, which the rig lacks.
    ColliderNode { collider: usize, node: usize },
    /// Collider `collider`'s shape has a point that is not finite, or a
    /// radius that is not finite or is negative.
    ColliderShape { collider: usize },
}

impl std::fmt::Display for SpringBonesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChainNode { chain, node } => {
                write!(f, "chain {chain} names node {node}, which the rig lacks")
            }
            Self::NotChild { chain, node } => write!(
                f,
                "chain {chain}'s node {node} is not the child of the node before it"
            ),
            Self::SharedNode {
                first,
                second,
                node,
            } => write!(
                f,
                "chains {first} and {second} both name node {node}; a node belongs to one chain"
            ),
            Self::ChainValue { chain, field } => {
                let (name, range) = match field {
                    ChainField::Tip => ("tip", "finite and nonnegative"),
                    ChainField::Stiffness => ("stiffness", "finite and nonnegative"),
                    ChainField::Drag => ("drag", "within 0..=1"),
                    ChainField::Gravity => ("gravity", "finite and nonnegative"),
                    ChainField::Radius => ("radius", "finite and nonnegative"),
                };
                write!(f, "chain {chain}'s {name} must be {range}")
            }
            Self::SheetChain { sheet, chain } => {
                write!(f, "sheet {sheet} names chain {chain}, which was not given")
            }
            Self::SharedChain {
                first,
                second,
                chain,
            } => write!(
                f,
                "sheets {first} and {second} both name chain {chain}; a chain belongs to one sheet"
            ),
            Self::ColliderNode { collider, node } => {
                write!(
                    f,
                    "collider {collider} names node {node}, which the rig lacks"
                )
            }
            Self::ColliderShape { collider } => write!(
                f,
                "collider {collider}'s points must be finite and its radius finite and nonnegative"
            ),
        }
    }
}

impl std::error::Error for SpringBonesError {}

/// Chains of a rig's nodes swinging under their colliders, and their state
/// between frames: one per instance. Each frame, sample the instance's
/// pose, call `step` with it, then pose the instance from it. Allocates
/// nothing in `step` once constructed.
#[derive(Clone, Debug)]
pub struct SpringBones {
    /// Every chain's bones, chain after chain, each chain root first.
    bones: Vec<Bone>,
    /// Every sheet's links between bone tails.
    links: Vec<Link>,
    colliders: Vec<Collider>,
    /// Each collider's node's transform in the asset's space this call.
    collider_nodes: Vec<Mat4>,
    /// Each collider in world space at the step being run.
    placed_colliders: Vec<ColliderShape>,
    /// How many nodes the rig has.
    node_count: usize,
    /// Each node's matrix relative to its parent: the pose's, with the
    /// chains' nodes turned as far as the solve has reached.
    locals: Vec<Mat4>,
    /// `Rig::global`'s composed nodes and ancestor walk.
    globals: Vec<Option<Mat4>>,
    walk: Vec<usize>,
    /// The world matrix of the last call.
    world: Placement,
    /// The time since the last step, under `STEP_SECONDS`.
    elapsed: f32,
    /// Whether the bones hold a solve; false until the first step and
    /// after `reset`.
    started: bool,
}

/// One bone of a chain: Godot's joint and its Verlet state.
#[derive(Clone, Copy, Debug)]
struct Bone {
    /// The node the bone turns.
    node: usize,
    /// The next node of the chain, which the bone runs to; `None` for the
    /// last node, whose bone is the chain's tip.
    child: Option<usize>,
    stiffness: f32,
    drag: f32,
    gravity: f32,
    radius: f32,
    tip: f32,
    /// Whether the bone has a length at rest and swings: Godot gives a
    /// joint without one no Verlet state.
    simulated: bool,
    /// Whether the bone's chain is in a sheet or hangs below one that is,
    /// so it is put back after the sheets' links each round.
    restored: bool,
    /// Where a restored bone's head is found in the sheets' rounds before
    /// the last.
    head: Head,
    /// The bone's head in the world, and its length and radius there, from
    /// the step's first pass, which the sheets' rounds hold; `None` where
    /// the rig could not place the bone.
    reach: Option<Reach>,
    /// The bone's direction in its node's space, a unit vector, and its
    /// length (Godot's `forward_vector` and `length`), from the latest pose.
    forward: Vec3,
    length: f32,
    /// The node's transform relative to its parent in this call's pose, and
    /// its matrix.
    animated: Transform,
    animated_matrix: Mat4,
    /// The tail's world position at the last two steps (Godot's
    /// `current_tail` and `prev_tail`).
    tail: Vec3,
    previous_tail: Vec3,
    /// The turn from the bone's animated direction to its solved one, in
    /// the asset's space, at the last two steps (Godot's `current_rot`).
    turn: Quat,
    previous_turn: Quat,
}

impl Bone {
    /// The bone's direction and length from `pose`, as Godot's
    /// `_update_bone_axis` and `get_end_bone_axis` take them: the next
    /// node's translation; for the tip, the last node's translation in its
    /// rest frame, `tip` long. A zero axis keeps the last.
    fn update_axis(&mut self, rig: &Rig, pose: &Pose) {
        let (axis, length) = match self.child {
            Some(child) => {
                let axis = pose.nodes[child].translation;
                (axis, axis.length())
            }
            None => (
                rig.nodes[self.node].rest.rotation.inverse() * pose.nodes[self.node].translation,
                self.tip,
            ),
        };
        if !is_zero_approx(axis) {
            self.forward = axis.normalize();
            self.length = length;
        }
    }

    /// The bone's tail in the asset's space, its node at `global`.
    fn tail_at(&self, global: Mat4) -> Vec3 {
        global.transform_point3(self.forward * self.length)
    }
}

/// Where a bone's head lies in a sheet's rounds before the last, which
/// compose no frames: the head moves only as the tails above it do, and
/// its world length and radius stay the first pass's, since the rounds only
/// turn bones. The last round composes every frame through the rig.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Head {
    /// A chain's first bone below no bone a sheet moves: where the first
    /// pass placed it.
    Root,
    /// At the tail of the bone before it in the chain, which swings.
    Above,
    /// Composed through the rig every round: a chain's first bone hanging
    /// from a node of a restored chain, a bone below one that does not
    /// swing, and every restored bone above those.
    Composed,
}

/// A sheet's link between two bones' tails at the same depth, which keeps
/// them its length apart.
#[derive(Clone, Copy, Debug)]
struct Link {
    /// The two bones, indices into `SpringBones::bones`.
    bones: [usize; 2],
    /// The second tail less the first in the asset's space in this call's
    /// pose, which the step's world takes to the link's length.
    offset: Vec3,
    /// The link's length in the world at the step being run.
    rest: f32,
    /// How far the link's segment stays from every collider's surface: the
    /// larger of its two bones' radii in the world, so two chains of
    /// different radii keep the thicker one's clearance between them.
    radius: f32,
}

/// A world matrix as a scale, a rotation and a translation, so that two
/// can be interpolated.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placement {
    scale: Vec3,
    rotation: Quat,
    translation: Vec3,
}

impl Placement {
    const IDENTITY: Self = Self {
        scale: Vec3::ONE,
        rotation: Quat::IDENTITY,
        translation: Vec3::ZERO,
    };

    fn of(matrix: Mat4) -> Self {
        let (scale, rotation, translation) = matrix.to_scale_rotation_translation();
        Self {
            scale,
            rotation: rotation.normalize(),
            translation,
        }
    }

    fn is_finite(&self) -> bool {
        self.scale.is_finite() && self.rotation.is_finite() && self.translation.is_finite()
    }

    /// The placement the fraction `t` of the way from `self` to `to`.
    fn lerp(&self, to: &Self, t: f32) -> Self {
        Self {
            scale: self.scale.lerp(to.scale, t),
            rotation: self.rotation.slerp(to.rotation, t),
            translation: self.translation.lerp(to.translation, t),
        }
    }

    fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// The inverse of the matrix's basis applied to `direction` (Godot's
    /// `p_inverted_center_transform.basis.xform`).
    fn inverse_direction(&self, direction: Vec3) -> Vec3 {
        (self.rotation.inverse() * direction) / self.scale
    }
}

/// A node's place in the asset's space as the solve has posed it.
#[derive(Clone, Copy, Debug)]
struct Frame {
    /// The node's transform in the asset's space.
    matrix: Mat4,
    rotation: Quat,
    parent_rotation: Quat,
}

impl SpringBones {
    /// Chains, the sheets that tie them, and colliders on `rig`, at rest
    /// until the first `step`. Refuses a node the rig lacks, a chain node
    /// that is not its predecessor's child, a node two chains name, a chain
    /// a sheet names that was not given or that two sheets (or one twice)
    /// name, and a chain value or collider shape that is not finite or is
    /// out of range, saying which.
    pub fn new(
        rig: &Rig,
        chains: &[Chain],
        sheets: &[Sheet],
        colliders: &[Collider],
    ) -> Result<Self, SpringBonesError> {
        let node_count = rig.nodes.len();
        // The chain that names each node: a node is turned by one chain.
        let mut owners: Vec<Option<usize>> = vec![None; node_count];
        for (index, chain) in chains.iter().enumerate() {
            let nonnegative = |value: f32| value.is_finite() && value >= 0.;
            for (field, valid) in [
                (ChainField::Tip, nonnegative(chain.tip)),
                (ChainField::Stiffness, nonnegative(chain.stiffness)),
                (ChainField::Drag, (0. ..=1.).contains(&chain.drag)),
                (ChainField::Gravity, nonnegative(chain.gravity)),
                (ChainField::Radius, nonnegative(chain.radius)),
            ] {
                if !valid {
                    return Err(SpringBonesError::ChainValue {
                        chain: index,
                        field,
                    });
                }
            }
            for (position, &node) in chain.nodes.iter().enumerate() {
                if node >= node_count {
                    return Err(SpringBonesError::ChainNode { chain: index, node });
                }
                if position > 0 && rig.nodes[node].parent != Some(chain.nodes[position - 1]) {
                    return Err(SpringBonesError::NotChild { chain: index, node });
                }
                if let Some(first) = owners[node] {
                    return Err(SpringBonesError::SharedNode {
                        first,
                        second: index,
                        node,
                    });
                }
                owners[node] = Some(index);
            }
        }
        // The sheet that names each chain: a chain belongs to one sheet.
        let mut sheet_of: Vec<Option<usize>> = vec![None; chains.len()];
        for (index, sheet) in sheets.iter().enumerate() {
            for &chain in &sheet.chains {
                if chain >= chains.len() {
                    return Err(SpringBonesError::SheetChain {
                        sheet: index,
                        chain,
                    });
                }
                if let Some(first) = sheet_of[chain] {
                    return Err(SpringBonesError::SharedChain {
                        first,
                        second: index,
                        chain,
                    });
                }
                sheet_of[chain] = Some(index);
            }
        }
        // Solved by the depth of their first node in the rig, the given
        // order otherwise: a chain hanging from another chain's node starts
        // deeper than that chain, so it follows that chain's solved motion
        // whatever order the game gives.
        let depth = |node: usize| {
            let mut at = node;
            let mut depth = 0;
            while let Some(parent) = rig.nodes[at]
                .parent
                .filter(|&parent| parent < node_count && depth < node_count)
            {
                at = parent;
                depth += 1;
            }
            depth
        };
        let mut order: Vec<usize> = (0..chains.len()).collect();
        order.sort_by_key(|&index| chains[index].nodes.first().map_or(0, |&node| depth(node)));
        let mut bones = Vec::new();
        // Each chain's first bone in `bones`, and whether it is put back
        // after the sheets' links: it is in a sheet, or hangs from a node
        // of a chain that is (solved before it, being shallower).
        let mut starts = vec![0; chains.len()];
        let mut restored = vec![false; chains.len()];
        for &index in &order {
            let chain = &chains[index];
            starts[index] = bones.len();
            let hangs = {
                let mut above = chain.nodes.first().and_then(|&node| rig.nodes[node].parent);
                let mut steps = 0;
                let mut follows = false;
                while let Some(node) = above.filter(|&node| node < node_count && steps < node_count)
                {
                    if owners[node].is_some_and(|owner| restored[owner]) {
                        follows = true;
                        break;
                    }
                    above = rig.nodes[node].parent;
                    steps += 1;
                }
                follows
            };
            restored[index] = sheet_of[index].is_some() || hangs;
            for (position, &node) in chain.nodes.iter().enumerate() {
                let child = chain.nodes.get(position + 1).copied();
                // Godot's `_init_joints`: a joint whose bone has no length
                // at rest gets no Verlet state.
                let (axis, length) = match child {
                    Some(child) => {
                        let axis = rig.nodes[child].rest.translation;
                        (axis, axis.length())
                    }
                    None if chain.tip > 0. => {
                        let rest = rig.nodes[node].rest;
                        (rest.rotation.inverse() * rest.translation, chain.tip)
                    }
                    None => (Vec3::ZERO, 0.),
                };
                let simulated = !is_zero_approx(axis);
                let animated = rig.nodes[node].rest;
                let head = if position == 0 {
                    if hangs { Head::Composed } else { Head::Root }
                } else if bones.last().is_some_and(|above: &Bone| above.simulated) {
                    Head::Above
                } else {
                    Head::Composed
                };
                bones.push(Bone {
                    node,
                    child,
                    stiffness: chain.stiffness,
                    drag: chain.drag,
                    gravity: chain.gravity,
                    radius: chain.radius,
                    tip: chain.tip,
                    simulated,
                    restored: restored[index],
                    head,
                    reach: None,
                    forward: axis.normalize_or_zero(),
                    length,
                    animated,
                    animated_matrix: animated.matrix(),
                    tail: Vec3::ZERO,
                    previous_tail: Vec3::ZERO,
                    turn: Quat::IDENTITY,
                    previous_turn: Quat::IDENTITY,
                });
            }
        }
        // A composed head needs every restored bone above it composed in
        // the same round.
        let mut bone_of: Vec<Option<usize>> = vec![None; node_count];
        for (index, bone) in bones.iter().enumerate() {
            bone_of[bone.node] = Some(index);
        }
        for index in 0..bones.len() {
            if !(bones[index].restored && bones[index].head == Head::Composed) {
                continue;
            }
            let mut above = rig.nodes[bones[index].node].parent;
            let mut steps = 0;
            while let Some(node) = above.filter(|&node| node < node_count && steps < node_count) {
                if let Some(bone) = bone_of[node].filter(|&bone| bones[bone].restored) {
                    bones[bone].head = Head::Composed;
                }
                above = rig.nodes[node].parent;
                steps += 1;
            }
        }
        // Each pair of neighbouring chains' tails at one depth, down to the
        // shorter chain's last bone, between bones that swing.
        let mut links = Vec::new();
        for sheet in sheets {
            let closing = (sheet.closed && sheet.chains.len() > 2)
                .then(|| [sheet.chains[sheet.chains.len() - 1], sheet.chains[0]]);
            let pairs = sheet
                .chains
                .windows(2)
                .map(|pair| [pair[0], pair[1]])
                .chain(closing);
            for [a, b] in pairs {
                let depth = chains[a].nodes.len().min(chains[b].nodes.len());
                for at in 0..depth {
                    let bones_ab = [starts[a] + at, starts[b] + at];
                    if bones_ab.iter().all(|&bone| bones[bone].simulated) {
                        links.push(Link {
                            bones: bones_ab,
                            offset: Vec3::ZERO,
                            rest: 0.,
                            radius: 0.,
                        });
                    }
                }
            }
        }
        for (index, collider) in colliders.iter().enumerate() {
            if collider.node >= node_count {
                return Err(SpringBonesError::ColliderNode {
                    collider: index,
                    node: collider.node,
                });
            }
            if !collider.shape.is_valid() {
                return Err(SpringBonesError::ColliderShape { collider: index });
            }
        }
        Ok(Self {
            bones,
            links,
            colliders: colliders.to_vec(),
            collider_nodes: vec![Mat4::IDENTITY; colliders.len()],
            placed_colliders: colliders.iter().map(|collider| collider.shape).collect(),
            node_count,
            locals: Vec::with_capacity(node_count),
            globals: vec![None; node_count],
            walk: Vec::with_capacity(node_count),
            world: Placement::IDENTITY,
            elapsed: 0.,
            started: false,
        })
    }

    /// Advances the chains by `dt` seconds and turns their nodes in `pose`.
    ///
    /// `pose` is the instance's pose as animated this frame (sampled, or
    /// the rest pose): each chain node's rotation is its animated direction,
    /// which stiffness pulls towards, so give a fresh pose every call,
    /// never last frame's output. `world` is the instance's model-to-world
    /// matrix this frame, taken as a scale, a rotation and a translation, so
    /// walking, turning and jumping move the chains by inertia. `force` is a
    /// world-space push on every bone, in world units as gravity's: vary it
    /// for wind.
    ///
    /// The chains move in fixed steps (D-100), so they move alike at any
    /// frame rate; a call of more than eight steps' time runs eight, the
    /// frame's movement spread across them. A
    /// non-finite or non-positive `dt` runs no step and still writes the
    /// pose. A non-finite `world` or `force` resets the chains (`reset`)
    /// and leaves the pose as it was given, as does a state that the
    /// inputs drove past `f32`'s range. A pose or rig with another number of
    /// nodes than the rig given to `new` is left alone.
    pub fn step(&mut self, rig: &Rig, pose: &mut Pose, world: Mat4, force: Vec3, dt: f32) {
        if rig.nodes.len() != self.node_count || pose.nodes.len() != self.node_count {
            return;
        }
        let placement = Placement::of(world);
        if !world.is_finite() || !force.is_finite() || !placement.is_finite() {
            self.reset();
            return;
        }
        pose.local_matrices(&mut self.locals);
        for bone in &mut self.bones {
            bone.animated = pose.nodes[bone.node];
            bone.animated_matrix = self.locals[bone.node];
            bone.update_axis(rig, pose);
        }
        self.globals.fill(None);
        for (matrix, collider) in self.collider_nodes.iter_mut().zip(&self.colliders) {
            match rig.global(
                collider.node,
                &self.locals,
                &mut self.globals,
                &mut self.walk,
            ) {
                Ok(global) => *matrix = global,
                Err(_) => return,
            }
        }
        // Each link's length is its tails' distance in this pose, as each
        // bone's is its own.
        for link in &mut self.links {
            let [a, b] = link.bones.map(|bone| &self.bones[bone]);
            let mut tail = |bone: &Bone| {
                rig.global(bone.node, &self.locals, &mut self.globals, &mut self.walk)
                    .map(|global| bone.tail_at(global))
            };
            match (tail(a), tail(b)) {
                (Ok(a), Ok(b)) => link.offset = b - a,
                _ => return,
            }
        }
        if !self.started {
            self.start(rig, placement);
        }
        if dt.is_finite() && dt > 0. {
            let total = self.elapsed + dt;
            let due = (total / STEP_SECONDS).floor();
            let capped = due > MAX_STEPS_PER_CALL as f32;
            let steps = if capped {
                MAX_STEPS_PER_CALL
            } else {
                due as u32
            };
            for step in 0..steps {
                // How far through the call the step's world lies: at its
                // own time, or, for a hitch whose surplus time is dropped,
                // spread evenly over the steps run, so no step sees most of
                // the frame's movement at once.
                let at = if capped {
                    (step + 1) as f32 / steps as f32
                } else {
                    (STEP_SECONDS - self.elapsed + step as f32 * STEP_SECONDS) / dt
                };
                let world = self.world.lerp(&placement, at.clamp(0., 1.));
                self.simulate(rig, &world, force);
            }
            self.elapsed = if capped {
                0.
            } else {
                (total - steps as f32 * STEP_SECONDS).clamp(0., STEP_SECONDS)
            };
        }
        self.world = placement;
        let finite = self.bones.iter().all(|bone| {
            bone.tail.is_finite()
                && bone.previous_tail.is_finite()
                && bone.turn.is_finite()
                && bone.previous_turn.is_finite()
        });
        if !finite {
            self.reset();
            return;
        }
        self.write(rig, pose, &placement);
    }

    /// Starts the next `step` at rest in the pose it is given, as for a
    /// spawn, a teleport or a camera cut: no motion carries over.
    pub fn reset(&mut self) {
        self.started = false;
    }

    /// Godot's `_init_joints`: each tail where the animated pose puts it.
    fn start(&mut self, rig: &Rig, world: Placement) {
        let world_matrix = world.matrix();
        self.globals.fill(None);
        for bone in &mut self.bones {
            bone.turn = Quat::IDENTITY;
            bone.previous_turn = Quat::IDENTITY;
            if !bone.simulated {
                continue;
            }
            if let Ok(global) =
                rig.global(bone.node, &self.locals, &mut self.globals, &mut self.walk)
            {
                let tail = world_matrix
                    .transform_point3(global.transform_point3(bone.forward * bone.length));
                bone.tail = tail;
                bone.previous_tail = tail;
            }
        }
        self.world = world;
        self.elapsed = 0.;
        self.started = true;
    }

    /// One fixed step of every chain under `world` (Godot's
    /// `_process_joints`), chain after chain, each bone after its parent;
    /// then `SHEET_ITERATIONS` rounds of the sheets' links put back to
    /// their lengths and collided, each followed by every chain in a sheet or below one put
    /// back root to tip. Only the last round composes frames and derives
    /// turns; the rounds before take each bone's head from the tail above it
    /// (`Head`) and its length and radius from the first pass.
    fn simulate(&mut self, rig: &Rig, world: &Placement, force: Vec3) {
        let Self {
            bones,
            links,
            collider_nodes,
            colliders,
            placed_colliders,
            locals,
            globals,
            walk,
            ..
        } = self;
        let world_matrix = world.matrix();
        place(placed_colliders, colliders, collider_nodes, world_matrix);
        for bone in bones.iter() {
            locals[bone.node] = bone.animated_matrix;
        }
        globals.fill(None);
        for bone in bones.iter_mut() {
            bone.reach = None;
            if !bone.simulated {
                continue;
            }
            let Some(frame) = frame(rig, bone.node, locals, globals, walk) else {
                continue;
            };
            let reach = Reach::of(bone, world_matrix, frame);
            // Godot rotates the external force into the centre's space and
            // back; with the world origin as centre it stays in world space.
            let external = (force + Vec3::NEG_Y * bone.gravity) * STEP_SECONDS;
            // Integration of velocity by Verlet. Stiffness, as gravity, is
            // in world units, along the animated direction.
            let next = bone.tail
                + (bone.tail - bone.previous_tail) * (1. - bone.drag)
                + reach.axis.normalize_or_zero() * (bone.stiffness * STEP_SECONDS)
                + external;
            bone.previous_tail = bone.tail;
            bone.previous_turn = bone.turn;
            settle(bone, next, frame, &reach, world, placed_colliders);
            locals[bone.node] = solved(bone, bone.turn, frame).matrix();
            globals[bone.node] = None;
            bone.reach = Some(reach);
        }
        if links.is_empty() {
            return;
        }
        for link in links.iter_mut() {
            link.rest = world_matrix.transform_vector3(link.offset).length();
            let [a, b] = link
                .bones
                .map(|bone| bones[bone].reach.map_or(0., |reach| reach.radius));
            link.radius = a.max(b);
        }
        let composed = bones
            .iter()
            .any(|bone| bone.restored && bone.head == Head::Composed);
        for round in 0..SHEET_ITERATIONS {
            pull(bones, links);
            collide_links(bones, links, placed_colliders);
            // The chains the links moved, back onto their bones and out of
            // the colliders, from the solved pose above them.
            let last = round + 1 == SHEET_ITERATIONS;
            let composes = |bone: &Bone| bone.restored && (last || bone.head == Head::Composed);
            if last || composed {
                for bone in bones.iter().filter(|bone| composes(bone)) {
                    locals[bone.node] = bone.animated_matrix;
                }
                globals.fill(None);
            }
            for index in 0..bones.len() {
                let bone = &bones[index];
                if !(bone.restored && bone.simulated) {
                    continue;
                }
                if composes(bone) {
                    let bone = &mut bones[index];
                    let Some(frame) = frame(rig, bone.node, locals, globals, walk) else {
                        continue;
                    };
                    let reach = Reach::of(bone, world_matrix, frame);
                    settle(bone, bone.tail, frame, &reach, world, placed_colliders);
                    locals[bone.node] = solved(bone, bone.turn, frame).matrix();
                    globals[bone.node] = None;
                } else if let Some(reach) = bone.reach {
                    let origin = match bone.head {
                        Head::Above => bones[index - 1].tail,
                        _ => reach.origin,
                    };
                    let tail = onto_bone(
                        placed_colliders,
                        reach.radius,
                        origin,
                        reach.length,
                        bone.tail,
                        bone.previous_tail,
                    );
                    bones[index].tail = tail;
                }
            }
        }
    }

    /// Turns the chains' nodes in `pose` by each bone's turn between its
    /// last two steps, the fraction of a step since the last. A turn between
    /// two collision-free steps can dip into a collider, so each bone's tail
    /// in the configuration drawn (this call's world, its parents as
    /// written) goes through the step's collider pass against the colliders
    /// placed there, and the pose takes the turn to the result; the state is
    /// unchanged.
    fn write(&mut self, rig: &Rig, pose: &mut Pose, world: &Placement) {
        let Self {
            bones,
            colliders,
            collider_nodes,
            placed_colliders,
            locals,
            globals,
            walk,
            elapsed,
            ..
        } = self;
        let world_matrix = world.matrix();
        place(placed_colliders, colliders, collider_nodes, world_matrix);
        for bone in bones.iter() {
            locals[bone.node] = bone.animated_matrix;
        }
        globals.fill(None);
        let alpha = *elapsed / STEP_SECONDS;
        for bone in bones.iter() {
            if !bone.simulated {
                continue;
            }
            let Some(frame) = frame(rig, bone.node, locals, globals, walk) else {
                continue;
            };
            let mut turn = bone.previous_turn.slerp(bone.turn, alpha);
            let reach = Reach::of(bone, world_matrix, frame);
            let from = frame
                .matrix
                .transform_vector3(bone.forward)
                .normalize_or_zero();
            let direction = world_matrix
                .transform_vector3(turn * from)
                .normalize_or_zero();
            let tail = reach.origin + direction * reach.length;
            let kept = keep_out(
                placed_colliders,
                reach.radius,
                reach.origin,
                reach.length,
                tail,
                bone.tail,
            );
            if kept != tail {
                let to = world
                    .inverse_direction(kept - reach.origin)
                    .normalize_or_zero();
                turn = from_to_rotation(from, to, turn);
            }
            let local = solved(bone, turn, frame);
            locals[bone.node] = local.matrix();
            globals[bone.node] = None;
            pose.nodes[bone.node].rotation = local.rotation;
        }
    }
}

/// A bone's head in the world and its length and radius there. Lengths are
/// in the node's space and scale with it (D-100), where Godot keeps them
/// unscaled: the bone is its animated axis taken into the world, and its
/// radius scales by the node's uniform scale.
#[derive(Clone, Copy, Debug)]
struct Reach {
    origin: Vec3,
    /// The bone's animated axis in the world.
    axis: Vec3,
    length: f32,
    radius: f32,
}

impl Reach {
    fn of(bone: &Bone, world: Mat4, frame: Frame) -> Self {
        let to_world = world * frame.matrix;
        let axis = to_world.transform_vector3(bone.forward * bone.length);
        Self {
            origin: to_world.w_axis.truncate(),
            axis,
            length: axis.length(),
            radius: bone.radius * uniform_scale(to_world),
        }
    }
}

/// The bone's tail taken from `tail` onto its length from its head and
/// through the collider pass, and its turn derived from where the tail
/// lands: the turn, in the asset's space, from the bone's animated
/// direction there to the solved one taken back from the world.
fn settle(
    bone: &mut Bone,
    tail: Vec3,
    frame: Frame,
    reach: &Reach,
    world: &Placement,
    colliders: &[ColliderShape],
) {
    let tail = onto_bone(
        colliders,
        reach.radius,
        reach.origin,
        reach.length,
        tail,
        bone.previous_tail,
    );
    bone.tail = tail;
    let from = frame
        .matrix
        .transform_vector3(bone.forward)
        .normalize_or_zero();
    let to = world
        .inverse_direction(tail - reach.origin)
        .normalize_or_zero();
    bone.turn = from_to_rotation(from, to, bone.turn);
}

/// One pass of the sheets' links over the bones' tails, in order: each pair
/// of tails moved along the line between them, half each, to the link's
/// length, whether they are further apart or closer (the position-based
/// distance constraint for two equal masses, Jolt's
/// `ApplyEdgeConstraints` at zero compliance: rigid).
fn pull(bones: &mut [Bone], links: &[Link]) {
    for link in links {
        let [a, b] = link.bones;
        let delta = bones[b].tail - bones[a].tail;
        let length = delta.length();
        if !length.is_normal() {
            continue;
        }
        let correction = delta * ((length - link.rest) / (2. * length));
        bones[a].tail += correction;
        bones[b].tail -= correction;
    }
}

/// One pass of the sheets' links against the colliders: where a link's
/// segment enters a collider grown by the link's radius, its two tails
/// move along the way out so that the segment's deepest point lands on
/// the surface. With that point the fraction `t` along the link and `push`
/// the move it needs, the position-based projection of the point for two
/// equal masses (Müller et al. 2007, eq. 5) moves the first tail by
/// `push (1 − t) / ((1 − t)² + t²)` and the second by `push t / ((1 − t)² + t²)`,
/// which moves the point by exactly `push`.
fn collide_links(bones: &mut [Bone], links: &[Link], colliders: &[ColliderShape]) {
    for link in links {
        let [a, b] = link.bones;
        for collider in colliders {
            let Some((along, push)) =
                collider.push_segment(link.radius, bones[a].tail, bones[b].tail)
            else {
                continue;
            };
            let weight = (1. - along) * (1. - along) + along * along;
            bones[a].tail += push * ((1. - along) / weight);
            bones[b].tail += push * (along / weight);
        }
    }
}

/// Each collider placed in the world through `world` and its node.
fn place(placed: &mut [ColliderShape], colliders: &[Collider], nodes: &[Mat4], world: Mat4) {
    for ((placed, collider), node) in placed.iter_mut().zip(colliders).zip(nodes) {
        *placed = collider.shape.placed(world * *node);
    }
}

/// `tail` taken onto the bone `length` from `origin`, then through the
/// collider pass.
fn onto_bone(
    colliders: &[ColliderShape],
    radius: f32,
    origin: Vec3,
    length: f32,
    tail: Vec3,
    previous: Vec3,
) -> Vec3 {
    let tail = limit_length(origin, tail, length);
    keep_out(colliders, radius, origin, length, tail, previous)
}

/// The collider pass over a tail `length` from `origin`, `radius` thick:
/// `COLLIDER_ROUNDS` times, each collider's Godot push and length restore,
/// then SGL's placement on the circle where the two meet.
fn keep_out(
    colliders: &[ColliderShape],
    radius: f32,
    origin: Vec3,
    length: f32,
    mut tail: Vec3,
    previous: Vec3,
) -> Vec3 {
    for _ in 0..COLLIDER_ROUNDS {
        for collider in colliders {
            tail = collider.collide(radius, tail);
            tail = limit_length(origin, tail, length);
            tail = collider.onto_reach(radius, origin, length, tail, previous);
        }
    }
    tail
}

/// Where `node` is in the asset's space with `locals`, composed by the rig.
/// `None` for a node or parent the rig cannot compose or whose rotation is
/// not finite (an axis scaled to zero).
fn frame(
    rig: &Rig,
    node: usize,
    locals: &[Mat4],
    globals: &mut [Option<Mat4>],
    walk: &mut Vec<usize>,
) -> Option<Frame> {
    let global = rig.global(node, locals, globals, walk).ok()?;
    let parent_rotation = match rig.nodes[node].parent {
        Some(parent) => rotation_of(rig.global(parent, locals, globals, walk).ok()?)?,
        None => Quat::IDENTITY,
    };
    Some(Frame {
        matrix: global,
        rotation: rotation_of(global)?,
        parent_rotation,
    })
}

/// The one factor a matrix scales lengths by: the cube root of its 3×3's
/// absolute determinant, exact for a uniform scale. Under a non-uniform
/// scale a radius takes this mean, so a sphere stays a sphere.
fn uniform_scale(matrix: Mat4) -> f32 {
    glam::Mat3::from_mat4(matrix).determinant().abs().cbrt()
}

/// A matrix's rotation, normalised; `None` if it is not finite.
fn rotation_of(matrix: Mat4) -> Option<Quat> {
    let rotation = matrix.to_scale_rotation_translation().1.normalize();
    rotation.is_finite().then_some(rotation)
}

/// The bone's node's transform relative to its parent once `turn` takes
/// its animated rotation in the asset's space to the solved one (Godot's
/// `get_local_pose_rotation`): only its rotation changes.
fn solved(bone: &Bone, turn: Quat, frame: Frame) -> Transform {
    let rotation = (frame.parent_rotation.inverse() * (turn * frame.rotation)).normalize();
    Transform {
        rotation,
        ..bone.animated
    }
}

/// Godot's `limit_length`: `destination` moved along the line from
/// `origin` to `length` from it.
fn limit_length(origin: Vec3, destination: Vec3, length: f32) -> Vec3 {
    origin + (destination - origin).normalize_or_zero() * length
}

/// Godot's `get_from_to_rotation`: the rotation from the unit vector `from`
/// to the unit vector `to`, or `previous` when they are parallel or
/// opposite, which keeps a flip from glitching.
fn from_to_rotation(from: Vec3, to: Vec3, previous: Quat) -> Quat {
    if is_equal_approx(from.dot(to), -1.) {
        return previous;
    }
    let axis = from.cross(to);
    if is_zero_approx(axis) {
        return previous;
    }
    // Godot's `Vector3::angle_to`.
    let mut angle = axis.length().atan2(from.dot(to));
    if angle.abs() < CMP_EPSILON {
        angle = 0.;
    }
    Quat::from_axis_angle(axis.normalize(), angle)
}

/// Godot's `Vector3::is_zero_approx`.
fn is_zero_approx(vector: Vec3) -> bool {
    vector.abs().max_element() < CMP_EPSILON
}

/// Godot's `Math::is_equal_approx` for `f32`.
fn is_equal_approx(a: f32, b: f32) -> bool {
    if a == b {
        return true;
    }
    let tolerance = (CMP_EPSILON * a.abs()).max(CMP_EPSILON);
    (a - b).abs() < tolerance
}

#[cfg(test)]
#[path = "spring_bones_tests.rs"]
mod tests;
