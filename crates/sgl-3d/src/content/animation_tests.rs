//! Clips sampled, blended and layered, against values worked by hand from
//! the glTF 2.0 specification's interpolation formulas (Appendix C) and
//! three.js's additive mode, never from the code under test.
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};

use glam::{Quat, Vec3};
use wasm_bindgen_test::wasm_bindgen_test;

use super::{Pose, Wrap};
use crate::deformation::{
    Channel, ChannelValues, Clip, Interpolation, MorphWeight, Node, Rig, Transform,
};

const TOLERANCE: f32 = 1e-5;

fn node(parent: Option<usize>, rest: Transform) -> Node {
    Node {
        name: None,
        parent,
        rest,
    }
}

/// A rig of `nodes` identity-rest nodes, each a root, with `morphs` morph
/// weights (node, rest).
fn rig(nodes: usize, morphs: &[(usize, f32)]) -> Rig {
    Rig {
        nodes: (0..nodes)
            .map(|_| node(None, Transform::IDENTITY))
            .collect(),
        morph_weights: morphs
            .iter()
            .map(|&(node, rest)| MorphWeight { node, rest })
            .collect(),
        ..Default::default()
    }
}

fn channel(
    node: usize,
    interpolation: Interpolation,
    times: &[f32],
    values: ChannelValues,
) -> Channel {
    Channel {
        node,
        interpolation,
        times: times.to_vec(),
        values,
    }
}

fn clip(channels: Vec<Channel>) -> Clip {
    Clip {
        name: None,
        channels,
    }
}

fn sampled(rig: &Rig, clip: &Clip, time: f32, wrap: Wrap) -> Pose {
    let mut pose = Pose::rest(rig);
    pose.sample(rig, clip, time, wrap);
    pose
}

#[track_caller]
fn assert_vec3(actual: Vec3, expected: Vec3) {
    assert!(
        actual.abs_diff_eq(expected, TOLERANCE),
        "{actual} is not {expected}"
    );
}

/// The same rotation: `q` and `-q` are one rotation.
#[track_caller]
fn assert_rotation(actual: Quat, expected: Quat) {
    assert!(
        (actual.length() - 1.).abs() < TOLERANCE,
        "{actual} is not a unit quaternion"
    );
    assert!(
        actual.abs_diff_eq(expected, TOLERANCE) || actual.abs_diff_eq(-expected, TOLERANCE),
        "{actual} is not {expected}"
    );
}

// Plausible defects: the fraction taken over the clip or a fixed interval
// instead of the keyframes' own span, or the wrong span chosen.
// Oracle: glTF's v = (1 - t)·v_k + t·v_k+1 with t = (time - t_k) / (t_k+1 - t_k),
// worked by hand for keys at 0, 1 and 4.
#[wasm_bindgen_test(unsupported = test)]
fn linear_translation_interpolates_within_unequal_key_intervals() {
    let rig = rig(1, &[]);
    let clip = clip(vec![channel(
        0,
        Interpolation::Linear,
        &[0., 1., 4.],
        ChannelValues::Translation(vec![
            Vec3::ZERO,
            Vec3::new(2., 0., 0.),
            Vec3::new(8., 4., 0.),
        ]),
    )]);
    let at = |time| sampled(&rig, &clip, time, Wrap::Clamp).nodes[0].translation;
    assert_vec3(at(0.5), Vec3::new(1., 0., 0.));
    // (2.5 - 1) / 3 = 0.5 of the way from (2, 0, 0) to (8, 4, 0).
    assert_vec3(at(2.5), Vec3::new(5., 2., 0.));
    // (3 - 1) / 3 = 2/3 of the way.
    assert_vec3(at(3.), Vec3::new(6., 8. / 3., 0.));
}

// Plausible defect: rotations interpolated component-wise without
// renormalising, or by an angle that is not linear in time.
// Oracle: slerp between two rotations about one axis turns the angle
// linearly: halfway from 0 to 90 degrees about Y is 45 degrees, and keys at
// 1 and 3 seconds put 2 seconds halfway between 90 and 180 degrees.
#[wasm_bindgen_test(unsupported = test)]
fn linear_rotation_turns_the_angle_linearly() {
    let rig = rig(1, &[]);
    let clip = clip(vec![channel(
        0,
        Interpolation::Linear,
        &[0., 1., 3.],
        ChannelValues::Rotation(vec![
            Quat::IDENTITY,
            Quat::from_rotation_y(FRAC_PI_2),
            Quat::from_rotation_y(PI),
        ]),
    )]);
    let at = |time| sampled(&rig, &clip, time, Wrap::Clamp).nodes[0].rotation;
    assert_rotation(at(0.5), Quat::from_rotation_y(FRAC_PI_4));
    assert_rotation(at(2.), Quat::from_rotation_y(0.75 * PI));
    assert_rotation(at(0.25), Quat::from_rotation_y(PI / 8.));
}

