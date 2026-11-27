//! Spring-bone chains judged by the rig's geometry and by closed forms
//! worked by hand from the Verlet update, never by the solver's own output.
use glam::{Mat4, Quat, Vec3, Vec4};
use wasm_bindgen_test::wasm_bindgen_test;

use super::{Chain, ChainField, Collider, ColliderShape, Sheet, SpringBones, SpringBonesError};
use crate::animation::Pose;
use crate::deformation::{Joint, JointMatrices, MorphWeight, Node, Rig, Transform};

/// A frame at 60 Hz.
const FRAME: f32 = 1. / 60.;

fn node(parent: Option<usize>, translation: Vec3, rotation: Quat) -> Node {
    Node {
        name: None,
        parent,
        rest: Transform {
            translation,
            rotation,
            scale: Vec3::ONE,
        },
    }
}

/// A body at the asset's origin (node 0) and a chain from `(0, 1, 0)`
/// (node 1) through one node per entry of `bones`, each its translation
/// from the node before it, at rest unrotated. Every node is a joint with
/// an identity inverse bind matrix, so the joint matrices are the nodes'
/// transforms in the asset's space.
fn rig(bones: &[Vec3]) -> Rig {
    let mut nodes = vec![
        node(None, Vec3::ZERO, Quat::IDENTITY),
        node(Some(0), Vec3::Y, Quat::IDENTITY),
    ];
    for (index, &bone) in bones.iter().enumerate() {
        nodes.push(node(Some(index + 1), bone, Quat::IDENTITY));
    }
    with_joints(nodes)
}

fn with_joints(nodes: Vec<Node>) -> Rig {
    Rig {
        joints: (0..nodes.len())
            .map(|node| Joint {
                node,
                inverse_bind: Mat4::IDENTITY,
            })
            .collect(),
        nodes,
        ..Default::default()
    }
}

/// The chain over every node from 1 on.
fn chain(rig: &Rig) -> Chain {
    Chain {
        nodes: (1..rig.nodes.len()).collect(),
        ..Chain::default()
    }
}

/// Each node's position in the world under `world`.
fn positions(rig: &Rig, pose: &Pose, world: Mat4) -> Vec<Vec3> {
    let mut locals = Vec::new();
    let mut joints = JointMatrices::new();
    pose.local_matrices(&mut locals);
    rig.joint_matrices(&locals, &mut joints).unwrap();
    joints
        .matrices()
        .iter()
        .map(|matrix| world.transform_point3(matrix.w_axis.truncate()))
        .collect()
}

/// Steps `springs` for `frames` calls of `dt`, each with a fresh rest pose
/// and the world `motion` gives at the call's time, and hands each call's
/// solved pose and world to `check`.
fn run(
    springs: &mut SpringBones,
    rig: &Rig,
    frames: usize,
    dt: f32,
    force: Vec3,
    motion: impl Fn(f32) -> Mat4,
    mut check: impl FnMut(usize, &Pose, Mat4),
) {
    for frame in 1..=frames {
        let world = motion(frame as f32 * dt);
        let mut pose = Pose::rest(rig);
        springs.step(rig, &mut pose, world, force, dt);
        check(frame, &pose, world);
    }
}

#[track_caller]
fn assert_direction(actual: Vec3, expected: Vec3, tolerance: f32) {
    let angle = actual.normalize().angle_between(expected.normalize());
    assert!(
        angle < tolerance,
        "{actual} points {angle} rad from {expected}"
    );
}

// Plausible defects: a tail started or held in the wrong space (the asset's
// for the world's, or the node's rest frame for its posed one), the
// stiffness term turned by the world or the node twice, or a turn written
// where no force acts.
// Oracle: the rig's geometry. With no gravity or force, every pull is along
// the animated direction, so each node keeps its rest rotation.
#[wasm_bindgen_test(unsupported = test)]
fn at_rest_without_forces_the_pose_is_unchanged() {
    // A chain that curls: each node turned about Z, each child along its
    // parent's −Y, under a turned and moved instance.
    let mut nodes = vec![
        node(None, Vec3::ZERO, Quat::IDENTITY),
        node(Some(0), Vec3::Y, Quat::from_rotation_z(0.3)),
    ];
    for index in 1..5 {
        nodes.push(node(
            Some(index),
            Vec3::new(0., -0.2, 0.05),
            Quat::from_rotation_z(0.3) * Quat::from_rotation_x(-0.2),
        ));
    }
    let rig = with_joints(nodes);
    let mut springs = SpringBones::new(
        &rig,
        &[Chain {
            tip: 0.1,
            ..chain(&rig)
        }],
        &[],
        &[],
    )
    .unwrap();
    let world = Mat4::from_rotation_translation(Quat::from_rotation_y(0.8), Vec3::new(3., 0., -2.));
    run(
        &mut springs,
        &rig,
        120,
        FRAME,
        Vec3::ZERO,
        |_| world,
        |frame, pose, _| {
            for (index, (solved, rest)) in pose.nodes.iter().zip(&rig.nodes).enumerate() {
                assert!(
                    solved.rotation.dot(rest.rest.rotation).abs() > 1. - 1e-6,
                    "frame {frame}: node {index} turned to {} from {}",
                    solved.rotation,
                    rest.rest.rotation
                );
            }
        },
    );
}

// Plausible defects: the solve writing anything but the chains' rotations
// (a translation, a scale, a morph weight, a node outside the chains), or
// a non-unit rotation, which would scale the bone.
// Oracle: the contract of `step`: it turns each chain's nodes and changes
// nothing else, so everything else is bit for bit the pose it was given.
#[wasm_bindgen_test(unsupported = test)]
fn step_writes_only_the_chains_unit_rotations() {
    let mut rig = rig(&[
        Vec3::new(0.3, -0.1, 0.),
        Vec3::new(0., -0.25, 0.1),
        Vec3::new(0.1, -0.2, -0.1),
        Vec3::new(0., -0.15, 0.),
    ]);
    // An arm beside the chain, and two morph weights.
    rig.nodes.push(node(
        Some(0),
        Vec3::new(0.4, 1.2, 0.),
        Quat::from_rotation_z(0.4),
    ));
    rig.morph_weights = vec![
        MorphWeight {
            node: 0,
            rest: 0.25,
        },
        MorphWeight {
            node: 0,
            rest: 0.75,
        },
    ];
    let chain_nodes = 1..=5;
    let mut springs = SpringBones::new(
        &rig,
        &[Chain {
            nodes: chain_nodes.clone().collect(),
            tip: 0.1,
            gravity: 1.5,
            drag: 0.1,
            ..Chain::default()
        }],
        &[],
        &[],
    )
    .unwrap();
    let bits = |v: Vec3| v.to_array().map(f32::to_bits);
    for frame in 1..=180 {
        let t = frame as f32 * FRAME;
        let world = Mat4::from_rotation_translation(
            Quat::from_rotation_y(3. * t.sin()),
            Vec3::new(2. * (4. * t).sin(), (7. * t).cos(), 0.),
        );
        // A pose that is not the rest pose: every node moved, turned and
        // scaled a little, and the weights set.
        let mut given = Pose::rest(&rig);
        for (index, node) in given.nodes.iter_mut().enumerate() {
            let wobble = (t + index as f32).sin() * 0.1;
            node.translation *= 1. + wobble;
            node.rotation = (node.rotation * Quat::from_rotation_x(wobble)).normalize();
            node.scale = Vec3::splat(1. + 0.5 * wobble);
        }
        given.morph_weights = vec![t.sin(), t.cos()];
        let mut pose = given.clone();
        springs.step(&rig, &mut pose, world, Vec3::new(0.5, 0., 2.), FRAME);
        assert_eq!(
            pose.morph_weights
                .iter()
                .map(|w| w.to_bits())
                .collect::<Vec<_>>(),
            given
                .morph_weights
                .iter()
                .map(|w| w.to_bits())
                .collect::<Vec<_>>(),
            "frame {frame}: the morph weights changed"
        );
        for (index, (solved, given)) in pose.nodes.iter().zip(&given.nodes).enumerate() {
            assert_eq!(
                bits(solved.translation),
                bits(given.translation),
                "frame {frame}: node {index}'s translation"
            );
            assert_eq!(
                bits(solved.scale),
                bits(given.scale),
                "frame {frame}: node {index}'s scale"
            );
            if chain_nodes.contains(&index) {
                assert!(
                    (solved.rotation.length() - 1.).abs() < 1e-6,
                    "frame {frame}: node {index}'s rotation {} is not a unit quaternion",
                    solved.rotation
                );
            } else {
                assert_eq!(
                    Vec4::from(solved.rotation).to_array().map(f32::to_bits),
                    Vec4::from(given.rotation).to_array().map(f32::to_bits),
                    "frame {frame}: node {index} outside the chain turned"
                );
            }
        }
    }
}

