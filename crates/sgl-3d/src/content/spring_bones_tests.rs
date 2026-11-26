//! Spring-bone chains judged by the rig's geometry and by closed forms
//! worked by hand from the Verlet update, never by the solver's own output.
use glam::{Mat4, Quat, Vec3, Vec4};
use wasm_bindgen_test::wasm_bindgen_test;

use super::{Chain, ChainField, Collider, ColliderShape, SpringBones, SpringBonesError};
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
    let mut springs = SpringBones::new(&rig, &[chain(&rig)], &[]).unwrap();
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
    let mut springs = SpringBones::new(&rig, &[chain(&rig)], &[]).unwrap();
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
    let refused =
        |chains: &[Chain], colliders: &[Collider]| SpringBones::new(&rig, chains, colliders).err();
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
        let mut springs = SpringBones::new(&rig, chains, &[]).unwrap();
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
    let mut springs = SpringBones::new(&rig, &[chain(&rig)], &[]).unwrap();
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
