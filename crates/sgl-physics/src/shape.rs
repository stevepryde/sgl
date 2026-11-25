//! Shapes, colliders and their kinds: the plain data a world is built from.

use core::cmp::Ordering;
use core::fmt;

use sgl_core::{CanonicalWrite, StateHasher};

use crate::space::{self, Vector};

/// Every collision layer: a collider on it, or a query masking it, meets
/// everything.
pub const ALL_LAYERS: u32 = u32::MAX;

/// A body's or collider's shape, centred on its position.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Shape<V> {
    /// An axis-aligned box with per-axis half-extents `half`, in the game's
    /// units. Zero along an axis is legal: a point, a line or a plane. A box
    /// with a negative half-extent is inverted: it occupies nothing.
    Box {
        /// Half of the box's size along each axis.
        half: V,
    },
}

impl<V: Vector> Shape<V> {
    /// The largest postcard encoding of a shape: its variant and its
    /// half-extents (13 bytes in 3D, 9 in 2D).
    pub const MAX_POSTCARD_BYTES: usize = 1 + space::postcard_bytes::<V>();

    /// Half-extents of the axis-aligned box that encloses the shape.
    #[must_use]
    pub fn half_extents(&self) -> V {
        match *self {
            Self::Box { half } => half,
        }
    }

    /// The same shape at `scale` times its size.
    pub(crate) fn scaled(self, scale: f32) -> Self {
        match self {
            Self::Box { half } => Self::Box { half: half * scale },
        }
    }

    /// A fixed total order over shapes, for breaking ties.
    pub(crate) fn order(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Box { half: a }, Self::Box { half: b }) => space::compare(*a, *b),
        }
    }
}

impl<V: Vector> CanonicalWrite for Shape<V> {
    /// A `u8` variant tag (`0` for a box), then the variant's fields: a box
    /// writes its half-extents' bits in axis order.
    fn canonical_write(&self, out: &mut StateHasher) {
        match *self {
            Self::Box { half } => {
                out.u8(0);
                space::write(out, half);
            }
        }
    }
}

/// A plane through a collider's box that planes part of it away: a ramp, a
/// slope tile or a slope block. What is left is the box where
/// `normal · (p - position) <= offset`, so the plane's face points out of
/// the solid along `normal`, and a contact on it reports `normal`.
///
/// Build it with [`Self::new`]; its fields are private, so nothing bypasses
/// the validation. In 2D the normal may point any way. In 3D it has at least
/// one zero component: the ramp turns about one cardinal axis, at any angle
/// (upright, inverted and sideways ramps), which keeps the sweep against it
/// closed-form and exact. A cut serialises as the normal and offset it was
/// built from, and deserialising validates and normalises them again, to the
/// same bits.
///
/// ```
/// use sgl_core::math::Vec2;
/// use sgl_physics::{Collider, Cut, Shape};
///
/// // A one-unit tile rising to the right: the plane through its bottom-left
/// // and top-right corners, solid below it.
/// let ramp = Collider::new(1, Shape::Box { half: Vec2::splat(0.5) }, Vec2::new(0.5, 0.5))
///     .with_cut(Cut::new(Vec2::new(-1.0, 1.0), 0.0));
/// assert_eq!(ramp.cut.map(|cut| cut.offset()), Some(0.0));
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cut<V> {
    /// The normal as the game gave it, which is what serialises.
    given: V,
    /// `given`, normalised.
    normal: V,
    offset: f32,
}

/// Why a cut was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvalidCut {
    Normal,
    Turn,
    Offset,
}

impl fmt::Display for InvalidCut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Normal => "Cut: normal must be a finite non-zero direction",
            Self::Turn => "Cut: a 3D normal must have a zero component",
            Self::Offset => "Cut: offset must be finite",
        })
    }
}

impl<V: Vector> Cut<V> {
    /// The largest postcard encoding: the normal and an `f32` (16 bytes in
    /// 3D, 12 in 2D).
    pub const MAX_POSTCARD_BYTES: usize = space::postcard_bytes::<V>() + 4;

    /// The plane with outward `normal` (any length) at `offset` along it
    /// from the box's centre, in the game's units. An offset at or beyond
    /// the box's reach along `normal` cuts nothing away; one below the box's
    /// reach against it leaves nothing, and the collider occupies nothing.
    ///
    /// # Panics
    /// If `normal` is not a usable direction (zero, not finite, or so long
    /// its length overflows), if in 3D it has no zero component, or if
    /// `offset` is not finite. All are authoring errors.
    #[must_use]
    pub fn new(normal: V, offset: f32) -> Self {
        Self::validated(normal, offset).unwrap_or_else(|invalid| panic!("{invalid}"))
    }