// Plausible defects: stiffness or gravity scaled by the wrong step, gravity
// taken in the model's space instead of the world's, stiffness pulling
// towards a direction other than the animated one, or a bone that never
// settles.
// Oracle: at rest the Verlet step adds `step · (s·rest + g·down)` to the
// tail and the length limit pulls it back onto the bone's sphere, so the
// tail stays put only where that sum lies along the bone: the bone settles
// along normalize(s·rest + g·down), straight down when s = 0, whatever the
// step. The instance is turned about Y, which turns `rest` and not `down`.
#[wasm_bindgen_test(unsupported = test)]
fn one_bone_settles_between_its_rest_direction_and_down() {
    let rig = rig(&[Vec3::X * 0.5]);
    let turn = Quat::from_rotation_y(0.7);
    let world = Mat4::from_rotation_translation(turn, Vec3::new(1., 2., 3.));
    for (stiffness, gravity) in [(1., 1.), (2., 0.5), (0.5, 3.), (0., 1.)] {
        let mut springs = SpringBones::new(
            &rig,
            &[Chain {
                stiffness,
                gravity,
                ..chain(&rig)
            }],
            &[],
            &[],
        )
        .unwrap();
        let mut last = Vec3::ZERO;
        run(
            &mut springs,
            &rig,
            600,
            FRAME,
            Vec3::ZERO,
            |_| world,
            |_, pose, world| {
                let at = positions(&rig, pose, world);
                last = at[2] - at[1];
            },
        );
        let expected = stiffness * (turn * Vec3::X) + gravity * Vec3::NEG_Y;
        assert_direction(last, expected, 1e-3);
    }
}

// Plausible defects: the instance's motion ignored (the chain simulated in
// the asset's space), the tail dragged along with the body, or stiffness
// that does not bring it back.
// Oracle: the rig's geometry. At rest the tip hangs straight below the
// chain's root; while the body moves along +X the tip's inertia leaves it
// behind, on the −X side, and once the body stops, stiffness and drag
// bring it back below the root.
#[wasm_bindgen_test(unsupported = test)]
fn moving_sideways_makes_the_tip_lag_and_return() {
    let rig = rig(&[Vec3::NEG_Y * 0.2; 3]);
    let mut springs = SpringBones::new(&rig, &[chain(&rig)], &[], &[]).unwrap();
    // 2 m/s along +X for half a second, then still.
    let motion = |t: f32| Mat4::from_translation(Vec3::X * 2. * t.min(0.5));
    run(
        &mut springs,
        &rig,
        300,
        FRAME,
        Vec3::ZERO,
        motion,
        |frame, pose, world| {
            let at = positions(&rig, pose, world);
            let behind = at[4].x - at[1].x;
            if (10..=30).contains(&frame) {
                assert!(
                    behind < -0.01,
                    "frame {frame}: the tip is {behind} m along the motion"
                );
            }
            if frame == 300 {
                assert!(
                    at[4].abs_diff_eq(at[1] + Vec3::NEG_Y * 0.6, 1e-3),
                    "after the stop the tip is at {}, not below the root {}",
                    at[4],
                    at[1]
                );
            }
        },
    );
}

/// A chain of three 0.1 m bones held out level along +X, which gravity
/// drops onto `collider` on the body and leaves draped over it for two
/// seconds; then the body swings along X at 1 m/s for two more, carrying
/// the collider and the chain's root. The instance scales everything by
/// `scale`. It runs at 30, 60, 90 and 144 calls a second, so the pose
/// written between two steps is tested as well as the steps'. Every call,
/// each tail of the written pose, composed through the rig in the world
/// given to that call, lies at a distance from the shape's centre or
/// segment (`distance`, in the body's space, so in the world `scale` times
/// it) of at least the two radii less 0.1 mm, f32 rounding at this scale.
/// The chain's root lies outside the grown collider and within a bone's
/// length of its surface; each tail moves under 2 cm a step, a tenth of
/// the collider's width.
fn drape_over(collider: ColliderShape, reach: f32, scale: f32, distance: impl Fn(Vec3) -> f32) {
    let rig = rig(&[Vec3::X * 0.1; 3]);
    let chain = Chain {
        stiffness: 0.,
        gravity: 1.,
        radius: 0.02,
        ..chain(&rig)
    };
    let root = distance(Vec3::Y) - reach - chain.radius;
    assert!(
        root > 0. && root < 0.1,
        "the root is {root} from the surface"
    );
    let motion = |t: f32| {
        // Still for two seconds, then 0.25 m either way and back at 1 m/s.
        let phase = (t - 2.).max(0.) % 1.;
        let x = if phase < 0.25 {
            phase
        } else if phase < 0.75 {
            0.5 - phase
        } else {
            phase - 1.
        };
        Mat4::from_translation(Vec3::new(x, 0., 4.)) * Mat4::from_scale(Vec3::splat(scale))
    };
    for rate in [30, 60, 90, 144] {
        let mut springs = SpringBones::new(
            &rig,
            std::slice::from_ref(&chain),
            &[],
            &[Collider {
                node: 0,
                shape: collider,
            }],
        )
        .unwrap();
        let mut closest = f32::INFINITY;
        run(
            &mut springs,
            &rig,
            4 * rate,
            1. / rate as f32,
            Vec3::ZERO,
            motion,
            |frame, pose, world| {
                let at = positions(&rig, pose, world);
                for (index, tail) in at[2..].iter().enumerate() {
                    let away = distance(world.inverse().transform_point3(*tail));
                    closest = closest.min(away);
                    assert!(
                        away >= reach + chain.radius - 1e-4,
                        "{rate} Hz, call {frame}: tail {index} is {away} from the collider, inside {}",
                        reach + chain.radius
                    );
                }
            },
        );
        // Unblocked, the chain would fall straight down through the
        // collider (the s = 0 case above); it must have come to lie
        // against it.
        assert!(
            closest < reach + chain.radius + 0.01,
            "{rate} Hz: no tail came within 1 cm of the collider: {closest}"
        );
    }
}

// Plausible defects: the collider not moved with its node or the instance,
// the push taken from the wrong centre, the bone's radius left out, or the
// length restore left to undo the push.
// Oracle: the sphere's geometry, its surface grown by the chain's radius.
// The sphere sits under the arc the falling chain sweeps.
#[wasm_bindgen_test(unsupported = test)]
fn tails_stay_out_of_a_sphere() {
    let center = Vec3::new(0.15, 0.85, 0.);
    drape_over(
        ColliderShape::Sphere {
            center,
            radius: 0.1,
        },
        0.1,
        1.,
        |point| point.distance(center),
    );
}

// Plausible defects: as for the sphere, and the nearest point of the
// capsule's segment taken wrongly (beyond its ends, or not moved as the
// tail moves along it).
// Oracle: the capsule's geometry: its segment, slanted across the chain's
// path so that its nearest point moves as a tail slides over it, grown by
// both radii.
#[wasm_bindgen_test(unsupported = test)]
fn tails_stay_out_of_a_capsule() {
    let (head, tail) = (Vec3::new(0., 0.88, -0.3), Vec3::new(0.3, 0.88, 0.3));
    drape_over(
        ColliderShape::Capsule {
            head,
            tail,
            radius: 0.08,
        },
        0.08,
        1.,
        |point| {
            let along = (point - head).dot(tail - head) / (tail - head).length_squared();
            point.distance(head + (tail - head) * along.clamp(0., 1.))
        },
    );
}

/// Steps a chain hanging 0.45 m below a body that moves 1 m/s along X for
/// half a second and then stands still, at `rate` calls a second, and
/// returns the tip's offset from the chain's root at 0.5 s and at 1 s.
fn tip_at_rate(rate: u32) -> [Vec3; 2] {
    let rig = rig(&[Vec3::NEG_Y * 0.15; 3]);
    let mut springs = SpringBones::new(
        &rig,
        &[Chain {
            drag: 0.1,
            ..chain(&rig)
        }],
        &[],
        &[],
    )
    .unwrap();
    let dt = 1. / rate as f32;
    let motion = |t: f32| Mat4::from_translation(Vec3::X * t.min(0.5));
    // The first call starts the chain at rest where it is given, so it is
    // made at time zero at every rate, stepping nothing.
    springs.step(&rig, &mut Pose::rest(&rig), motion(0.), Vec3::ZERO, 0.);
    let mut tips = [Vec3::ZERO; 2];
    run(
        &mut springs,
        &rig,
        rate as usize,
        dt,
        Vec3::ZERO,
        motion,
        |frame, pose, world| {
            let at = positions(&rig, pose, world);
            if frame as u32 * 2 == rate {
                tips[0] = at[4] - at[1];
            }
            if frame as u32 == rate {
                tips[1] = at[4] - at[1];
            }
        },
    );
    tips
}

