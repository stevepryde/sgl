//! The game's mover settings, in its own units and axis convention.

use core::fmt;

use sgl_core::{CanonicalWrite, StateHasher};

use crate::space::{self, Vector};
use crate::sweep::TIE_ORDER;

/// The values a [`MoverConfig`] is built from, named so none can be swapped
/// for another, all in the game's own world units.
///
/// There is deliberately no `Default`: `skin` and `snap_distance` are lengths
/// in the game's units and `up` is its axis convention, so a default would
/// bake one game's world into every other.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoverSettings<V> {
    /// The world's up axis, any length: `Vec2::Y` for a y-up 2D world,
    /// `Vec2::NEG_Y` for a y-down one, `Vec3::Y` for SGL3D's.
    pub up: V,
    /// Gap left between the body and a surface after a contact resolves, so
    /// the next sweep starts clear of the surface instead of touching it.
    /// Small relative to the body.
    pub skin: f32,
    /// How far [`snap_to_ground`](crate::snap_to_ground) probes along `-up`.
    pub snap_distance: f32,
    /// `normal · up` above which a face is a floor, and below whose negation
    /// it is a ceiling, in `(0, 1)`. `0.7` is roughly 45 degrees.
    pub normal_threshold: f32,
    /// Tolerance when comparing requested against achieved motion along `up`
    /// for [`Contacts`](crate::Contacts)' blocked flags. Well under `skin`,
    /// or a resolved contact reads as unblocked.
    pub block_epsilon: f32,
}

/// The game's mover settings, validated, with `up` normalised.
///
/// Construct it with [`Self::new`] from [`MoverSettings`]; the fields are
/// private, so nothing bypasses the validation. A config serialises as the
/// settings it was built from, and deserialising validates and normalises
/// them again, to the same bits.
///
/// ```compile_fail
/// use sgl_core::math::Vec2;
/// use sgl_physics::{MoverConfig, MoverSettings};
///
/// let config = MoverConfig {
///     settings: MoverSettings {
///         up: Vec2::new(0.0, 2.0),
///         skin: -1.0,
///         snap_distance: 0.1,
///         normal_threshold: 0.7,
///         block_epsilon: 0.0,
///     },
///     up: Vec2::new(0.0, 2.0),
/// };
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoverConfig<V> {
    settings: MoverSettings<V>,
    up: V,
}

/// Why a set of mover settings was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Invalid {
    Up,
    Threshold,
    Length(&'static str),
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Up => f.write_str("MoverConfig: up must be a finite non-zero direction"),
            Self::Threshold => f.write_str("MoverConfig: normal threshold must be in (0, 1)"),
            Self::Length(name) => write!(f, "MoverConfig: {name} must be finite and non-negative"),
        }
    }
}

impl<V: Vector> MoverConfig<V> {
    /// The largest postcard encoding: `up` and four `f32`s (28 bytes in 3D,
    /// 24 in 2D).
    pub const MAX_POSTCARD_BYTES: usize = space::postcard_bytes::<V>() + 16;

    /// Validates the settings and normalises `up`.
    ///
    /// # Panics
    /// If `up` is zero or not finite (any other direction, however short or
    /// long, is normalised exactly enough to be unit); if `normal_threshold` is outside `(0, 1)`, where
    /// every face would classify alike and floors, ceilings and walls stop
    /// being distinguishable; or if `skin`, `snap_distance` or
    /// `block_epsilon` is negative or not finite (a negative skin backs a
    /// contact off into the surface). All are authoring errors.
    #[must_use]
    pub fn new(settings: MoverSettings<V>) -> Self {
        Self::validated(settings).unwrap_or_else(|invalid| panic!("{invalid}"))
    }

    fn validated(settings: MoverSettings<V>) -> Result<Self, Invalid> {
        let Some((unit, _)) = space::direction(settings.up) else {
            return Err(Invalid::Up);
        };
        if !(settings.normal_threshold > 0.0 && settings.normal_threshold < 1.0) {
            return Err(Invalid::Threshold);
        }
        for (name, value) in [
            ("skin", settings.skin),
            ("snap distance", settings.snap_distance),
            ("block epsilon", settings.block_epsilon),
        ] {
            if !(value.is_finite() && value >= 0.0) {
                return Err(Invalid::Length(name));
            }
        }
        Ok(Self { settings, up: unit })
    }

    /// Unit vector along the world's up axis: `Vec2::Y` for a y-up 2D world,
    /// `Vec2::NEG_Y` for a y-down one, `Vec3::Y` for SGL3D's. Every floor,
    /// ceiling, one-way and ground-snap decision is made against it.
    #[must_use]
    pub fn up(&self) -> V {
        self.up
    }

    /// Gap left between the body and a surface after a contact resolves.
    #[must_use]
    pub fn skin(&self) -> f32 {
        self.settings.skin
    }

    /// How far [`snap_to_ground`](crate::snap_to_ground) probes along `-up`.
    #[must_use]
    pub fn snap_distance(&self) -> f32 {
        self.settings.snap_distance
    }

    /// `normal · up` above which a face is a floor, and below whose negation
    /// it is a ceiling.
    #[must_use]
    pub fn normal_threshold(&self) -> f32 {
        self.settings.normal_threshold
    }

    /// Tolerance when comparing requested against achieved motion along `up`
    /// for [`Contacts`](crate::Contacts)' blocked flags.
    #[must_use]
    pub fn block_epsilon(&self) -> f32 {
        self.settings.block_epsilon
    }

