//! Sampling and blending a rig's animation clips into poses, on the CPU: a
//! game calls it with its own times and weights, and feeds the pose to
//! `Rig::joint_matrices` and `Scene::set_instance_deformation`. It holds no
//! clock, weight or state between calls; transitions, state machines, masks
//! and layer order are the game's.
//!
//! Sampling follows the glTF 2.0 specification's animation sampler
//! interpolation (Appendix C), whose shortest-path spherical interpolation
//! blending shares. Blending and additive layering follow three.js r187's `PropertyMixer`
//! and `AnimationUtils.makeClipAdditive` (MIT,
//! <https://github.com/mrdoob/three.js/tree/r187/src/animation>), whose
//! formulas are rewritten here, not copied.
use glam::{Mat4, Quat, Vec3, Vec4};

use super::deformation::{Channel, ChannelValues, Clip, Interpolation, Rig, Transform};

/// How a time past a clip's duration maps into it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Wrap {
    /// Each channel holds its first keyframe before its first time and its
    /// last after its last, as glTF defines.
    #[default]
    Clamp,
    /// The time wraps into `[0, duration)` (`Clip::duration`), as three.js's
    /// `LoopRepeat` does; a clip of no duration samples time zero.
    Loop,
}

/// A rig's pose: each node's transform relative to its parent and each
/// morph weight, in the rig's order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pose {
    pub nodes: Vec<Transform>,
    pub morph_weights: Vec<f32>,
}

impl Pose {
    /// `rig`'s rest pose.
    pub fn rest(rig: &Rig) -> Self {
        let mut pose = Self::default();
        pose.set_rest(rig);
        pose
    }

    /// `clip` at `time` seconds: the rig's rest pose with each of the clip's
    /// channels set as glTF interpolates it, so a node or morph weight the
    /// clip does not animate keeps its rest value. A channel the rig cannot
    /// take is skipped: one naming a node it lacks, without keyframes,
    /// whose value count does not match its keyframes and interpolation, or
    /// whose morph weights the node does not have. A NaN time samples time
    /// zero. The pose's buffers are reused.
    pub fn sample(&mut self, rig: &Rig, clip: &Clip, time: f32, wrap: Wrap) {
        self.set_rest(rig);
        let time = match wrap {
            _ if time.is_nan() => 0.,
            Wrap::Clamp => time,
            Wrap::Loop => {
                let duration = clip.duration();
                if duration > 0. && time.is_finite() {
                    time.rem_euclid(duration)
                } else {
                    0.
                }
            }
        };
        for channel in &clip.channels {
            self.sample_channel(rig, channel, time);
        }
    }

    /// Moves this pose towards `other` by `weight`, clamped to `0..=1`
    /// (zero keeps this pose, one takes `other`): translation, scale and
    /// morph weights linearly, rotation by shortest-path spherical
    /// interpolation. Several poses blend incrementally, as three.js's
    /// `PropertyMixer` accumulates: start from the first and blend the
    /// `i`th in by `w_i / (w_1 + … + w_i)`. Both
    /// poses are of one rig; entries one lacks are left as they are.
    pub fn blend(&mut self, other: &Pose, weight: f32) {
        // A NaN weight keeps this pose.
        let weight = if weight > 0. { weight.min(1.) } else { 0. };
        for (node, other) in self.nodes.iter_mut().zip(&other.nodes) {
            node.translation = node.translation.lerp(other.translation, weight);
            node.rotation = slerp(node.rotation, other.rotation, weight);
            node.scale = node.scale.lerp(other.scale, weight);
        }
        for (morph, other) in self.morph_weights.iter_mut().zip(&other.morph_weights) {
            *morph += (other - *morph) * weight;
        }
    }

    /// Adds `additive`'s difference from `reference`, scaled by `weight`,
    /// as three.js's additive blend mode does: translation, scale and morph
    /// weights add `weight * (additive - reference)`; rotation is followed
    /// by `weight` of the rotation from the reference to the additive pose
    /// (`rotation * slerp(identity, reference⁻¹ * additive, weight)`).
    /// Where `additive` equals `reference`, as at every node an additive
    /// clip does not animate when both are sampled from the same rig, the
    /// pose is unchanged. The reference is usually the additive clip
    /// sampled at its first time. All three poses are of one rig. A NaN
    /// weight adds nothing.
    pub fn add(&mut self, additive: &Pose, reference: &Pose, weight: f32) {
        if weight.is_nan() {
            return;
        }
        for (node, (additive, reference)) in self
            .nodes
            .iter_mut()
            .zip(additive.nodes.iter().zip(&reference.nodes))
        {
            node.translation += (additive.translation - reference.translation) * weight;
            // Normalised, so a non-unit reference or additive rotation
            // (a game-built pose) still gives the delta's true angle.
            let delta = (reference.rotation.conjugate() * additive.rotation).normalize();
            node.rotation = (node.rotation * slerp(Quat::IDENTITY, delta, weight)).normalize();
            node.scale += (additive.scale - reference.scale) * weight;
        }
        for (morph, (additive, reference)) in self
            .morph_weights
            .iter_mut()
            .zip(additive.morph_weights.iter().zip(&reference.morph_weights))
        {
            *morph += (additive - reference) * weight;
        }
    }