    fn validated(given: V, offset: f32) -> Result<Self, InvalidCut> {
        // A ramp's only square root: the sweep and the slide read the unit
        // normal this computes once.
        let length = space::dot(given, given).sqrt();
        if !(length.is_finite() && length > 0.0) {
            return Err(InvalidCut::Normal);
        }
        let mut normal = given;
        for axis in 0..V::AXES {
            normal[axis] = given[axis] / length;
        }
        if V::AXES == 3 && (0..V::AXES).all(|axis| normal[axis] != 0.0) {
            return Err(InvalidCut::Turn);
        }
        if !offset.is_finite() {
            return Err(InvalidCut::Offset);
        }
        Ok(Self {
            given,
            normal,
            offset,
        })
    }

    /// The plane's unit normal, pointing out of the solid part.
    #[must_use]
    pub fn normal(&self) -> V {
        self.normal
    }

    /// How far the plane lies from the box's centre along [`Self::normal`],
    /// in the game's units.
    #[must_use]
    pub fn offset(&self) -> f32 {
        self.offset
    }

    /// The same plane through a box at `scale` times its size.
    pub(crate) fn scaled(self, scale: f32) -> Self {
        Self {
            offset: self.offset * scale,
            ..self
        }
    }

    /// A fixed total order over cuts, for breaking ties.
    fn order(&self, other: &Self) -> Ordering {
        space::compare(self.given, other.given).then_with(|| self.offset.total_cmp(&other.offset))
    }
}

impl<V: Vector> CanonicalWrite for Cut<V> {
    /// What it was built from: the normal's bits in axis order as given,
    /// then the offset's bits.
    fn canonical_write(&self, out: &mut StateHasher) {
        space::write(out, self.given);
        out.u32(self.offset.to_bits());
    }
}

/// A cut as it travels: the normal it was built from and its offset.
#[cfg(feature = "serde")]
#[derive(serde::Serialize, serde::Deserialize)]
struct CutData<V> {
    normal: V,
    offset: f32,
}

#[cfg(feature = "serde")]
impl<V: Vector + serde::Serialize> serde::Serialize for Cut<V> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        CutData {
            normal: self.given,
            offset: self.offset,
        }
        .serialize(serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de, V: Vector + serde::Deserialize<'de>> serde::Deserialize<'de> for Cut<V> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let data = CutData::deserialize(deserializer)?;
        Self::validated(data.normal, data.offset).map_err(serde::de::Error::custom)
    }
}

/// How a collider behaves when a body meets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ColliderKind {
    /// Blocks motion from every side.
    Solid,
    /// A one-way platform: passable except for motion into a floor face (one
    /// within `normal_threshold` of the world's up axis). A
    /// [`cast`](crate::cast) or [`ray`](crate::ray), which knows no up axis,
    /// meets it whole.
    OneWay,
    /// Never blocks. A game finds what is in its sensors with
    /// [`overlaps`](crate::overlaps).
    Sensor,
}

impl ColliderKind {
    /// The largest postcard encoding: the variant's index, one byte.
    pub const MAX_POSTCARD_BYTES: usize = 1;

    /// Whether the collider can stop a body at all.
    pub(crate) fn blocks(self) -> bool {
        !matches!(self, Self::Sensor)
    }

    /// The variant's index, as postcard writes it.
    fn tag(self) -> u8 {
        match self {
            Self::Solid => 0,
            Self::OneWay => 1,
            Self::Sensor => 2,
        }
    }
}

impl CanonicalWrite for ColliderKind {
    /// The variant's index as a `u8`: `0` solid, `1` one-way, `2` sensor.
    fn canonical_write(&self, out: &mut StateHasher) {
        out.u8(self.tag());
    }
}

/// One collider in a world: a shape at a position, how it behaves, the
/// layers it is on, and the game's own id for it.
///
/// SGL hands out no handles. `id` is the game's (an index and a generation,
/// a block kind, a tile's coordinates): SGL carries it into what a query
/// reports and uses it to break ties, nothing else. Within one
/// [`ColliderSet`](crate::ColliderSet) an id is unique: inserting an id
/// already present replaces that collider. In a slice, a
/// [`CellWorld`](crate::CellWorld) or a pair of worlds, ids may repeat.
///
/// A collider with a negative half-extent, or a position or half-extent that
/// is not finite, occupies nothing: no query meets it, through any world. So
/// does one whose cut leaves nothing of its box, and a one-way platform with
/// a cut, which [`Self::with_cut`] and [`Self::with_kind`] refuse to build.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(bound(
        serialize = "V: Vector + serde::Serialize",
        deserialize = "V: Vector + serde::Deserialize<'de>"
    ))
)]
pub struct Collider<V> {
    /// The game's id for this collider.
    pub id: u64,
    /// Its shape, centred on `position`.
    pub shape: Shape<V>,
    /// Its centre, in world units.
    pub position: V,
    /// How it behaves.
    pub kind: ColliderKind,
    /// The layers it is on. A query meets it when `layers & mask != 0`.
    pub layers: u32,
    /// A plane that cuts part of the box away (a ramp), or `None` for the
    /// whole box. A world offers a cut collider by its whole box.
    pub cut: Option<Cut<V>>,
}