// Plausible defects: steps of the call's length instead of the fixed one,
// the remainder dropped or counted twice, each step given the call's world
// rather than the world at its own time, or the output not taken between
// the last two steps.
// Oracle: an equivalence. The motion is linear between calls at every rate
// (the stop falls on a call at each), so interpolating `world` to each
// fixed step's time gives the same steps at 30, 60 and 144 calls a second;
// what is left is rounding of the carried time at the tip's speed, under
// 1 mm.
#[wasm_bindgen_test(unsupported = test)]
fn the_frame_rate_does_not_change_the_motion() {
    let at_60 = tip_at_rate(60);
    for rate in [30, 144] {
        for (time, (tip, reference)) in tip_at_rate(rate).iter().zip(&at_60).enumerate() {
            assert!(
                tip.distance(*reference) < 1e-3,
                "at {} s and {rate} calls a second the tip is at {tip}, at 60 {reference}",
                (time + 1) as f32 * 0.5
            );
        }
    }
}

// Plausible defects: `reset` that keeps the old tails, or the world of the
// call before the reset interpolated into the steps after it, either of
// which whips the chain across the kilometre.
// Oracle: the rig's geometry. Without gravity or force the settled pose is
// the rest pose, and a reset starts at rest in the pose it is given.
#[wasm_bindgen_test(unsupported = test)]
fn reset_after_a_teleport_keeps_the_settled_pose() {
    let rig = rig(&[Vec3::new(0.1, -0.15, 0.05); 3]);
    let mut springs = SpringBones::new(&rig, &[chain(&rig)], &[], &[]).unwrap();
    // A swing that settles: 0.3 m along Z, then still for 3 s.
    let here = |t: f32| Mat4::from_translation(Vec3::Z * 0.6 * t.min(0.5));
    run(
        &mut springs,
        &rig,
        210,
        FRAME,
        Vec3::ZERO,
        here,
        |_, _, _| {},
    );
    springs.reset();
    let there = Mat4::from_translation(Vec3::new(1000., 0., 0.3));
    run(
        &mut springs,
        &rig,
        60,
        FRAME,
        Vec3::ZERO,
        |_| there,
        |frame, pose, _| {
            for (index, node) in pose.nodes.iter().enumerate() {
                let angle = node.rotation.angle_between(Quat::IDENTITY);
                assert!(
                    angle < 1e-3,
                    "frame {frame}: node {index} turned {angle} rad"
                );
            }
        },
    );
}

// Plausible defects: a NaN or infinite input stored and spread to every
// later pose, a step run for a NaN or negative time, or a zero-length
// bone's direction normalised to NaN.
// Oracle: a bound: every rotation written is a finite unit quaternion, and
// a call refused for a non-finite world or force leaves the pose given.
#[wasm_bindgen_test(unsupported = test)]
fn non_finite_inputs_and_a_zero_length_bone_leave_a_finite_pose() {
    let rig = rig(&[Vec3::X * 0.2, Vec3::ZERO, Vec3::NEG_Y * 0.2]);
    let mut springs = SpringBones::new(
        &rig,
        &[Chain {
            gravity: 1.,
            tip: 0.1,
            ..chain(&rig)
        }],
        &[],
        &[Collider {
            node: 0,
            shape: ColliderShape::Sphere {
                center: Vec3::new(0.2, 0.8, 0.),
                radius: 0.05,
            },
        }],
    )
    .unwrap();
    let moved = Mat4::from_translation(Vec3::X);
    let calls = [
        (Mat4::IDENTITY, Vec3::ZERO, FRAME),
        (moved, Vec3::ZERO, f32::NAN),
        (moved, Vec3::ZERO, f32::INFINITY),
        (moved, Vec3::ZERO, -FRAME),
        (moved, Vec3::ZERO, 0.),
        (moved, Vec3::ZERO, FRAME),
        (
            Mat4::from_translation(Vec3::splat(f32::NAN)),
            Vec3::ZERO,
            FRAME,
        ),
        (
            Mat4::from_scale(Vec3::splat(f32::INFINITY)),
            Vec3::ZERO,
            FRAME,
        ),
        (Mat4::ZERO, Vec3::ZERO, FRAME),
        (moved, Vec3::splat(f32::NAN), FRAME),
        (moved, Vec3::new(f32::INFINITY, 0., 0.), FRAME),
        (moved, Vec3::splat(f32::MAX), FRAME),
        (Mat4::IDENTITY, Vec3::ZERO, 1e30),
        (moved, Vec3::ZERO, FRAME),
        (Mat4::IDENTITY, Vec3::ZERO, FRAME),
    ];
    for (call, &(world, force, dt)) in calls.iter().enumerate() {
        let given = Pose::rest(&rig);
        let mut pose = given.clone();
        springs.step(&rig, &mut pose, world, force, dt);
        for (index, node) in pose.nodes.iter().enumerate() {
            assert!(
                node.rotation.is_finite() && (node.rotation.length() - 1.).abs() < 1e-5,
                "call {call}: node {index}'s rotation is {}",
                node.rotation
            );
        }
        if !world.is_finite() || !force.is_finite() {
            assert_eq!(pose, given, "call {call} changed the pose");
        }
    }
}

// Plausible defect: a refusal missed, or reported for the wrong chain,
// collider, node or field.
// Oracle: each case breaks exactly one documented requirement of `new`.
#[wasm_bindgen_test(unsupported = test)]
fn each_refusal_names_its_cause() {
    let rig = rig(&[Vec3::X * 0.2; 2]);
    let good = chain(&rig);
    let sphere = |node, center: Vec3, radius| Collider {
        node,
        shape: ColliderShape::Sphere { center, radius },
    };
    let refused = |chains: &[Chain], colliders: &[Collider]| {
        SpringBones::new(&rig, chains, &[], colliders).err()
    };
    assert_eq!(
        refused(
            &[
                good.clone(),
                Chain {
                    nodes: vec![9],
                    ..good.clone()
                }
            ],
            &[]
        ),
        Some(SpringBonesError::ChainNode { chain: 1, node: 9 })
    );
    assert_eq!(
        refused(
            &[Chain {
                nodes: vec![1, 3],
                ..good.clone()
            }],
            &[]
        ),
        Some(SpringBonesError::NotChild { chain: 0, node: 3 })
    );
    // Two chains from one fork's node: [1, 2] and [1, 3] where 2 and 3
    // are both children of 1.
    let mut fork = rig.clone();
    fork.nodes[3].parent = Some(1);
    assert_eq!(
        SpringBones::new(
            &fork,
            &[
                Chain {
                    nodes: vec![1, 2],
                    ..Chain::default()
                },
                Chain {
                    nodes: vec![1, 3],
                    ..Chain::default()
                }
            ],
            &[],
            &[]
        )
        .err(),
        Some(SpringBonesError::SharedNode {
            first: 0,
            second: 1,
            node: 1
        })
    );
    for (chain, field) in [
        (
            Chain {
                tip: f32::NAN,
                ..good.clone()
            },
            ChainField::Tip,
        ),
        (
            Chain {
                stiffness: -1.,
                ..good.clone()
            },
            ChainField::Stiffness,
        ),
        (
            Chain {
                drag: 1.5,
                ..good.clone()
            },
            ChainField::Drag,
        ),
        (
            Chain {
                drag: -0.1,
                ..good.clone()
            },
            ChainField::Drag,
        ),
        (
            Chain {
                gravity: f32::INFINITY,
                ..good.clone()
            },
            ChainField::Gravity,
        ),
        (
            Chain {
                radius: -0.01,
                ..good.clone()
            },
            ChainField::Radius,
        ),
    ] {
        assert_eq!(
            refused(&[chain], &[]),
            Some(SpringBonesError::ChainValue { chain: 0, field })
        );
    }
    assert_eq!(
        refused(
            std::slice::from_ref(&good),
            &[sphere(0, Vec3::ZERO, 0.1), sphere(7, Vec3::ZERO, 0.1)]
        ),
        Some(SpringBonesError::ColliderNode {
            collider: 1,
            node: 7
        })
    );
    assert_eq!(
        refused(
            std::slice::from_ref(&good),
            &[sphere(0, Vec3::splat(f32::NAN), 0.1)]
        ),
        Some(SpringBonesError::ColliderShape { collider: 0 })
    );
    assert_eq!(
        refused(
            std::slice::from_ref(&good),
            &[Collider {
                node: 0,
                shape: ColliderShape::Capsule {
                    head: Vec3::ZERO,
                    tail: Vec3::Y,
                    radius: -0.1,
                },
            }]
        ),
        Some(SpringBonesError::ColliderShape { collider: 0 })
    );
    assert!(refused(&[good], &[sphere(0, Vec3::ZERO, 0.)]).is_none());
}