    /// Each node's matrix relative to its parent, into `locals` (cleared
    /// first and reused): `Rig::joint_matrices`'s input.
    pub fn local_matrices(&self, locals: &mut Vec<Mat4>) {
        locals.clear();
        locals.extend(self.nodes.iter().map(Transform::matrix));
    }

    /// Sets this pose to `rig`'s rest pose, reusing its buffers.
    fn set_rest(&mut self, rig: &Rig) {
        self.nodes.clear();
        self.nodes.extend(rig.nodes.iter().map(|node| node.rest));
        self.morph_weights.clear();
        self.morph_weights
            .extend(rig.morph_weights.iter().map(|weight| weight.rest));
    }

    /// Sets what `channel` animates to its value at `time`, if the rig can
    /// take it.
    fn sample_channel(&mut self, rig: &Rig, channel: &Channel, time: f32) {
        let times = channel.times.as_slice();
        let Some(node) = self
            .nodes
            .get_mut(channel.node)
            .filter(|_| !times.is_empty())
        else {
            return;
        };
        let per_key = match channel.interpolation {
            Interpolation::CubicSpline => 3,
            Interpolation::Step | Interpolation::Linear => 1,
        };
        let fits =
            |values: usize, width: usize| Some(values) == times.len().checked_mul(per_key * width);
        let segment = Segment::at(times, time);
        match &channel.values {
            ChannelValues::Translation(values) if fits(values.len(), 1) => {
                node.translation = segment.vector(channel.interpolation, values, 1, 0);
            }
            ChannelValues::Scale(values) if fits(values.len(), 1) => {
                node.scale = segment.vector(channel.interpolation, values, 1, 0);
            }
            ChannelValues::Rotation(values) if fits(values.len(), 1) => {
                node.rotation = segment.rotation(channel.interpolation, values);
            }
            ChannelValues::MorphWeights(values) => {
                // A node's morph weights are consecutive in the rig.
                let Some(first) = rig
                    .morph_weights
                    .iter()
                    .position(|weight| weight.node == channel.node)
                else {
                    return;
                };
                let width = rig.morph_weights[first..]
                    .iter()
                    .take_while(|weight| weight.node == channel.node)
                    .count();
                if !fits(values.len(), width) || first + width > self.morph_weights.len() {
                    return;
                }
                for (target, weight) in self.morph_weights[first..first + width]
                    .iter_mut()
                    .enumerate()
                {
                    *weight = segment.vector(channel.interpolation, values, width, target);
                }
            }
            _ => {}
        }
    }
}

/// Where a time falls among a channel's keyframe times.
#[derive(Clone, Copy, Debug)]
enum Segment {
    /// At or outside the keyframes: keyframe `key`'s value.
    Hold { key: usize },
    /// Between keyframes `key` and `key + 1`, `span` seconds apart, at the
    /// fraction `t` of the span.
    Between { key: usize, t: f32, span: f32 },
}

impl Segment {
    /// The segment of `times` (strictly increasing and not empty) holding
    /// `time`, found by a binary search.
    fn at(times: &[f32], time: f32) -> Self {
        let last = times.len() - 1;
        if time <= times[0] || last == 0 {
            return Self::Hold { key: 0 };
        }
        if time >= times[last] {
            return Self::Hold { key: last };
        }
        // Between the first and last times, so 1 <= after <= last when the
        // times increase; the clamp keeps a game-built channel's in range.
        let after = times
            .partition_point(|&key_time| key_time <= time)
            .clamp(1, last);
        let key = after - 1;
        let span = times[after] - times[key];
        let t = (time - times[key]) / span;
        // Times that do not increase (a game-built channel) would make `t`
        // fall outside 0..=1 or be NaN; hold the keyframe instead.
        if t.is_finite() && (0. ..=1.).contains(&t) {
            Self::Between { key, t, span }
        } else {
            Self::Hold { key }
        }
    }