impl<V: Vector> Collider<V> {
    /// The largest postcard encoding: its shape, position, kind, a `u32`
    /// varint (five bytes), a `u64` varint (ten) and an optional cut. 58
    /// bytes in 3D, 46 in 2D.
    pub const MAX_POSTCARD_BYTES: usize = Shape::<V>::MAX_POSTCARD_BYTES
        + space::postcard_bytes::<V>()
        + ColliderKind::MAX_POSTCARD_BYTES
        + 5
        + 10
        + 1
        + Cut::<V>::MAX_POSTCARD_BYTES;

    /// A solid collider on every layer.
    #[must_use]
    pub const fn new(id: u64, shape: Shape<V>, position: V) -> Self {
        Self {
            id,
            shape,
            position,
            kind: ColliderKind::Solid,
            layers: ALL_LAYERS,
            cut: None,
        }
    }

    /// The same collider of `kind`.
    ///
    /// # Panics
    /// If `kind` is [`ColliderKind::OneWay`] and the collider has a cut: a
    /// one-way platform is a whole box.
    #[must_use]
    pub const fn with_kind(mut self, kind: ColliderKind) -> Self {
        assert!(
            !(matches!(kind, ColliderKind::OneWay) && self.cut.is_some()),
            "Collider: a one-way platform has no cut"
        );
        self.kind = kind;
        self
    }

    /// The same collider with its box cut by `cut`: a ramp.
    ///
    /// # Panics
    /// If the collider is a one-way platform, which is a whole box.
    #[must_use]
    pub const fn with_cut(mut self, cut: Cut<V>) -> Self {
        assert!(
            !matches!(self.kind, ColliderKind::OneWay),
            "Collider: a one-way platform has no cut"
        );
        self.cut = Some(cut);
        self
    }

    /// Whether the collider occupies any space: a finite position and
    /// finite, non-negative half-extents, and, when it is cut, a cut that
    /// leaves something of the box on a collider that is not one-way. Every
    /// query skips one that does not, so every world agrees on it.
    pub(crate) fn occupies(&self) -> bool {
        let half = self.shape.half_extents();
        let sound = space::is_finite(self.position)
            && space::is_finite(half)
            && (0..V::AXES).all(|axis| half[axis] >= 0.0);
        match self.cut {
            None => sound,
            Some(cut) => {
                sound
                    && self.kind != ColliderKind::OneWay
                    && cut.offset >= -space::reach(half, cut.normal)
            }
        }
    }

    /// The rounding allowance of the collider's own coordinates along each
    /// axis, `4ε(|position| + half)`: how far a face computed relative to a
    /// body may lie from the same face in world coordinates.
    pub(crate) fn rounding(&self) -> V {
        let half = self.shape.half_extents();
        let mut allowance = V::ZERO;
        for axis in 0..V::AXES {
            allowance[axis] = 4.0 * f32::EPSILON * (self.position[axis].abs() + half[axis]);
        }
        allowance
    }

    /// The same collider on `layers`.
    #[must_use]
    pub const fn with_layers(mut self, layers: u32) -> Self {
        self.layers = layers;
        self
    }

    /// A fixed total order over colliders: id, then position component by
    /// component, then shape, kind, layers and cut (none first). A result
    /// that lists colliders lists them in this order, so it never shows the
    /// order a world visited them in.
    pub(crate) fn order(&self, other: &Self) -> Ordering {
        self.id
            .cmp(&other.id)
            .then_with(|| space::compare(self.position, other.position))
            .then_with(|| self.shape.order(&other.shape))
            .then_with(|| self.kind.tag().cmp(&other.kind.tag()))
            .then_with(|| self.layers.cmp(&other.layers))
            .then_with(|| order_cuts(self.cut.as_ref(), other.cut.as_ref()))
    }
}

/// A fixed total order over optional cuts: none first.
pub(crate) fn order_cuts<V: Vector>(a: Option<&Cut<V>>, b: Option<&Cut<V>>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => a.order(b),
    }
}

impl<V: Vector> CanonicalWrite for Collider<V> {
    /// `id` (`u64`), shape, position bits in axis order, kind, `layers`
    /// (`u32`), then the cut as a bool tag followed, when present, by the
    /// cut.
    fn canonical_write(&self, out: &mut StateHasher) {
        out.u64(self.id);
        self.shape.canonical_write(out);
        space::write(out, self.position);
        self.kind.canonical_write(out);
        out.u32(self.layers);
        out.bool(self.cut.is_some());
        if let Some(cut) = &self.cut {
            cut.canonical_write(out);
        }
    }
}