// Plausible defects: a chain that stops before a lone child, runs past a
// fork, or does not stop at a leaf.
// Oracle: the rig's hierarchy: 0 → 1 → 2, which forks into 3 and 4.
#[wasm_bindgen_test(unsupported = test)]
fn a_chain_from_a_root_runs_to_a_fork_or_a_leaf() {
    let rig = with_joints(vec![
        node(None, Vec3::ZERO, Quat::IDENTITY),
        node(Some(0), Vec3::Y, Quat::IDENTITY),
        node(Some(1), Vec3::Y, Quat::IDENTITY),
        node(Some(2), Vec3::X, Quat::IDENTITY),
        node(Some(2), Vec3::NEG_X, Quat::IDENTITY),
    ]);
    assert_eq!(Chain::from_root(&rig, 0).nodes, [0, 1, 2]);
    assert_eq!(Chain::from_root(&rig, 3).nodes, [3]);
}

/// `rig(bones)` authored `factor` times larger, in centimetres for 100,
/// under a body node scaled by `1 / factor`: the same world geometry.
fn rig_in_units(bones: &[Vec3], factor: f32) -> Rig {
    let mut rig = rig(&bones.iter().map(|bone| *bone * factor).collect::<Vec<_>>());
    rig.nodes[0].rest.scale = Vec3::splat(1. / factor);
    rig.nodes[1].rest.translation *= factor;
    rig
}

/// The world positions of the chain's nodes, frame by frame, for a chain
/// with a tip and a sphere on the body, authored `factor` times larger
/// under a `1 / factor` body, falling, draping and swung by the instance.
fn run_in_units(factor: f32) -> Vec<Vec<Vec3>> {
    let rig = rig_in_units(&[Vec3::X * 0.1; 3], factor);
    let mut springs = SpringBones::new(
        &rig,
        &[Chain {
            tip: 0.05 * factor,
            stiffness: 0.3,
            gravity: 1.,
            radius: 0.02 * factor,
            ..chain(&rig)
        }],
        &[],
        &[Collider {
            node: 0,
            shape: ColliderShape::Sphere {
                center: Vec3::new(0.15, 0.85, 0.) * factor,
                radius: 0.1 * factor,
            },
        }],
    )
    .unwrap();
    let motion = |t: f32| {
        let swing = (t - 1.).max(0.);
        Mat4::from_rotation_translation(
            Quat::from_rotation_y(0.5 * swing),
            Vec3::new(0.5 * swing.min(1.), 0., 0.),
        )
    };
    let mut frames = Vec::new();
    run(
        &mut springs,
        &rig,
        150,
        FRAME,
        Vec3::ZERO,
        motion,
        |_, pose, world| {
            frames.push(positions(&rig, pose, world)[1..].to_vec());
        },
    );
    frames
}

// Plausible defects: a bone's length, the tip, the chain's radius or a
// collider's radius taken in the world unscaled, so a rig authored in
// centimetres under a 0.01 root simulates bones a hundred times too long
// and colliders a hundred times too large or small.
// Oracle: scale invariance. Both rigs put every node, the tip and the
// sphere at the same world places, so every step's world positions must
// agree; the bound is f32 rounding (0.01 is inexact in f32, and a
// hundredfold length composed through it differs in the last bits), which
// the run's damped motion keeps near its size, far under 10 µm.
#[wasm_bindgen_test(unsupported = test)]
fn a_rig_in_centimetres_under_a_scaled_root_moves_as_one_in_metres() {
    let metres = run_in_units(1.);
    let centimetres = run_in_units(100.);
    for (frame, (a, b)) in metres.iter().zip(&centimetres).enumerate() {
        for (index, (a, b)) in a.iter().zip(b).enumerate() {
            assert!(
                a.distance(*b) < 1e-5,
                "frame {frame}: node {} at {a} in metres, {b} in centimetres",
                index + 1
            );
        }
    }
}

// Plausible defects: the instance's scale left out of a bone's length, the
// tip or a radius, so a doubled character keeps hair and colliders of the
// old size, or the solved world direction turned back into the asset's
// space without undoing the scale.
// Oracle: scale invariance and the closed form above. Doubling the
// instance doubles every offset from it: a bone settles along
// normalize(s·rest + g·down), twice its length from its root, and tails
// stay outside colliders twice the size, by twice the chain's radius.
#[wasm_bindgen_test(unsupported = test)]
fn a_doubled_instance_doubles_lengths_and_colliders() {
    let rig = rig(&[Vec3::X * 0.5]);
    let turn = Quat::from_rotation_y(0.7);
    let world = Mat4::from_scale_rotation_translation(Vec3::splat(2.), turn, Vec3::new(1., 2., 3.));
    let mut springs = SpringBones::new(
        &rig,
        &[Chain {
            stiffness: 1.,
            gravity: 0.5,
            ..chain(&rig)
        }],
        &[],
        &[],
    )
    .unwrap();
    let mut last = Vec3::ZERO;
    run(
        &mut springs,
        &rig,
        600,
        FRAME,
        Vec3::ZERO,
        |_| world,
        |_, pose, world| {
            let at = positions(&rig, pose, world);
            last = at[2] - at[1];
        },
    );
    let expected = (turn * Vec3::X + 0.5 * Vec3::NEG_Y).normalize() * 2. * 0.5;
    assert!(
        last.abs_diff_eq(expected, 1e-3),
        "the tail settles {last} from its root, not {expected}"
    );

    let center = Vec3::new(0.15, 0.85, 0.);
    drape_over(
        ColliderShape::Sphere {
            center,
            radius: 0.1,
        },
        0.1,
        2.,
        |point| point.distance(center),
    );
    let (head, tail) = (Vec3::new(0., 0.88, -0.3), Vec3::new(0.3, 0.88, 0.3));
    drape_over(
        ColliderShape::Capsule {
            head,
            tail,
            radius: 0.08,
        },
        0.08,
        2.,
        |point| {
            let along = (point - head).dot(tail - head) / (tail - head).length_squared();
            point.distance(head + (tail - head) * along.clamp(0., 1.))
        },
    );
}

// Plausible defect: chains solved in the order given, so a chain hanging
// from another chain's node and listed first reads that node before it is
// solved and trails it by a step.
// Oracle: an equivalence. The two orders of one parent chain and one child
// chain name the same chains, so they must give the same pose.
#[wasm_bindgen_test(unsupported = test)]
fn the_order_of_chains_does_not_change_the_pose() {
    // A parent chain [1, 2] with a tip, and a child chain [3, 4, 5] hanging
    // from node 2.
    let rig = rig(&[Vec3::new(0.15, -0.1, 0.); 4]);
    let parent = Chain {
        nodes: vec![1, 2],
        tip: 0.1,
        gravity: 1.,
        ..Chain::default()
    };
    let child = Chain {
        nodes: vec![3, 4, 5],
        gravity: 1.,
        ..Chain::default()
    };
    let motion = |t: f32| {
        Mat4::from_rotation_translation(
            Quat::from_rotation_y(2. * t),
            Vec3::new((3. * t).sin(), 0., 0.),
        )
    };
    let poses = |chains: &[Chain]| {
        let mut springs = SpringBones::new(&rig, chains, &[], &[]).unwrap();
        let mut poses = Vec::new();
        run(
            &mut springs,
            &rig,
            120,
            FRAME,
            Vec3::ZERO,
            motion,
            |_, pose, _| {
                poses.push(pose.clone());
            },
        );
        poses
    };
    let given = poses(&[parent.clone(), child.clone()]);
    let reversed = poses(&[child, parent]);
    for (frame, (a, b)) in given.iter().zip(&reversed).enumerate() {
        assert_eq!(a, b, "frame {frame}: the two orders differ");
    }
}