    /// The value at this segment of a channel whose keyframes each hold
    /// `width` elements (a node's morph weights; one otherwise), element
    /// `item`. A cubic spline's keyframe holds its in-tangents, values and
    /// out-tangents in that order, each `width` elements.
    fn vector<T: Lerp>(
        self,
        interpolation: Interpolation,
        values: &[T],
        width: usize,
        item: usize,
    ) -> T {
        let element = |key: usize, part: usize| match interpolation {
            Interpolation::CubicSpline => values[(key * 3 + part) * width + item],
            Interpolation::Step | Interpolation::Linear => values[key * width + item],
        };
        match self {
            Self::Hold { key } => element(key, VALUE),
            Self::Between { key, t, span } => match interpolation {
                Interpolation::Step => element(key, VALUE),
                Interpolation::Linear => element(key, VALUE).lerp_to(element(key + 1, VALUE), t),
                Interpolation::CubicSpline => hermite(
                    [
                        element(key, VALUE),
                        element(key, OUT_TANGENT),
                        element(key + 1, IN_TANGENT),
                        element(key + 1, VALUE),
                    ],
                    t,
                    span,
                ),
            },
        }
    }

    /// A rotation channel's unit rotation at this segment: linear keys by
    /// shortest-path spherical interpolation, a cubic spline's components
    /// by the spline and then normalised, as glTF defines.
    fn rotation(self, interpolation: Interpolation, values: &[Quat]) -> Quat {
        let element = |key: usize, part: usize| match interpolation {
            Interpolation::CubicSpline => Vec4::from(values[key * 3 + part]),
            Interpolation::Step | Interpolation::Linear => Vec4::from(values[key]),
        };
        let rotation = match self {
            Self::Between { key, t, span } => match interpolation {
                Interpolation::Step => element(key, VALUE),
                Interpolation::Linear => Vec4::from(slerp(
                    Quat::from_vec4(element(key, VALUE)),
                    Quat::from_vec4(element(key + 1, VALUE)),
                    t,
                )),
                Interpolation::CubicSpline => hermite(
                    [
                        element(key, VALUE),
                        element(key, OUT_TANGENT),
                        element(key + 1, IN_TANGENT),
                        element(key + 1, VALUE),
                    ],
                    t,
                    span,
                ),
            },
            Self::Hold { key } => element(key, VALUE),
        };
        // A zero quaternion (degenerate keys) has no direction: no rotation.
        Quat::from_vec4(rotation.normalize_or(Vec4::W))
    }
}

/// A cubic-spline keyframe's in-tangent, value and out-tangent.
const IN_TANGENT: usize = 0;
const VALUE: usize = 1;
const OUT_TANGENT: usize = 2;

/// What a channel interpolates: a vector space's sum and scaling.
trait Lerp: Copy + std::ops::Add<Output = Self> + std::ops::Mul<f32, Output = Self> {
    fn lerp_to(self, end: Self, t: f32) -> Self {
        self * (1. - t) + end * t
    }
}

impl Lerp for f32 {}
impl Lerp for Vec3 {}
impl Lerp for Vec4 {}

/// glTF's cubic Hermite spline between keyframes `k` and `k + 1`, `span`
/// seconds apart, at the fraction `t`, from `[v_k, b_k, a_k+1, v_k+1]`
/// (value, out-tangent, next in-tangent, next value), the tangents scaled
/// by the span.
fn hermite<T: Lerp>([value, out_tangent, next_in_tangent, next]: [T; 4], t: f32, span: f32) -> T {
    let (t2, t3) = (t * t, t * t * t);
    value * (2. * t3 - 3. * t2 + 1.)
        + out_tangent * (span * (t3 - 2. * t2 + t))
        + next * (-2. * t3 + 3. * t2)
        + next_in_tangent * (span * (t3 - t2))
}

/// Shortest-path spherical interpolation from `from` to `to` by `t`, glTF's
/// for linear rotation keys: `to` is negated when the two lie more than a
/// half-turn apart (glTF's `s = sign(d)`), and where the angle between them
/// vanishes the result is the normalised linear interpolation, which avoids
/// dividing by its sine.
fn slerp(from: Quat, to: Quat, t: f32) -> Quat {
    let (from, mut to) = (Vec4::from(from), Vec4::from(to));
    let mut cos = from.dot(to);
    if cos < 0. {
        to = -to;
        cos = -cos;
    }
    let sin_squared = 1. - cos * cos;
    let (from_scale, to_scale) = if sin_squared > f32::EPSILON {
        let sin = sin_squared.sqrt();
        let angle = cos.acos();
        (((1. - t) * angle).sin() / sin, (t * angle).sin() / sin)
    } else {
        (1. - t, t)
    };
    Quat::from_vec4(from * from_scale + to * to_scale).normalize()
}

#[cfg(test)]
#[path = "animation_tests.rs"]
mod tests;