/// glTF's spherical linear interpolation (Appendix C), written out in f64:
/// a = acos(|d|), s = d / |d|, v = sin(a(1 - t)) / sin(a)·q0 + s·sin(at) / sin(a)·q1.
fn gltf_slerp(q0: [f64; 4], q1: [f64; 4], t: f64) -> Quat {
    let d: f64 = q0.iter().zip(&q1).map(|(a, b)| a * b).sum();
    let a = d.abs().acos();
    let s = d.signum();
    let k0 = (a * (1. - t)).sin() / a.sin();
    let k1 = s * (a * t).sin() / a.sin();
    let v: Vec<f64> = q0.iter().zip(&q1).map(|(a, b)| k0 * a + k1 * b).collect();
    Quat::from_xyzw(v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32)
}

// Plausible defect: a second key stored as the negated quaternion (the same
// rotation) interpolated the long way round, through the opposite
// orientation. Oracles: halfway from 0 to 170 degrees about Z the short
// way is 85 degrees (the long way, 265); and glTF's formula worked in f64
// for keys about a tilted axis at a fraction that is not a midpoint.
#[wasm_bindgen_test(unsupported = test)]
fn antipodal_linear_rotation_keys_take_the_shorter_way() {
    let rig = rig(1, &[]);
    let far = Quat::from_rotation_z(170f32.to_radians());
    let clip_z = clip(vec![channel(
        0,
        Interpolation::Linear,
        &[0., 1.],
        ChannelValues::Rotation(vec![Quat::IDENTITY, -far]),
    )]);
    let halfway = sampled(&rig, &clip_z, 0.5, Wrap::Clamp).nodes[0].rotation;
    assert_rotation(halfway, Quat::from_rotation_z(85f32.to_radians()));

    let axis = Vec3::new(1., 2., 2.).normalize();
    let from = Quat::from_axis_angle(axis, 0.3);
    let to = Quat::from_axis_angle(axis, 2.1);
    let clip_tilted = clip(vec![channel(
        0,
        Interpolation::Linear,
        &[0., 2.],
        ChannelValues::Rotation(vec![from, -to]),
    )]);
    let f64s = |q: Quat| q.to_array().map(f64::from);
    let expected = gltf_slerp(f64s(from), f64s(-to), 0.3);
    assert_rotation(
        sampled(&rig, &clip_tilted, 0.6, Wrap::Clamp).nodes[0].rotation,
        expected,
    );
    // The same rotation as the short way about the axis: 0.3 + 0.3·1.8.
    assert_rotation(expected, Quat::from_axis_angle(axis, 0.84));
}

// Plausible defects: tangents not scaled by the key interval, the wrong
// keyframe's tangent (in for out), or the in-tangent and out-tangent
// swapped in the keyframe triplet.
// Oracle: glTF's Hermite spline worked by hand. Keys at 0, 2 and 3 seconds:
// span 2 then span 1. Basis at t = 0.25: h00 = 0.84375, h10 = 0.140625,
// h01 = 0.15625, h11 = -0.046875; at t = 0.5: 0.5, 0.125, 0.5, -0.125.
#[wasm_bindgen_test(unsupported = test)]
fn cubic_spline_scales_tangents_by_the_key_interval() {
    let rig = rig(1, &[]);
    let x = |v: f32| Vec3::new(v, 0., 0.);
    // Keyframes: (in-tangent, value, out-tangent).
    let values = vec![
        x(100.),
        x(0.),
        x(1.), // key 0 at 0 s; its in-tangent is never used
        x(3.),
        x(1.),
        x(-2.), // key 1 at 2 s
        x(4.),
        x(5.),
        x(100.), // key 2 at 3 s; its out-tangent is never used
    ];
    let clip = clip(vec![channel(
        0,
        Interpolation::CubicSpline,
        &[0., 2., 3.],
        ChannelValues::Translation(values),
    )]);
    let at = |time| {
        sampled(&rig, &clip, time, Wrap::Clamp).nodes[0]
            .translation
            .x
    };
    // t = 0.5 s / 2 s = 0.25: 0.84375·0 + 2·0.140625·1 + 0.15625·1 + 2·(-0.046875)·3
    // = 0.28125 + 0.15625 - 0.28125 = 0.15625.
    assert!((at(0.5) - 0.15625).abs() < TOLERANCE, "{}", at(0.5));
    // t = 0.5 s / 1 s = 0.5: 0.5·1 + 1·0.125·(-2) + 0.5·5 + 1·(-0.125)·4
    // = 0.5 - 0.25 + 2.5 - 0.5 = 2.25.
    assert!((at(2.5) - 2.25).abs() < TOLERANCE, "{}", at(2.5));
    // At and outside the keys, the keyframe's value, not a tangent.
    assert!((at(2.) - 1.).abs() < TOLERANCE);
    assert!((at(-1.) - 0.).abs() < TOLERANCE);
    assert!((at(9.) - 5.).abs() < TOLERANCE);
}