// Plausible defect: a hitch's capped steps placing the first step at the
// call's time less seven steps, so it sees most of the frame's movement at
// once and whips the hair ahead of the character.
// Oracle: the motion's direction. Running steadily along +X, a hanging tip
// trails its root; a 0.5 s frame of the same running moves the body the
// same way, so after it the tip is still behind the root along +X.
#[wasm_bindgen_test(unsupported = test)]
fn a_hitch_does_not_whip_the_chain_ahead() {
    let rig = rig(&[Vec3::NEG_Y * 0.15; 3]);
    let mut springs = SpringBones::new(&rig, &[chain(&rig)], &[], &[]).unwrap();
    // 3 m/s along +X.
    let motion = |t: f32| Mat4::from_translation(Vec3::X * 3. * t);
    let mut behind = 0.;
    run(
        &mut springs,
        &rig,
        120,
        FRAME,
        Vec3::ZERO,
        motion,
        |_, pose, world| {
            let at = positions(&rig, pose, world);
            behind = at[4].x - at[1].x;
        },
    );
    assert!(
        behind < 0.,
        "running, the tip is {behind} m along the motion"
    );
    let world = motion(2.5);
    let mut pose = Pose::rest(&rig);
    springs.step(&rig, &mut pose, world, Vec3::ZERO, 0.5);
    let at = positions(&rig, &pose, world);
    let behind = at[4].x - at[1].x;
    assert!(
        behind < 0.,
        "after the hitch the tip is {behind} m along the motion"
    );
}

/// The length of every bone of `hanging`'s chains, their tips included.
const BONE: f32 = 0.08;
/// `hanging`'s leg: a node on the body at its origin, which a test moves
/// through the pose to carry a sphere.
const LEG: usize = 1;
/// The chains' radius, `Chain`'s default.
const RADIUS: f32 = 0.02;

/// A body (node 0) with a leg (node 1) and a hip (node 2) at its origin,
/// and a chain from each of `roots` of `nodes` nodes, each `bone` from the
/// one before, and a tip `BONE` on along it, so as many bones as nodes.
/// Odd chains hang from the hip, so they are solved after the even ones
/// whatever order they are given in. No node is rotated.
fn hanging(roots: &[Vec3], nodes: &[usize], bone: Vec3) -> (Rig, Vec<Chain>) {
    let mut rig = vec![
        node(None, Vec3::ZERO, Quat::IDENTITY),
        node(Some(0), Vec3::ZERO, Quat::IDENTITY),
        node(Some(0), Vec3::ZERO, Quat::IDENTITY),
    ];
    let mut chains = Vec::new();
    for (index, (&root, &count)) in roots.iter().zip(nodes).enumerate() {
        let first = rig.len();
        let parent = if index % 2 == 0 { 0 } else { 2 };
        rig.push(node(Some(parent), root, Quat::IDENTITY));
        for _ in 1..count {
            rig.push(node(Some(rig.len() - 1), bone, Quat::IDENTITY));
        }
        chains.push(Chain {
            nodes: (first..first + count).collect(),
            tip: BONE,
            gravity: 0.5,
            ..Chain::default()
        });
    }
    (with_joints(rig), chains)
}

/// `count` roots evenly round a circle of `radius` about the body's Y axis
/// at 1 m, the first on +X, turning towards +Z.
fn ring(count: usize, radius: f32) -> Vec<Vec3> {
    (0..count)
        .map(|index| {
            let angle = index as f32 / count as f32 * std::f32::consts::TAU;
            Vec3::new(angle.cos() * radius, 1., angle.sin() * radius)
        })
        .collect()
}

/// A sphere of `radius` on the leg.
fn leg(radius: f32) -> Collider {
    Collider {
        node: LEG,
        shape: ColliderShape::Sphere {
            center: Vec3::ZERO,
            radius,
        },
    }
}

/// Each chain's tails in the world as `pose` draws them: its nodes after
/// the first, then its tip, `tip` on from the last node the way it lies
/// from its parent at rest.
fn drawn_tails(rig: &Rig, pose: &Pose, world: Mat4, chains: &[Chain]) -> Vec<Vec<Vec3>> {
    let mut locals = Vec::new();
    let mut joints = JointMatrices::new();
    pose.local_matrices(&mut locals);
    rig.joint_matrices(&locals, &mut joints).unwrap();
    let globals = joints.matrices();
    chains
        .iter()
        .map(|chain| {
            let last = *chain.nodes.last().unwrap();
            let tip = rig.nodes[last].rest.translation.normalize() * chain.tip;
            chain.nodes[1..]
                .iter()
                .map(|&node| globals[node].w_axis.truncate())
                .chain([globals[last].transform_point3(tip)])
                .map(|point| world.transform_point3(point))
                .collect()
        })
        .collect()
}

/// Steps `springs` at `rate` calls a second for `seconds`, each call with
/// the rest pose but the leg at `leg(t)` in the body's space and the
/// instance at `world(t)`, and hands each call's drawn tails, world and
/// leg, and the solver, to `check`.
#[allow(clippy::too_many_arguments)]
fn kick(
    springs: &mut SpringBones,
    rig: &Rig,
    chains: &[Chain],
    rate: u32,
    seconds: f32,
    leg: impl Fn(f32) -> Vec3,
    world: impl Fn(f32) -> Mat4,
    mut check: impl FnMut(usize, &[Vec<Vec3>], Mat4, Vec3, &SpringBones),
) {
    let dt = 1. / rate as f32;
    for frame in 1..=(seconds * rate as f32).round() as usize {
        let t = frame as f32 * dt;
        let (world, leg) = (world(t), leg(t));
        let mut pose = Pose::rest(rig);
        pose.nodes[LEG].translation = leg;
        springs.step(rig, &mut pose, world, Vec3::ZERO, dt);
        let tails = drawn_tails(rig, &pose, world, chains);
        check(frame, &tails, world, leg, springs);
    }
}

/// How far the distance between chain `a`'s and chain `b`'s tails at each
/// depth both reach strays from its length in `rest`, the most, as a
/// fraction of that length.
fn stray(rest: &[Vec<Vec3>], tails: &[Vec<Vec3>], [a, b]: [usize; 2]) -> f32 {
    rest[a]
        .iter()
        .zip(&rest[b])
        .zip(tails[a].iter().zip(&tails[b]))
        .map(|((rest_a, rest_b), (a, b))| {
            let rest = rest_a.distance(*rest_b);
            (a.distance(*b) - rest).abs() / rest
        })
        .fold(0., f32::max)
}

/// The leg's path: from `from` to `to` in the body's space over a second,
/// then still.
fn stride(from: Vec3, to: Vec3) -> impl Fn(f32) -> Vec3 {
    move |t| from.lerp(to, t.min(1.))
}

/// A turned and moved instance, still for 1.5 s, then swung 0.25 m either
/// way along the body's X and back at 1 m/s.
fn swing(t: f32) -> Mat4 {
    let phase = (t - 1.5).max(0.) % 1.;
    let x = if phase < 0.25 {
        phase
    } else if phase < 0.75 {
        0.5 - phase
    } else {
        phase - 1.
    };
    Mat4::from_rotation_translation(Quat::from_rotation_y(0.5), Vec3::new(1., 0., 2.))
        * Mat4::from_translation(Vec3::X * x)
}

/// Chains from `roots`, `nodes` long, held out level along +Z, which
/// gravity drops (stiffness 0, gravity 1) onto a sphere 0.07 in radius on
/// the leg at `center` (in reach of the roots, as a body part under them),
/// left to drape for 1.5 s and swung along X for 2 s more, at `rate` calls
/// a second with `sheets`; each call's drawn tails go to `check`, with
/// their rest places.
fn dropped(
    roots: &[Vec3],
    nodes: &[usize],
    sheets: &[Sheet],
    center: Vec3,
    rate: u32,
    mut check: impl FnMut(&[Vec<Vec3>], &[Vec<Vec3>]),
) {
    let (rig, chains) = hanging(roots, nodes, Vec3::Z * BONE);
    let chains: Vec<Chain> = chains
        .into_iter()
        .map(|chain| Chain {
            stiffness: 0.,
            gravity: 1.,
            ..chain
        })
        .collect();
    let mut springs = SpringBones::new(&rig, &chains, sheets, &[leg(0.07)]).unwrap();
    let rest = drawn_tails(&rig, &Pose::rest(&rig), swing(0.), &chains);
    kick(
        &mut springs,
        &rig,
        &chains,
        rate,
        3.5,
        |_| center,
        swing,
        |_, tails, _, _, _| check(&rest, tails),
    );
}

/// The most chain pair `pair`'s spacing strays as they are `dropped`.
fn dropped_stray(
    roots: &[Vec3],
    nodes: &[usize],
    sheets: &[Sheet],
    center: Vec3,
    rate: u32,
    pair: [usize; 2],
) -> f32 {
    let mut most = 0f32;
    dropped(roots, nodes, sheets, center, rate, |rest, tails| {
        most = most.max(stray(rest, tails, pair));
    });
    most
}

