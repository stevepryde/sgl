//! Deterministic 3D distance attenuation and constant-power stereo panning.

use core::f32::consts::FRAC_PI_4;
use glam::Vec3;

/// Distance below which a 3D source is treated as colocated with the listener
/// and centered (`pan = 0.0`).
const COLOCATED_EPSILON: f32 = 1e-4;

/// A 3D audio listener pose in right-handed world space (Y-up by default).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Listener3d {
    /// Listener position in world space.
    pub position: Vec3,
    /// Listener forward direction (need not be normalized).
    pub forward: Vec3,
    /// Listener up direction (need not be normalized).
    pub up: Vec3,
}

impl Default for Listener3d {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            forward: Vec3::NEG_Z,
            up: Vec3::Y,
        }
    }
}

impl Listener3d {
    /// Unit vector pointing to the listener's right (`forward.cross(up)`),
    /// falling back to [`Vec3::X`] if `forward` and `up` are degenerate.
    #[must_use]
    pub fn right(&self) -> Vec3 {
        self.forward
            .cross(self.up)
            .try_normalize()
            .unwrap_or(Vec3::X)
    }
}

/// Computed stereo mix for an audio source.
///
/// Constant-power panning preserves total stereo energy:
/// `left_gain * left_gain + right_gain * right_gain == gain * gain` for all
/// pan values in `-1.0..=1.0`, with center pan (`pan == 0.0`) producing
/// `left_gain == right_gain == gain * FRAC_1_SQRT_2`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SpatialMix {
    /// Overall linear gain after volume and distance attenuation.
    pub gain: f32,
    /// Stereo pan in `-1.0..=1.0` (`-1.0` hard left, `0.0` center, `+1.0` hard right).
    pub pan: f32,
    /// Left-channel linear gain.
    pub left_gain: f32,
    /// Right-channel linear gain.
    pub right_gain: f32,
}

impl SpatialMix {
    /// Silence (`gain = 0`, `pan = 0`, `left_gain = 0`, `right_gain = 0`).
    pub const SILENT: Self = Self {
        gain: 0.0,
        pan: 0.0,
        left_gain: 0.0,
        right_gain: 0.0,
    };

    /// Computes a constant-power stereo mix from linear `gain` and `pan` (`-1..=1`).
    ///
    /// Maps `theta = (pan + 1.0) * FRAC_PI_4`, yielding
    /// `left_gain = gain * theta.cos()` and `right_gain = gain * theta.sin()`,
    /// so `left_gain^2 + right_gain^2 == gain^2` across the full pan range.
    #[must_use]
    pub(crate) fn from_gain_pan(gain: f32, pan: f32) -> Self {
        let gain = if gain.is_finite() {
            gain.max(0.0)
        } else {
            0.0
        };
        if gain == 0.0 {
            return Self::SILENT;
        }
        let pan = if pan.is_finite() {
            pan.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let theta = (pan + 1.0) * FRAC_PI_4;
        let left_gain = gain * theta.cos().max(0.0);
        let right_gain = gain * theta.sin().max(0.0);
        Self {
            gain,
            pan,
            left_gain,
            right_gain,
        }
    }
}

/// Computes deterministic 3D distance attenuation and constant-power stereo
/// panning for a source relative to `listener`.
///
/// - **Attenuation**: smooth quadratic rolloff `t = clamp(1 - d / range, 0, 1)`,
///   `atten = t * t`, yielding `1.0` at `d == 0` and exact `0.0` for `d >= range`.
/// - **Non-attenuated 2D**: when `range <= 0.0`, attenuation is bypassed
///   (`gain = volume.max(0.0)`) and the mix is centered (`pan = 0.0`).
/// - **Constant-power stereo panning**: `pan = clamp(dir.dot(listener.right()), -1, 1)`
///   (`0.0` when `d < 1e-4`), with `theta = (pan + 1) * FRAC_PI_4`,
///   `left_gain = gain * cos(theta)`, `right_gain = gain * sin(theta)`,
///   preserving unity stereo energy (`left_gain^2 + right_gain^2 == gain^2`).
#[must_use]
pub fn spatialize(
    listener: &Listener3d,
    source_position: Vec3,
    range: f32,
    volume: f32,
) -> SpatialMix {
    let volume = if volume.is_finite() {
        volume.max(0.0)
    } else {
        0.0
    };
    if volume == 0.0 {
        return SpatialMix::SILENT;
    }
    if !range.is_finite() || range <= 0.0 {
        return SpatialMix::from_gain_pan(volume, 0.0);
    }

    let to_source = source_position - listener.position;
    let distance = to_source.length();
    if !distance.is_finite() {
        return SpatialMix::SILENT;
    }
    if distance >= range {
        return SpatialMix::SILENT;
    }

    let t = (1.0 - distance / range).clamp(0.0, 1.0);
    let atten = t * t;
    let gain = volume * atten;

    let pan = if distance < COLOCATED_EPSILON {
        0.0
    } else {
        (to_source / distance)
            .dot(listener.right())
            .clamp(-1.0, 1.0)
    };

    SpatialMix::from_gain_pan(gain, pan)
}