// Plausible defect: a cubic-spline rotation left unnormalised, or slerped
// instead of splined. Oracle: with zero tangents the spline at the midpoint
// is the mean of the two keys (0.5·v0 + 0.5·v1); normalised, the mean of
// 0 and 90 degrees about X is 45 degrees. With non-zero tangents the
// result is glTF's spline worked by hand, normalised.
#[wasm_bindgen_test(unsupported = test)]
fn cubic_spline_rotations_are_normalised() {
    let rig = rig(1, &[]);
    let zero = Quat::from_xyzw(0., 0., 0., 0.);
    let quarter = Quat::from_rotation_x(FRAC_PI_2);
    let flat = clip(vec![channel(
        0,
        Interpolation::CubicSpline,
        &[0., 1.],
        ChannelValues::Rotation(vec![zero, Quat::IDENTITY, zero, zero, quarter, zero]),
    )]);
    assert_rotation(
        sampled(&rig, &flat, 0.5, Wrap::Clamp).nodes[0].rotation,
        Quat::from_rotation_x(FRAC_PI_4),
    );
    // Out-tangent (0, 0, 0, 1) on key 0, span 2, t = 0.5:
    // 0.5·(0, 0, 0, 1) + 2·0.125·(0, 0, 0, 1) + 0.5·(s, 0, 0, s) with
    // s = sin 45° = cos 45°, so (0.5s, 0, 0, 0.75 + 0.5s), normalised.
    let tangent = Quat::from_xyzw(0., 0., 0., 1.);
    let leaning = clip(vec![channel(
        0,
        Interpolation::CubicSpline,
        &[0., 2.],
        ChannelValues::Rotation(vec![zero, Quat::IDENTITY, tangent, zero, quarter, zero]),
    )]);
    let s = FRAC_PI_4.sin();
    let expected = Quat::from_xyzw(0.5 * s, 0., 0., 0.75 + 0.5 * s).normalize();
    assert_rotation(
        sampled(&rig, &leaning, 1., Wrap::Clamp).nodes[0].rotation,
        expected,
    );
}

// Plausible defects: a step key changing before its successor's time, times
// outside the keys extrapolated, or a loop wrapping by the wrong duration.
// Oracle: glTF's STEP holds v_k over [t_k, t_k+1), the first key before the
// start, the last after the end; a 2-second clip looped at 3.25 seconds
// samples 1.25 seconds.
#[wasm_bindgen_test(unsupported = test)]
fn step_holds_and_wrapping_clamps_or_loops() {
    let rig = rig(1, &[]);
    let x = |v: f32| Vec3::new(v, 0., 0.);
    let clip = clip(vec![channel(
        0,
        Interpolation::Step,
        &[0., 1., 2.],
        ChannelValues::Translation(vec![x(10.), x(20.), x(30.)]),
    )]);
    let at = |time, wrap| sampled(&rig, &clip, time, wrap).nodes[0].translation.x;
    assert_eq!(at(0.999, Wrap::Clamp), 10.);
    assert_eq!(at(1., Wrap::Clamp), 20.);
    assert_eq!(at(1.5, Wrap::Clamp), 20.);
    assert_eq!(at(-1., Wrap::Clamp), 10.);
    assert_eq!(at(3.25, Wrap::Clamp), 30.);
    assert_eq!(at(3.25, Wrap::Loop), 20.);
    assert_eq!(at(2.5, Wrap::Loop), 10.);
    assert_eq!(at(-0.5, Wrap::Loop), 20.);
    assert_eq!(at(f32::NAN, Wrap::Loop), 10.);
    assert_eq!(at(f32::NAN, Wrap::Clamp), 10.);
}