/// Under `dropped`'s chains: a sphere between two chains 0.1 m apart along
/// X, below their third tails, wider than their gap.
const BETWEEN: Vec3 = Vec3::new(0., 0.88, 0.16);

// Plausible defects: links that do nothing, link the wrong bones, or are
// undone by the chains' restore.
// Oracle: the rig's rest geometry. Two chains 0.1 m apart fall onto a
// sphere between them, wider than their gap, and are swung across it.
// Apart, each slides off its side of the sphere; as a sheet, their
// same-depth tails keep within 10% of their 0.1 m spacing as the pair
// drapes over it, at 30, 60, 90 and 144 calls a second.
#[wasm_bindgen_test(unsupported = test)]
fn a_sheet_keeps_its_spacing_where_a_collider_parts_separate_chains() {
    let roots = [Vec3::new(-0.05, 1., 0.), Vec3::new(0.05, 1., 0.)];
    let apart = dropped_stray(&roots, &[4; 2], &[], BETWEEN, 60, [0, 1]);
    assert!(
        apart > 0.1,
        "apart, the chains' spacing strays only {apart} of its length"
    );
    let sheet = Sheet {
        chains: vec![0, 1],
        closed: false,
    };
    for rate in [30, 60, 90, 144] {
        let linked = dropped_stray(
            &roots,
            &[4; 2],
            std::slice::from_ref(&sheet),
            BETWEEN,
            rate,
            [0, 1],
        );
        assert!(
            linked < 0.1,
            "{rate} Hz: as a sheet, the chains' spacing strays {linked} of its length"
        );
    }
}

// Plausible defect: `closed` ignored, or linking the wrong ends.
// Oracle: the rig's rest geometry. A skirt of six chains round a torso
// capsule, each 0.15 m from its neighbours, swung along X by the body.
// Open, the last and the first are the sheet's free edges and swing apart;
// closed, they are linked and keep within 10% of their spacing. The torso
// clears the links by the chains' radius at rest, as an asset's must: six
// chains 0.15 m from the axis put each link's middle 0.15 cos 30° = 0.130 m
// from it, so the torso's 0.10 m and the chains' 0.02 m leave it clear.
#[wasm_bindgen_test(unsupported = test)]
fn a_closed_sheet_links_its_last_chain_to_its_first() {
    let (rig, chains) = hanging(&ring(6, 0.15), &[5; 6], Vec3::NEG_Y * BONE);
    let torso = Collider {
        node: 0,
        shape: ColliderShape::Capsule {
            head: Vec3::Y * 0.4,
            tail: Vec3::Y * 1.1,
            radius: 0.10,
        },
    };
    let gap = |closed| {
        let sheet = Sheet {
            chains: (0..6).collect(),
            closed,
        };
        let mut springs = SpringBones::new(&rig, &chains, &[sheet], &[torso]).unwrap();
        let rest = drawn_tails(&rig, &Pose::rest(&rig), swing(0.), &chains);
        let mut most = 0f32;
        kick(
            &mut springs,
            &rig,
            &chains,
            60,
            3.5,
            |_| Vec3::ZERO,
            swing,
            |_, tails, _, _, _| most = most.max(stray(&rest, tails, [5, 0])),
        );
        most
    };
    let (open, closed) = (gap(false), gap(true));
    assert!(
        open > 0.1,
        "open, the last and first chains' spacing strays only {open} of its length"
    );
    assert!(
        closed < 0.1,
        "closed, the last and first chains' spacing strays {closed} of its length"
    );
}

// Plausible defects: links run past the shorter chain's last bone (into
// another chain's bones, or out of range), or stop short of it.
// Oracle: the rig's rest geometry, and an equivalence. A chain of three
// bones beside one of six as a sheet (given second-first, and solved
// first: the short chain is followed in the solve by a third, lone chain
// well away) fall onto a sphere under the short chain's tails. Those keep
// within 10% of their spacing, and the lone chain moves bit for bit as it
// does with no sheet at all.
#[wasm_bindgen_test(unsupported = test)]
fn chains_of_unequal_length_link_down_to_the_shorter() {
    let roots = [
        Vec3::new(0.05, 1., 0.),
        Vec3::new(-0.05, 1., 0.),
        Vec3::new(0.6, 1., 0.),
    ];
    let run = |sheets: &[Sheet]| {
        let (mut most, mut lone) = (0f32, Vec::new());
        dropped(&roots, &[3, 6, 4], sheets, BETWEEN, 60, |rest, tails| {
            most = most.max(stray(rest, tails, [1, 0]));
            lone.push(tails[2].clone());
        });
        (most, lone)
    };
    let (_, alone) = run(&[]);
    let (most, beside) = run(&[Sheet {
        chains: vec![1, 0],
        closed: false,
    }]);
    assert!(
        most < 0.1,
        "the linked depths' spacing strays {most} of its length"
    );
    assert_eq!(beside, alone, "a chain in no sheet moved with the sheet");
}

// Plausible defects: a link's length taken from the rig's rest pose rather
// than the pose given, or in the asset's units rather than the world's, so
// links pull a still sheet out of its pose.
// Oracle: the pose given. Without gravity or force, a sheet whose pose
// spreads its roots 1.3 times wider than the rig's rest and tilts every
// bone, under a turned, moved and 1.5 times scaled instance, keeps that
// pose's rotations.
#[wasm_bindgen_test(unsupported = test)]
fn at_rest_a_sheet_keeps_the_pose_given() {
    let (rig, chains) = hanging(&ring(4, 0.12), &[4; 4], Vec3::NEG_Y * BONE);
    let chains: Vec<Chain> = chains
        .into_iter()
        .map(|chain| Chain {
            gravity: 0.,
            ..chain
        })
        .collect();
    let mut springs = SpringBones::new(
        &rig,
        &chains,
        &[Sheet {
            chains: (0..4).collect(),
            closed: true,
        }],
        &[],
    )
    .unwrap();
    let mut given = Pose::rest(&rig);
    for chain in &chains {
        given.nodes[chain.nodes[0]].translation *= Vec3::new(1.3, 1., 1.3);
        for &node in &chain.nodes {
            given.nodes[node].rotation = Quat::from_rotation_x(0.3);
        }
    }
    let world = Mat4::from_scale_rotation_translation(
        Vec3::splat(1.5),
        Quat::from_rotation_y(0.8),
        Vec3::new(3., 0., -2.),
    );
    for frame in 1..=120 {
        let mut pose = given.clone();
        springs.step(&rig, &mut pose, world, Vec3::ZERO, FRAME);
        for (index, (solved, given)) in pose.nodes.iter().zip(&given.nodes).enumerate() {
            assert!(
                solved.rotation.dot(given.rotation).abs() > 1. - 1e-6,
                "frame {frame}: node {index} turned to {} from {}",
                solved.rotation,
                given.rotation
            );
        }
    }
}

// Plausible defects: tails the links moved left off their bones' lengths,
// or a chain hanging from a sheet's chain left on its parent's tail before
// the links moved it.
// Oracle: the rig's rest geometry. Through a kick of a closed skirt under
// an instance scaled by 1.5, every solved tail lies 1.5 · `BONE` from its
// bone's head, within 0.1 mm (f32 rounding at this scale, as the collider
// tests allow): the chain's root, placed by the instance, or the tail
// before it; a tassel hangs from a skirt chain's second node at its third
// node's place, so its head is the second bone's tail.
#[wasm_bindgen_test(unsupported = test)]
fn bones_keep_their_lengths_in_a_sheet() {
    let (mut rig, mut chains) = hanging(&ring(6, 0.12), &[5; 6], Vec3::NEG_Y * BONE);
    let tassel = rig.nodes.len();
    for index in 0..3 {
        let parent = if index == 0 {
            chains[0].nodes[1]
        } else {
            tassel + index - 1
        };
        rig.nodes
            .push(node(Some(parent), Vec3::NEG_Y * BONE, Quat::IDENTITY));
        rig.joints.push(Joint {
            node: tassel + index,
            inverse_bind: Mat4::IDENTITY,
        });
    }
    chains.push(Chain {
        nodes: (tassel..tassel + 3).collect(),
        ..chains[0].clone()
    });
    let mut springs = SpringBones::new(
        &rig,
        &chains,
        &[Sheet {
            chains: (0..6).collect(),
            closed: true,
        }],
        &[leg(0.07)],
    )
    .unwrap();
    let world = |_| {
        Mat4::from_scale_rotation_translation(
            Vec3::splat(1.5),
            Quat::from_rotation_y(0.5),
            Vec3::new(1., 0., 2.),
        )
    };
    let between = Quat::from_rotation_y(30f32.to_radians()) * Vec3::X;
    kick(
        &mut springs,
        &rig,
        &chains,
        60,
        1.5,
        stride(Vec3::Y * 0.75, Vec3::Y * 0.75 + between * 0.35),
        world,
        |frame, _, world, _, springs| {
            let tail = |node: usize| {
                springs
                    .bones
                    .iter()
                    .find(|bone| bone.node == node)
                    .unwrap()
                    .tail
            };
            for (index, chain) in chains.iter().enumerate() {
                let mut head = if index == 6 {
                    tail(chains[0].nodes[1])
                } else {
                    world.transform_point3(rig.nodes[chain.nodes[0]].rest.translation)
                };
                for &node in &chain.nodes {
                    let length = tail(node).distance(head);
                    assert!(
                        (length - 1.5 * BONE).abs() < 1e-4,
                        "frame {frame}: chain {index}'s bone at node {node} is {length} long"
                    );
                    head = tail(node);
                }
            }
        },
    );
}

