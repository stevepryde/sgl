//! Shapes, colliders and their kinds: the plain data a world is built from.

use core::cmp::Ordering;

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
/// is not finite, occupies nothing: no query meets it, through any world.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
}

impl<V: Vector> Collider<V> {
    /// The largest postcard encoding: its shape, position, kind, a `u32`
    /// varint (five bytes) and a `u64` varint (ten). 41 bytes in 3D, 33 in 2D.
    pub const MAX_POSTCARD_BYTES: usize = Shape::<V>::MAX_POSTCARD_BYTES
        + space::postcard_bytes::<V>()
        + ColliderKind::MAX_POSTCARD_BYTES
        + 5
        + 10;

    /// A solid collider on every layer.
    #[must_use]
    pub const fn new(id: u64, shape: Shape<V>, position: V) -> Self {
        Self {
            id,
            shape,
            position,
            kind: ColliderKind::Solid,
            layers: ALL_LAYERS,
        }
    }

    /// The same collider of `kind`.
    #[must_use]
    pub const fn with_kind(mut self, kind: ColliderKind) -> Self {
        self.kind = kind;
        self
    }

    /// Whether the collider occupies any space: a finite position and
    /// finite, non-negative half-extents. Every query skips one that does
    /// not, so every world agrees on it.
    pub(crate) fn occupies(&self) -> bool {
        let half = self.shape.half_extents();
        space::is_finite(self.position)
            && space::is_finite(half)
            && (0..V::AXES).all(|axis| half[axis] >= 0.0)
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
    /// component, then shape, kind and layers. A result that lists
    /// colliders lists them in this order, so it never shows the order a
    /// world visited them in.
    pub(crate) fn order(&self, other: &Self) -> Ordering {
        self.id
            .cmp(&other.id)
            .then_with(|| space::compare(self.position, other.position))
            .then_with(|| self.shape.order(&other.shape))
            .then_with(|| self.kind.tag().cmp(&other.kind.tag()))
            .then_with(|| self.layers.cmp(&other.layers))
    }
}

impl<V: Vector> CanonicalWrite for Collider<V> {
    /// `id` (`u64`), shape, position bits in axis order, kind, `layers`
    /// (`u32`).
    fn canonical_write(&self, out: &mut StateHasher) {
        out.u64(self.id);
        self.shape.canonical_write(out);
        space::write(out, self.position);
        self.kind.canonical_write(out);
        out.u32(self.layers);
    }
}