    /// The same settings probing `distance` along `-up` when snapping: how
    /// the mover asks whether a floor lies within a given depth below a body.
    /// `distance` is finite and non-negative.
    pub(crate) fn with_snap_distance(&self, distance: f32) -> Self {
        let mut probe = *self;
        probe.settings.snap_distance = distance;
        probe
    }

    /// Whether `normal` belongs to a face a body can stand on.
    #[must_use]
    pub fn is_floor(&self, normal: V) -> bool {
        space::dot(normal, self.up) > self.settings.normal_threshold
    }

    /// Whether `normal` belongs to a face a body can bump its head on.
    #[must_use]
    pub fn is_ceiling(&self, normal: V) -> bool {
        space::dot(normal, self.up) < -self.settings.normal_threshold
    }

    /// The order in which an exact tie between axes resolves for the mover:
    /// the axis most along `up` first (on equal lengths, y before x before
    /// z), then the others y, x, z. A body arriving exactly at a ledge's
    /// corner lands on it, whichever axis the world calls up.
    pub(crate) fn tie_order(&self) -> [usize; 3] {
        let first = TIE_ORDER[..V::AXES]
            .iter()
            .copied()
            .fold(TIE_ORDER[0], |best, axis| {
                if self.up[axis].abs() > self.up[best].abs() {
                    axis
                } else {
                    best
                }
            });
        let mut order = [first; 3];
        let mut next = 1;
        for axis in TIE_ORDER {
            if axis != first {
                order[next] = axis;
                next += 1;
            }
        }
        order
    }
}

impl<V: Vector> CanonicalWrite for MoverConfig<V> {
    /// The settings it was built from: `up`'s bits in axis order, then the
    /// bits of `skin`, `snap_distance`, `normal_threshold` and
    /// `block_epsilon`.
    fn canonical_write(&self, out: &mut StateHasher) {
        let settings = &self.settings;
        space::write(out, settings.up);
        for value in [
            settings.skin,
            settings.snap_distance,
            settings.normal_threshold,
            settings.block_epsilon,
        ] {
            out.u32(value.to_bits());
        }
    }
}

#[cfg(feature = "serde")]
impl<V: Vector + serde::Serialize> serde::Serialize for MoverConfig<V> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.settings.serialize(serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de, V: Vector + serde::Deserialize<'de>> serde::Deserialize<'de> for MoverConfig<V> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::validated(MoverSettings::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sgl_core::math::{Vec2, Vec3};
    use wasm_bindgen_test::wasm_bindgen_test;

    const SKIN: f32 = 0.01;
    const SNAP: f32 = 2.0 / 32.0;

    fn settings<V>(up: V, skin: f32, snap: f32, threshold: f32, epsilon: f32) -> MoverSettings<V> {
        MoverSettings {
            up,
            skin,
            snap_distance: snap,
            normal_threshold: threshold,
            block_epsilon: epsilon,
        }
    }

    /// #249: floor and ceiling classification is strict at the configured
    /// threshold.
    #[wasm_bindgen_test(unsupported = test)]
    fn classification_is_strict_at_the_threshold() {
        let config = MoverConfig::new(settings(Vec2::Y, 0.01, 0.1, 0.7, 1e-4));
        assert!(
            !config.is_floor(Vec2::new(0.3, 0.7)),
            "exactly at the threshold"
        );
        assert!(config.is_floor(Vec2::new(0.0, 0.7001)));
        assert!(
            !config.is_ceiling(Vec2::new(0.3, -0.7)),
            "exactly at the threshold"
        );
        assert!(config.is_ceiling(Vec2::new(0.0, -0.7001)));
    }

    /// `up` is normalised: a y-up 3D world given `(0, 2, 0)` classifies a
    /// face whose normal has `0.69` along up as a wall, where an unnormalised
    /// `up` would read `1.38` and call it a floor; `0.71` is a floor.
    #[wasm_bindgen_test(unsupported = test)]
    fn up_is_normalised_before_classifying() {
        let config = MoverConfig::new(settings(Vec3::new(0.0, 2.0, 0.0), SKIN, SNAP, 0.7, 1e-4));
        assert_eq!(config.up(), Vec3::Y);
        assert!(config.is_floor(Vec3::new(0.0, 0.71, 0.0)));
        assert!(!config.is_floor(Vec3::new(0.0, 0.69, 0.0)));
    }

    /// #315: negative or non-finite lengths are authoring errors.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_config_rejects_negative_and_non_finite_lengths() {
        for bad in [-0.01, f32::NAN, f32::INFINITY] {
            for (skin, snap, epsilon) in [(bad, SNAP, 1e-4), (SKIN, bad, 1e-4), (SKIN, SNAP, bad)] {
                let built = std::panic::catch_unwind(|| {
                    MoverConfig::new(settings(Vec2::Y, skin, snap, 0.7, epsilon))
                });
                assert!(built.is_err(), "accepted {skin}, {snap}, {epsilon}");
            }
        }
    }

    /// Defect: a direction whose squared length overflows normalised to
    /// zero (or was refused). Oracle: physics.md 12, any finite non-zero
    /// `up` is a direction, here exactly the x axis.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_config_normalises_an_up_direction_whose_length_overflows() {
        let config = MoverConfig::new(settings(Vec2::new(1e20, 0.0), SKIN, SNAP, 0.7, 1e-4));
        assert_eq!(config.up(), Vec2::X);
    }
}