// Plausible defects: the links' moves not taken back through the collider
// pass, or the written pose between two steps leaving it.
// Oracle: the colliders' geometry. A closed skirt round a torso capsule,
// kicked by the leg's sphere and then swung along X by the body, at 30, 60,
// 90 and 144 calls a second: every tail drawn stays at least the two radii
// less 0.1 mm from each collider's centre or segment, and the sphere,
// grown by the chains' radius, comes within 1 mm of the sheet (a tail, or
// a link between two chains' tails at one depth, which hold it off
// between them), so the scenario presses it on the sheet. At 60 calls a second each call runs one step in its own world, so
// the solver's tails, which the pose written covers for, are held to the
// same bound there.
#[wasm_bindgen_test(unsupported = test)]
fn a_sheet_stays_out_of_colliders() {
    let (rig, chains) = hanging(&ring(6, 0.12), &[5; 6], Vec3::NEG_Y * BONE);
    let sheet = Sheet {
        chains: (0..6).collect(),
        closed: true,
    };
    let (bottom, top) = (Vec3::Y * 0.5, Vec3::Y * 1.1);
    let torso = Collider {
        node: 0,
        shape: ColliderShape::Capsule {
            head: bottom,
            tail: top,
            radius: 0.06,
        },
    };
    let between = Quat::from_rotation_y(30f32.to_radians()) * Vec3::X;
    for rate in [30, 60, 90, 144] {
        let mut springs = SpringBones::new(
            &rig,
            &chains,
            std::slice::from_ref(&sheet),
            &[leg(0.07), torso],
        )
        .unwrap();
        let mut closest = f32::INFINITY;
        kick(
            &mut springs,
            &rig,
            &chains,
            rate,
            3.5,
            stride(Vec3::Y * 0.75, Vec3::Y * 0.75 + between * 0.35),
            swing,
            |frame, tails, world, leg, springs| {
                let inverse = world.inverse();
                for (index, chain) in tails.iter().enumerate() {
                    let next = &tails[(index + 1) % tails.len()];
                    for (&a, &b) in chain.iter().zip(next) {
                        let [a, b] = [a, b].map(|point| inverse.transform_point3(point));
                        let along = ((leg - a).dot(b - a) / (b - a).length_squared()).clamp(0., 1.);
                        closest = closest.min(leg.distance(a + (b - a) * along) - 0.07 - RADIUS);
                    }
                }
                let solved = springs.bones.iter().map(|bone| bone.tail);
                let solved = solved.filter(|_| rate == 60);
                for tail in tails.iter().flatten().copied().chain(solved) {
                    let local = world.inverse().transform_point3(tail);
                    let along = ((local - bottom).dot(top - bottom)
                        / (top - bottom).length_squared())
                    .clamp(0., 1.);
                    for (name, away, reach) in [
                        ("leg", local.distance(leg), 0.07),
                        (
                            "torso",
                            local.distance(bottom + (top - bottom) * along),
                            0.06,
                        ),
                    ] {
                        if name == "leg" {
                            closest = closest.min(away - reach - RADIUS);
                        }
                        assert!(
                            away >= reach + RADIUS - 1e-4,
                            "{rate} Hz, call {frame}: a tail is {away} from the {name}'s collider"
                        );
                    }
                }
            },
        );
        assert!(
            closest < 1e-3,
            "{rate} Hz: the sheet came no nearer than {closest} to the leg"
        );
    }
}

// Plausible defect: a sheet refusal missed, or reported for the wrong
// sheet or chain.
// Oracle: each case breaks exactly one documented requirement of `new`.
#[wasm_bindgen_test(unsupported = test)]
fn each_sheet_refusal_names_its_cause() {
    let (rig, chains) = hanging(&ring(3, 0.12), &[3; 3], Vec3::NEG_Y * BONE);
    let sheet = |chains: &[usize]| Sheet {
        chains: chains.to_vec(),
        closed: false,
    };
    let refused = |sheets: &[Sheet]| SpringBones::new(&rig, &chains, sheets, &[]).err();
    assert_eq!(
        refused(&[sheet(&[0, 1]), sheet(&[2, 5])]),
        Some(SpringBonesError::SheetChain { sheet: 1, chain: 5 })
    );
    assert_eq!(
        refused(&[sheet(&[0, 1]), sheet(&[2, 1])]),
        Some(SpringBonesError::SharedChain {
            first: 0,
            second: 1,
            chain: 1
        })
    );
    assert_eq!(
        refused(&[sheet(&[0, 1, 0])]),
        Some(SpringBonesError::SharedChain {
            first: 0,
            second: 0,
            chain: 0
        })
    );
    assert!(refused(&[sheet(&[2, 0, 1])]).is_none());
}

// Plausible defects: links across the grid's cells, or anything else that
// keeps a sheet from shearing, so a skirt cannot swing the way the body
// moves or the wind blows.
// Oracle: the closed form of `one_bone_settles_between_its_rest_direction_and_down`
// at s = 0, with a level force added to gravity: every bone settles along
// normalize(g·down + force). Six parallel chains hanging in a ring, tied as
// a closed sheet, all turning the same way, keep every link between tails
// at one depth at its length, so the sheet must settle as its chains do.
#[wasm_bindgen_test(unsupported = test)]
fn a_closed_sheet_shears_as_its_chains_swing() {
    let (rig, chains) = hanging(&ring(6, 0.12), &[5; 6], Vec3::NEG_Y * BONE);
    let chains: Vec<Chain> = chains
        .into_iter()
        .map(|chain| Chain {
            stiffness: 0.,
            gravity: 1.,
            ..chain
        })
        .collect();
    let mut springs = SpringBones::new(
        &rig,
        &chains,
        &[Sheet {
            chains: (0..6).collect(),
            closed: true,
        }],
        &[],
    )
    .unwrap();
    let world = Mat4::from_rotation_translation(Quat::from_rotation_y(0.7), Vec3::new(1., 2., 3.));
    let force = Vec3::new(0.6, 0., -0.3);
    let mut last = Pose::rest(&rig);
    run(
        &mut springs,
        &rig,
        600,
        FRAME,
        force,
        |_| world,
        |_, pose, _| last = pose.clone(),
    );
    let at = positions(&rig, &last, world);
    let tails = drawn_tails(&rig, &last, world, &chains);
    let expected = force + Vec3::NEG_Y;
    for (index, chain) in chains.iter().enumerate() {
        let mut head = at[chain.nodes[0]];
        for &tail in &tails[index] {
            assert_direction(tail - head, expected, 1e-3);
            head = tail;
        }
    }
}

/// Two chains 0.1 m apart tied as a sheet, one losing a tenth of its
/// motion a step and the other six tenths, so apart they swing apart, on a
/// body that moves 1 m/s along X and grows from 1 to 2 times its size
/// over half a second, then holds, at `rate` calls a second. Returns both
/// chains' tails at 0.5 s and at 1 s.
fn sheet_at_rate(rate: u32) -> [Vec<Vec<Vec3>>; 2] {
    let (rig, chains) = hanging(
        &[Vec3::new(-0.05, 1., 0.), Vec3::new(0.05, 1., 0.)],
        &[4; 2],
        Vec3::NEG_Y * BONE,
    );
    let chains = [
        Chain {
            drag: 0.1,
            ..chains[0].clone()
        },
        Chain {
            drag: 0.6,
            ..chains[1].clone()
        },
    ];
    let mut springs = SpringBones::new(
        &rig,
        &chains,
        &[Sheet {
            chains: vec![0, 1],
            closed: false,
        }],
        &[],
    )
    .unwrap();
    let motion = |t: f32| {
        let t = t.min(0.5);
        Mat4::from_translation(Vec3::X * t) * Mat4::from_scale(Vec3::splat(1. + 2. * t))
    };
    springs.step(&rig, &mut Pose::rest(&rig), motion(0.), Vec3::ZERO, 0.);
    let mut tails = [Vec::new(), Vec::new()];
    run(
        &mut springs,
        &rig,
        rate as usize,
        1. / rate as f32,
        Vec3::ZERO,
        motion,
        |frame, pose, world| {
            if frame as u32 * 2 == rate {
                tails[0] = drawn_tails(&rig, pose, world, &chains);
            }
            if frame as u32 == rate {
                tails[1] = drawn_tails(&rig, pose, world, &chains);
            }
        },
    );
    tails
}