// Plausible defects: a property the clip does not animate left at the value
// the pose held from the previous clip, or zeroed instead of at rest.
// Oracle: the rig's rest values, set by the test.
#[wasm_bindgen_test(unsupported = test)]
fn a_channel_the_clip_lacks_keeps_its_rest_value() {
    let rest = Transform {
        translation: Vec3::new(0., 1., 0.),
        rotation: Quat::from_rotation_x(0.4),
        scale: Vec3::splat(2.),
    };
    let rig = Rig {
        nodes: vec![node(None, Transform::IDENTITY), node(Some(0), rest)],
        morph_weights: vec![MorphWeight { node: 1, rest: 0.3 }],
        ..Default::default()
    };
    let moves_node_1 = clip(vec![
        channel(
            1,
            Interpolation::Linear,
            &[0., 1.],
            ChannelValues::Rotation(vec![Quat::IDENTITY, Quat::from_rotation_y(1.)]),
        ),
        channel(
            1,
            Interpolation::Linear,
            &[0., 1.],
            ChannelValues::MorphWeights(vec![0., 1.]),
        ),
    ]);
    let moves_node_0 = clip(vec![channel(
        0,
        Interpolation::Linear,
        &[0., 1.],
        ChannelValues::Translation(vec![Vec3::ZERO, Vec3::X]),
    )]);
    let mut pose = Pose::rest(&rig);
    pose.sample(&rig, &moves_node_1, 0.5, Wrap::Clamp);
    pose.sample(&rig, &moves_node_0, 0.5, Wrap::Clamp);
    assert_vec3(pose.nodes[0].translation, Vec3::new(0.5, 0., 0.));
    assert_eq!(pose.nodes[1], rest);
    assert_eq!(pose.morph_weights, vec![0.3]);
}

// Plausible defects: a crossfade weighted the wrong way round, rotations
// mixed the long way or unnormalised, morph weights left out of the blend.
// Oracle: lerp and same-axis slerp worked by hand.
#[wasm_bindgen_test(unsupported = test)]
fn two_clip_crossfade_weights_each_pose() {
    let rig = rig(1, &[(0, 0.)]);
    let still = clip(vec![channel(
        0,
        Interpolation::Step,
        &[0.],
        ChannelValues::MorphWeights(vec![1.]),
    )]);
    let turned = clip(vec![
        channel(
            0,
            Interpolation::Step,
            &[0.],
            ChannelValues::Translation(vec![Vec3::new(4., 0., 0.)]),
        ),
        // The negated quaternion of a quarter turn: the same rotation.
        channel(
            0,
            Interpolation::Step,
            &[0.],
            ChannelValues::Rotation(vec![-Quat::from_rotation_z(FRAC_PI_2)]),
        ),
        channel(
            0,
            Interpolation::Step,
            &[0.],
            ChannelValues::Scale(vec![Vec3::splat(3.)]),
        ),
    ]);
    let from = sampled(&rig, &still, 0., Wrap::Clamp);
    let to = sampled(&rig, &turned, 0., Wrap::Clamp);
    let blended = |weight| {
        let mut pose = from.clone();
        pose.blend(&to, weight);
        pose
    };
    assert_eq!(blended(0.), from);
    let all = blended(1.);
    assert_vec3(all.nodes[0].translation, Vec3::new(4., 0., 0.));
    assert_rotation(all.nodes[0].rotation, Quat::from_rotation_z(FRAC_PI_2));
    assert_eq!(all.morph_weights, vec![0.]);
    let quarter = blended(0.25);
    assert_vec3(quarter.nodes[0].translation, Vec3::new(1., 0., 0.));
    assert_rotation(quarter.nodes[0].rotation, Quat::from_rotation_z(PI / 8.));
    assert_vec3(quarter.nodes[0].scale, Vec3::splat(1.5));
    assert!((quarter.morph_weights[0] - 0.75).abs() < TOLERANCE);
    // Weights clamp to 0..=1.
    assert_eq!(blended(1.5), blended(1.));
    assert_eq!(blended(-1.), from);
}

// Plausible defects: the additive difference taken without the reference
// (the reference added twice), the rotation delta applied on the wrong side
// or as an absolute rotation, nodes the additive clip leaves alone moved,
// and morph weights replaced instead of added to.
// Oracle: three.js's additive mode worked by hand: an arm resting at 30
// degrees about X; an additive wave whose first frame, the reference, is
// 20 degrees about Z and whose last is that followed by 60 degrees about
// Y, so its delta (reference⁻¹ · additive) is 60 degrees about Y, applied
// after the base rotation. None of the three axes commute, so a delta
// taken or applied on the other side lands elsewhere. A translation, scale
// and morph weight add w·(a - r), with a reference that is not zero (or one
// for scale), so a difference that drops the reference or multiplies
// scales lands elsewhere.
#[wasm_bindgen_test(unsupported = test)]
fn additive_upper_body_and_morph_layer_on_the_base() {
    // Hips (0), spine (1), arm (2) and leg (3); the face's morph weight on
    // the spine.
    let arm_rest = Quat::from_rotation_x(30f32.to_radians());
    let rig = Rig {
        nodes: vec![
            node(None, Transform::IDENTITY),
            node(Some(0), Transform::IDENTITY),
            node(
                Some(1),
                Transform {
                    rotation: arm_rest,
                    scale: Vec3::splat(1.5),
                    ..Transform::IDENTITY
                },
            ),
            node(Some(0), Transform::IDENTITY),
        ],
        morph_weights: vec![MorphWeight { node: 1, rest: 0. }],
        ..Default::default()
    };
    let walk = clip(vec![
        channel(
            0,
            Interpolation::Linear,
            &[0., 1.],
            ChannelValues::Translation(vec![Vec3::ZERO, Vec3::new(0., 0., 2.)]),
        ),
        channel(
            3,
            Interpolation::Linear,
            &[0., 1.],
            ChannelValues::Rotation(vec![Quat::IDENTITY, Quat::from_rotation_x(1.)]),
        ),
        channel(
            1,
            Interpolation::Linear,
            &[0., 1.],
            ChannelValues::MorphWeights(vec![0.2, 0.2]),
        ),
    ]);
    let wave = clip(vec![
        channel(
            2,
            Interpolation::Linear,
            &[0., 1.],
            ChannelValues::Rotation(vec![
                Quat::from_rotation_z(20f32.to_radians()),
                Quat::from_rotation_z(20f32.to_radians()) * Quat::from_rotation_y(PI / 3.),
            ]),
        ),
        channel(
            2,
            Interpolation::Linear,
            &[0., 1.],
            ChannelValues::Translation(vec![Vec3::Y, Vec3::new(0., 1.4, 0.)]),
        ),
        channel(
            2,
            Interpolation::Linear,
            &[0., 1.],
            ChannelValues::Scale(vec![Vec3::splat(2.), Vec3::splat(3.)]),
        ),
        channel(
            1,
            Interpolation::Linear,
            &[0., 1.],
            ChannelValues::MorphWeights(vec![0.1, 0.7]),
        ),
    ]);
    let base = sampled(&rig, &walk, 0.5, Wrap::Clamp);
    let reference = sampled(&rig, &wave, 0., Wrap::Clamp);
    let additive = sampled(&rig, &wave, 1., Wrap::Clamp);
    let mut layered = base.clone();
    layered.add(&additive, &reference, 0.5);
    // Half of 60 degrees about Y, after the arm's rest.
    assert_rotation(
        layered.nodes[2].rotation,
        arm_rest * Quat::from_rotation_y(PI / 6.),
    );
    // The arm rests at the origin and scale 1.5: 0 + 0.5·(1.4 - 1) and
    // 1.5 + 0.5·(3 - 2).
    assert_vec3(layered.nodes[2].translation, Vec3::new(0., 0.2, 0.));
    assert_vec3(layered.nodes[2].scale, Vec3::splat(2.));
    // 0.2 + 0.5·(0.7 - 0.1).
    assert!((layered.morph_weights[0] - 0.5).abs() < TOLERANCE);
    for node in [0, 1, 3] {
        assert_vec3(
            layered.nodes[node].translation,
            base.nodes[node].translation,
        );
        assert_rotation(layered.nodes[node].rotation, base.nodes[node].rotation);
        assert_vec3(layered.nodes[node].scale, base.nodes[node].scale);
    }
    // At full weight the arm reaches its rest followed by the whole delta.
    let mut full = base.clone();
    full.add(&additive, &reference, 1.);
    assert_rotation(
        full.nodes[2].rotation,
        arm_rest * Quat::from_rotation_y(PI / 3.),
    );
    // A reference rotation that is not unit length (a game-built pose)
    // gives the same delta.
    let mut scaled_reference = reference.clone();
    scaled_reference.nodes[2].rotation = reference.nodes[2].rotation * 2.;
    let mut scaled = base.clone();
    scaled.add(&additive, &scaled_reference, 0.5);
    assert_rotation(scaled.nodes[2].rotation, layered.nodes[2].rotation);
    // A NaN weight adds nothing.
    let mut unweighted = base.clone();
    unweighted.add(&additive, &reference, f32::NAN);
    assert_eq!(unweighted, base);
}