// Plausible defects: a link's length taken from the call's world rather
// than each fixed step's, or a length the sheet's rounds hold from another
// step, so a sheet on a growing body moves differently at another rate.
// Oracle: the equivalence of `the_frame_rate_does_not_change_the_motion`:
// the motion is linear between calls at every rate, so the steps are the
// same at 30, 60 and 144 calls a second, to rounding under 1 mm.
#[wasm_bindgen_test(unsupported = test)]
fn the_frame_rate_does_not_change_a_sheet() {
    let at_60 = sheet_at_rate(60);
    for rate in [30, 144] {
        for (time, (tails, reference)) in sheet_at_rate(rate).iter().zip(&at_60).enumerate() {
            for (tail, expected) in tails.iter().flatten().zip(reference.iter().flatten()) {
                assert!(
                    tail.distance(*expected) < 1e-3,
                    "at {} s and {rate} calls a second a tail is at {tail}, at 60 {expected}",
                    (time + 1) as f32 * 0.5
                );
            }
        }
    }
}

/// Two chains 0.1 m apart tied as a sheet, hanging, with a sphere 0.02 m
/// in radius just behind the middle of their second link (0.04 m with the
/// chains' radius, so it misses both tails and presses the link 2 cm
/// forward), settled for ten seconds under a turned, moved instance scaled
/// by `scale`, with stiffness and gravity, which are in world units, scaled
/// with it. Returns the tails' offsets from the instance's origin.
fn pressed(scale: f32) -> Vec<Vec3> {
    let (rig, chains) = hanging(
        &[Vec3::new(-0.05, 1., 0.), Vec3::new(0.05, 1., 0.)],
        &[4; 2],
        Vec3::NEG_Y * BONE,
    );
    let chains: Vec<Chain> = chains
        .into_iter()
        .map(|chain| Chain {
            stiffness: chain.stiffness * scale,
            gravity: chain.gravity * scale,
            ..chain
        })
        .collect();
    let sphere = Collider {
        node: 0,
        shape: ColliderShape::Sphere {
            center: Vec3::new(0., 1. - 2. * BONE, -0.02),
            radius: 0.02,
        },
    };
    let mut springs = SpringBones::new(
        &rig,
        &chains,
        &[Sheet {
            chains: vec![0, 1],
            closed: false,
        }],
        &[sphere],
    )
    .unwrap();
    let origin = Vec3::new(1., 2., 3.);
    let world = Mat4::from_scale_rotation_translation(
        Vec3::splat(scale),
        Quat::from_rotation_y(0.7),
        origin,
    );
    let mut last = Pose::rest(&rig);
    run(
        &mut springs,
        &rig,
        600,
        FRAME,
        Vec3::ZERO,
        |_| world,
        |_, pose, _| last = pose.clone(),
    );
    drawn_tails(&rig, &last, world, &chains)
        .into_iter()
        .flatten()
        .map(|tail| tail - origin)
        .collect()
}

// Plausible defects: a link's collision radius, or its length, not scaled
// with the instance, so a doubled character's sheet sits closer to its
// colliders or spaced differently than the same character's.
// Oracle: scale invariance, as `a_doubled_instance_doubles_lengths_and_colliders`:
// with every length doubled, the sphere's included, and the world-unit
// stiffness and gravity doubled too, each step is the single instance's
// doubled, so the pressed sheet settles with every tail's offset from the
// instance twice the single one's, within that test's 1 mm.
#[wasm_bindgen_test(unsupported = test)]
fn a_doubled_instance_doubles_a_pressed_sheet() {
    let (single, doubled) = (pressed(1.), pressed(2.));
    for (single, doubled) in single.iter().zip(&doubled) {
        assert!(
            doubled.abs_diff_eq(*single * 2., 1e-3),
            "a tail settles {doubled} from the doubled instance, not {}",
            *single * 2.
        );
    }
}

/// A closed skirt of eight chains round a 0.15 m ring, five bones each,
/// and a collider on the leg driven from the body's axis at 0.75 m out
/// between the first two chains by 0.35 m over a second, then held for
/// one, at `rate` calls a second, under an instance scaled by `scale`
/// with the chains' world-unit stiffness and gravity scaled alike, so
/// every scale runs the same motion at its own size. Returns the most the
/// first two chains' tails at any depth spread past their rest spacing, as
/// a fraction of it.
fn wedged(shape: ColliderShape, rate: u32, scale: f32) -> f32 {
    let (rig, chains) = hanging(&ring(8, 0.15), &[5; 8], Vec3::NEG_Y * BONE);
    let chains: Vec<Chain> = chains
        .into_iter()
        .map(|chain| Chain {
            stiffness: chain.stiffness * scale,
            gravity: chain.gravity * scale,
            ..chain
        })
        .collect();
    let world = Mat4::from_scale(Vec3::splat(scale));
    let mut springs = SpringBones::new(
        &rig,
        &chains,
        &[Sheet {
            chains: (0..8).collect(),
            closed: true,
        }],
        &[Collider { node: LEG, shape }],
    )
    .unwrap();
    let rest = drawn_tails(&rig, &Pose::rest(&rig), world, &chains);
    let between = Quat::from_rotation_y(-22.5f32.to_radians()) * Vec3::X;
    let mut most = 0f32;
    kick(
        &mut springs,
        &rig,
        &chains,
        rate,
        2.,
        stride(Vec3::Y * 0.75, Vec3::Y * 0.75 + between * 0.35),
        |_| world,
        |_, tails, _, _, _| {
            for (depth, (a, b)) in tails[0].iter().zip(&tails[1]).enumerate() {
                let gap = rest[0][depth].distance(rest[1][depth]);
                most = most.max((a.distance(*b) - gap) / gap);
            }
        },
    );
    most
}

// Plausible defects: links that do not collide, or are pushed the wrong
// way, so a leg wider than the gap between two chains of a skirt wedges
// them apart and goes between them (measured with link collision
// disabled: their link stretches 54% to 55% at these rates).
// Oracle: the skirt's rest geometry. Eight chains round a 0.15 m ring are
// 0.115 m apart; a sphere 0.07 m in radius, 0.18 m wide with the chains'
// radius, can pass between two of them only by stretching their link 57%.
// Driven out from inside the skirt between them, it leaves that link at
// every depth no more than 10% longer than its length. (Pressed outward,
// the link is also briefly shortened, by up to 12%; shortening does not let
// a collider through.)
#[wasm_bindgen_test(unsupported = test)]
fn a_sphere_wider_than_the_gap_cannot_part_two_chains() {
    for rate in [30, 60, 90, 144] {
        let most = wedged(
            ColliderShape::Sphere {
                center: Vec3::ZERO,
                radius: 0.07,
            },
            rate,
            1.,
        );
        assert!(
            most < 0.1,
            "{rate} Hz: the two chains' spacing spreads {most} past its length"
        );
    }
}

// Plausible defects: as for the sphere, and the nearest points of a link
// and the capsule's segment taken wrongly (measured with link collision
// disabled: the link stretches 55% to 57% at these rates), or by a test
// that holds at one size only (a fixed threshold for parallel segments
// takes every short link as parallel to the capsule: 23% at a quarter
// size).
// Oracle: as for the sphere. An upright capsule 0.07 m in radius across
// the skirt's lower rows, a leg, driven out between two chains, leaves
// their link at every depth no more than 10% longer than its length, for
// the character at its own size and at a quarter of it.
#[wasm_bindgen_test(unsupported = test)]
fn a_capsule_wider_than_the_gap_cannot_part_two_chains() {
    for (rate, scale) in [(30, 1.), (60, 1.), (90, 1.), (144, 1.), (60, 0.25)] {
        let most = wedged(
            ColliderShape::Capsule {
                head: Vec3::Y * 0.1,
                tail: Vec3::NEG_Y * 0.1,
                radius: 0.07,
            },
            rate,
            scale,
        );
        assert!(
            most < 0.1,
            "{rate} Hz, scale {scale}: the two chains' spacing spreads {most} past its length"
        );
    }
}