// Plausible defects: a node's morph weights read with a single target's
// stride, or a cubic spline's weights read as (in, value, out) per target
// instead of all in-tangents, all values, then all out-tangents (glTF
// 3.11). Oracle: the Hermite basis at t = 0.5 (0.5, 0.125, 0.5, -0.125)
// worked by hand for two targets with distinct tangents.
#[wasm_bindgen_test(unsupported = test)]
fn morph_weights_follow_gltfs_keyframe_layout() {
    // Node 1's two weights follow node 0's one.
    let rig = rig(2, &[(0, 0.), (1, 0.), (1, 0.)]);
    #[rustfmt::skip]
    let cubic = clip(vec![channel(
        1,
        Interpolation::CubicSpline,
        &[0., 1.],
        ChannelValues::MorphWeights(vec![
            9., 9., /* values */ 0., 1., /* out */ 1., 0., // key 0
            2., -1., /* values */ 1., 0., /* out */ 9., 9., // key 1
        ]),
    )]);
    let pose = sampled(&rig, &cubic, 0.5, Wrap::Clamp);
    // Target 0: 0.5·0 + 0.125·1 + 0.5·1 - 0.125·2 = 0.375.
    // Target 1: 0.5·1 + 0.125·0 + 0.5·0 - 0.125·(-1) = 0.625.
    assert_eq!(pose.morph_weights[0], 0.);
    assert!(
        (pose.morph_weights[1] - 0.375).abs() < TOLERANCE,
        "{:?}",
        pose.morph_weights
    );
    assert!(
        (pose.morph_weights[2] - 0.625).abs() < TOLERANCE,
        "{:?}",
        pose.morph_weights
    );

    let linear = clip(vec![channel(
        1,
        Interpolation::Linear,
        &[0., 2.],
        ChannelValues::MorphWeights(vec![0., 1., 1., 0.]),
    )]);
    let pose = sampled(&rig, &linear, 0.5, Wrap::Clamp);
    assert_eq!(pose.morph_weights, vec![0., 0.25, 0.75]);
}

// Plausible defect: a game-built clip that does not fit the rig panics
// (an index out of range) or writes another node's or weight's value.
// Oracle: each such channel is skipped, leaving the rest pose.
#[wasm_bindgen_test(unsupported = test)]
fn channels_the_rig_cannot_take_are_skipped() {
    let rig = rig(2, &[(0, 0.5)]);
    let misfits = clip(vec![
        // A node the rig lacks.
        channel(
            7,
            Interpolation::Linear,
            &[0., 1.],
            ChannelValues::Translation(vec![Vec3::X; 2]),
        ),
        // Two keyframes, three values.
        channel(
            0,
            Interpolation::Linear,
            &[0., 1.],
            ChannelValues::Translation(vec![Vec3::X; 3]),
        ),
        // A cubic spline with one value a keyframe.
        channel(
            0,
            Interpolation::CubicSpline,
            &[0., 1.],
            ChannelValues::Scale(vec![Vec3::X; 2]),
        ),
        // No keyframes.
        channel(
            0,
            Interpolation::Step,
            &[],
            ChannelValues::Rotation(Vec::new()),
        ),
        // Morph weights for a node without any, and with the wrong count.
        channel(
            1,
            Interpolation::Linear,
            &[0., 1.],
            ChannelValues::MorphWeights(vec![1., 1.]),
        ),
        channel(
            0,
            Interpolation::Linear,
            &[0., 1.],
            ChannelValues::MorphWeights(vec![1.; 4]),
        ),
    ]);
    for time in [-1., 0., 0.5, 1., 2.] {
        assert_eq!(sampled(&rig, &misfits, time, Wrap::Loop), Pose::rest(&rig));
    }
}
