//! Shapes, colliders and their kinds: the plain data a world is built from.

use core::cmp::Ordering;
use core::fmt;

use sgl_core::math::{Quat, Vec2, Vec3};
use sgl_core::{CanonicalWrite, StateHasher};

use crate::convex::{d2, d3};
use crate::space::{self, Vector};

/// Every collision layer: a collider on it, or a query masking it, meets
/// everything.
pub const ALL_LAYERS: u32 = u32::MAX;

/// A body's or collider's shape, centred on its position.
///
/// A box against a box, whole or cut ([`Cut`]), is met in closed form and
/// exactly. Every other pair, which a ball, a capsule or a convex shape
/// makes, is met through a convex kernel ported from `Box2D` and `Box3D`, to
/// within a small contact tolerance that scales with the two shapes
/// (`specs/physics.md`).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(bound(
        serialize = "V: Vector + serde::Serialize",
        deserialize = "V: Vector + serde::Deserialize<'de>"
    ))
)]
pub enum Shape<V> {
    /// An axis-aligned box with per-axis half-extents `half`, in the game's
    /// units. Zero along an axis is legal: a point, a line or a plane. A box
    /// with a negative half-extent is inverted: it occupies nothing.
    Box {
        /// Half of the box's size along each axis.
        half: V,
    },
    /// A ball: a circle in 2D, a sphere in 3D, of `radius` about its
    /// centre. A radius of zero is a point; a negative or non-finite one
    /// occupies nothing.
    Ball {
        /// The ball's radius, in the game's units.
        radius: f32,
    },
    /// A capsule: the segment from `-half_axis` to `half_axis` about its
    /// centre, grown by `radius`. An upright character in a y-up world has
    /// `half_axis` along y; its height is `2 * (half_axis.y + radius)`. A
    /// negative or non-finite radius, or a non-finite axis, occupies
    /// nothing.
    Capsule {
        /// Half of the segment, from the centre to one end.
        half_axis: V,
        /// How far the capsule reaches from its segment.
        radius: f32,
    },
    /// A convex polygon in 2D (up to 8 vertices) or a convex hull in 3D (up
    /// to 16 points), turned as the game chose when it built it: a rotated
    /// box, a wall at an angle, a ramp of any slope, a simple convex prop.
    Convex(Convex<V>),
}

impl<V: Vector> Shape<V> {
    /// The largest postcard encoding of a shape: its variant and the
    /// largest variant's fields, a convex shape of its most points (194
    /// bytes in 3D, 66 in 2D).
    pub const MAX_POSTCARD_BYTES: usize = 1 + Convex::<V>::MAX_POSTCARD_BYTES;

    /// Half-extents of the axis-aligned box, centred on the shape's
    /// position, that encloses the shape. Worlds find colliders by it.
    #[must_use]
    pub fn half_extents(&self) -> V {
        match *self {
            Self::Box { half } => half,
            Self::Ball { radius } => space::splat(radius),
            Self::Capsule { half_axis, radius } => {
                let mut half = V::ZERO;
                for axis in 0..V::AXES {
                    half[axis] = half_axis[axis].abs() + radius;
                }
                half
            }
            Self::Convex(convex) => convex.half_extents(),
        }
    }

    /// How far the shape reaches beyond its core, the hull of its points:
    /// a ball's or a capsule's radius, and zero for a box or a convex
    /// shape.
    pub(crate) fn radius(&self) -> f32 {
        match *self {
            Self::Ball { radius } | Self::Capsule { radius, .. } => radius,
            Self::Box { .. } | Self::Convex(_) => 0.0,
        }
    }

    /// Whether the shape's own numbers describe something: finite, with no
    /// negative half-extent or radius.
    pub(crate) fn is_sound(&self) -> bool {
        match *self {
            Self::Box { half } => {
                space::is_finite(half) && (0..V::AXES).all(|axis| half[axis] >= 0.0)
            }
            Self::Ball { radius } => radius.is_finite() && radius >= 0.0,
            Self::Capsule { half_axis, radius } => {
                space::is_finite(half_axis) && radius.is_finite() && radius >= 0.0
            }
            Self::Convex(_) => true,
        }
    }

    /// The same shape at `scale` times its size.
    pub(crate) fn scaled(self, scale: f32) -> Self {
        match self {
            Self::Box { half } => Self::Box { half: half * scale },
            Self::Ball { radius } => Self::Ball {
                radius: radius * scale,
            },
            Self::Capsule { half_axis, radius } => Self::Capsule {
                half_axis: half_axis * scale,
                radius: radius * scale,
            },
            Self::Convex(convex) => Self::Convex(convex.scaled(scale)),
        }
    }

    /// The variant's index, as postcard and the canonical encoding write it.
    fn tag(&self) -> u8 {
        match self {
            Self::Box { .. } => 0,
            Self::Ball { .. } => 1,
            Self::Capsule { .. } => 2,
            Self::Convex(_) => 3,
        }
    }

    /// A fixed total order over shapes, for breaking ties: the variant,
    /// then its fields.
    pub(crate) fn order(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Box { half: a }, Self::Box { half: b }) => space::compare(*a, *b),
            (Self::Ball { radius: a }, Self::Ball { radius: b }) => a.total_cmp(b),
            (
                Self::Capsule {
                    half_axis: a,
                    radius: ra,
                },
                Self::Capsule {
                    half_axis: b,
                    radius: rb,
                },
            ) => space::compare(*a, *b).then_with(|| ra.total_cmp(rb)),
            (Self::Convex(a), Self::Convex(b)) => a.order(b),
            _ => self.tag().cmp(&other.tag()),
        }
    }
}

impl<V: Vector> CanonicalWrite for Shape<V> {
    /// A `u8` variant tag, then the variant's fields: `0`, a box's
    /// half-extents; `1`, a ball's radius; `2`, a capsule's half axis and
    /// radius; `3`, a convex shape (its point count and points). Vectors
    /// write their components' bits in axis order, floats their bits.
    fn canonical_write(&self, out: &mut StateHasher) {
        out.u8(self.tag());
        match *self {
            Self::Box { half } => space::write(out, half),
            Self::Ball { radius } => out.u32(radius.to_bits()),
            Self::Capsule { half_axis, radius } => {
                space::write(out, half_axis);
                out.u32(radius.to_bits());
            }
            Self::Convex(convex) => convex.canonical_write(out),
        }
    }
}

/// Room for the points of a convex shape in either dimension: the 3D
/// kernel's proxy capacity, which is the larger.
const CONVEX_STORAGE: usize = d3::MAX_POINTS;
const _: () = assert!(d2::MAX_POINTS <= CONVEX_STORAGE);

/// A convex shape's points about its centre: the vertices of a convex
/// polygon in 2D, or points whose convex hull is the shape in 3D, already
/// turned by the rotation the game built it with.
///
/// Build it with [`Convex::polygon`] in 2D, from vertices and the cosine and
/// sine of a rotation, or [`Convex::hull`] in 3D, from points and a unit
/// quaternion. The crate calls
/// no trigonometric function, so the game supplies the rotation. The
/// points are stored inline, so a shape stays `Copy` with no handle. A
/// convex shape serialises as its turned points and is validated again,
/// to the same bits, when deserialised.
///
/// ```
/// use sgl_core::math::Vec2;
/// use sgl_physics::{Collider, Convex, Shape};
///
/// // A 4 x 1 wall turned an eighth of a turn counter-clockwise.
/// let half = std::f32::consts::FRAC_1_SQRT_2;
/// let corners = [
///     Vec2::new(-2.0, -0.5),
///     Vec2::new(2.0, -0.5),
///     Vec2::new(2.0, 0.5),
///     Vec2::new(-2.0, 0.5),
/// ];
/// let wall = Collider::new(3, Shape::Convex(Convex::polygon(&corners, half, half)), Vec2::ZERO);
/// assert_eq!(wall.shape.half_extents().x, wall.shape.half_extents().y);
/// ```
#[derive(Clone, Copy, PartialEq)]
pub struct Convex<V> {
    /// The points in use, then zeros.
    points: [V; CONVEX_STORAGE],
    count: usize,
}

/// Why a convex shape was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvalidConvex {
    Count,
    NotFinite,
    Rotation,
    NotConvex,
}

impl fmt::Display for InvalidConvex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Count => "Convex: a polygon needs 3 to 8 vertices, a hull 1 to 16 points",
            Self::NotFinite => "Convex: every point must be finite",
            Self::Rotation => "Convex: the rotation must be finite and non-zero",
            Self::NotConvex => {
                "Convex: a polygon's vertices must be strictly convex and counter-clockwise"
            }
        })
    }
}

impl<V: fmt::Debug> fmt::Debug for Convex<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Convex")
            .field(&&self.points[..self.count])
            .finish()
    }
}

impl<V: Vector> Convex<V> {
    /// The most points a convex shape holds: 8 vertices in 2D
    /// (`B2_MAX_POLYGON_VERTICES`), 16 points in 3D.
    pub const MAX_POINTS: usize = if V::AXES == 2 {
        d2::MAX_POINTS
    } else {
        d3::MAX_POINTS
    };

    /// The largest postcard encoding: a one-byte length and its most points
    /// (193 bytes in 3D, 65 in 2D).
    pub const MAX_POSTCARD_BYTES: usize = 1 + Self::MAX_POINTS * space::postcard_bytes::<V>();

    /// The points, turned, about the shape's centre.
    #[must_use]
    pub fn points(&self) -> &[V] {
        &self.points[..self.count]
    }

    /// The turned points, checked: finite, as many as the dimension holds
    /// and, in 2D, the vertices of a strictly convex polygon in
    /// counter-clockwise order.
    fn validated(points: &[V]) -> Result<Self, InvalidConvex> {
        let least = if V::AXES == 2 { 3 } else { 1 };
        if points.len() < least || points.len() > Self::MAX_POINTS {
            return Err(InvalidConvex::Count);
        }
        if !points.iter().all(|&point| space::is_finite(point)) {
            return Err(InvalidConvex::NotFinite);
        }
        if V::AXES == 2 && !strictly_convex(points) {
            return Err(InvalidConvex::NotConvex);
        }
        let mut out = Self {
            points: [V::ZERO; CONVEX_STORAGE],
            count: points.len(),
        };
        out.points[..points.len()].copy_from_slice(points);
        Ok(out)
    }

    /// Half-extents of the box, centred on the shape's position, that
    /// encloses its points.
    fn half_extents(&self) -> V {
        let mut half = V::ZERO;
        for point in self.points() {
            for axis in 0..V::AXES {
                half[axis] = space::max(half[axis], point[axis].abs());
            }
        }
        half
    }

    /// The same shape at `scale` times its size. A positive scale keeps a
    /// convex shape convex, so it is not checked again.
    fn scaled(self, scale: f32) -> Self {
        let mut out = self;
        for point in &mut out.points[..self.count] {
            *point = *point * scale;
        }
        out
    }

    /// A fixed total order: the point count, then the points in order.
    fn order(&self, other: &Self) -> Ordering {
        self.count.cmp(&other.count).then_with(|| {
            self.points()
                .iter()
                .zip(other.points())
                .map(|(a, b)| space::compare(*a, *b))
                .find(|order| order.is_ne())
                .unwrap_or(Ordering::Equal)
        })
    }
}

/// Whether the 2D points are the vertices of a strictly convex polygon in
/// counter-clockwise order: every other vertex lies strictly to the left
/// of each edge, so the polygon turns left at every vertex and winds once.
fn strictly_convex<V: Vector>(points: &[V]) -> bool {
    let n = points.len();
    (0..n).all(|i| {
        let (a, b) = (points[i], points[(i + 1) % n]);
        let edge = b - a;
        (0..n).filter(|&j| j != i && j != (i + 1) % n).all(|j| {
            let to = points[j] - a;
            edge[0] * to[1] - edge[1] * to[0] > 0.0
        })
    })
}

impl Convex<Vec2> {
    /// The convex polygon with vertices `points`, about its centre, turned
    /// counter-clockwise by the rotation whose cosine and sine are `cos`
    /// and `sin` (normalised first, so `(1, 0)` leaves it as given). The
    /// vertices go counter-clockwise, as seen with x to the right and y up.
    ///
    /// # Panics
    /// If there are fewer than 3 or more than 8 vertices, any is not
    /// finite, the rotation is zero or not finite, or the turned vertices
    /// are not strictly convex and counter-clockwise. All are authoring
    /// errors.
    #[must_use]
    pub fn polygon(points: &[Vec2], cos: f32, sin: f32) -> Self {
        Self::turned(points, cos, sin).unwrap_or_else(|invalid| panic!("{invalid}"))
    }

    fn turned(points: &[Vec2], cos: f32, sin: f32) -> Result<Self, InvalidConvex> {
        if points.len() > Self::MAX_POINTS || points.is_empty() {
            return Err(InvalidConvex::Count);
        }
        let mut turn = [cos, sin];
        if space::normalise(&mut turn).is_none() {
            return Err(InvalidConvex::Rotation);
        }
        let rotation = d2::Rot {
            c: turn[0],
            s: turn[1],
        };
        let turned = d2::Proxy::offset(points, 0.0, Vec2::ZERO, rotation);
        Self::validated(turned.points())
    }
}

impl Convex<Vec3> {
    /// The convex hull of `points`, about its centre, turned by the unit
    /// quaternion `rotation` (normalised first, so `Quat::IDENTITY` leaves
    /// it as given).
    ///
    /// # Panics
    /// If there are no points or more than 16, any is not finite, or the
    /// rotation is zero or not finite. All are authoring errors.
    #[must_use]
    pub fn hull(points: &[Vec3], rotation: Quat) -> Self {
        Self::turned(points, rotation).unwrap_or_else(|invalid| panic!("{invalid}"))
    }

    fn turned(points: &[Vec3], rotation: Quat) -> Result<Self, InvalidConvex> {
        if points.len() > Self::MAX_POINTS || points.is_empty() {
            return Err(InvalidConvex::Count);
        }
        let mut q = [rotation.x, rotation.y, rotation.z, rotation.w];
        if space::normalise(&mut q).is_none() {
            return Err(InvalidConvex::Rotation);
        }
        let rotation = d3::Rot::from_quat(q[0], q[1], q[2], q[3]);
        let turned = d3::Proxy::offset(points, 0.0, Vec3::ZERO, &rotation);
        Self::validated(turned.points())
    }

    /// The flat hull of one triangle's corners about its centre, unturned:
    /// how a [`TriangleSet`](crate::TriangleSet) or a
    /// [`HeightField`](crate::HeightField) offers a triangle. Its callers
    /// built the corners from finite positions and kept only finite ones,
    /// and three is within the count a hull holds, so the public
    /// constructor's checks are not repeated.
    pub(crate) fn triangle(corners: [Vec3; 3]) -> Self {
        debug_assert!(corners.iter().all(|&point| space::is_finite(point)));
        let mut out = Self {
            points: [Vec3::ZERO; CONVEX_STORAGE],
            count: 3,
        };
        out.points[..3].copy_from_slice(&corners);
        out
    }
}

impl<V: Vector> CanonicalWrite for Convex<V> {
    /// The point count as a `u8`, then each point's bits in axis order.
    fn canonical_write(&self, out: &mut StateHasher) {
        // At most 16 points: the count fits a byte.
        out.u8(u8::try_from(self.count).unwrap_or(u8::MAX));
        for &point in self.points() {
            space::write(out, point);
        }
    }
}

#[cfg(feature = "serde")]
impl<V: Vector + serde::Serialize> serde::Serialize for Convex<V> {
    /// The turned points, as a sequence.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.points())
    }
}

#[cfg(feature = "serde")]
impl<'de, V: Vector + serde::Deserialize<'de>> serde::Deserialize<'de> for Convex<V> {
    /// A sequence of turned points, validated as [`Convex::polygon`] and
    /// [`Convex::hull`] validate their turned points. Reading stops at the most a shape holds, so a
    /// long sequence from the wire is refused without being stored.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Points<V>(core::marker::PhantomData<V>);

        impl<'de, V: Vector + serde::Deserialize<'de>> serde::de::Visitor<'de> for Points<V> {
            type Value = Convex<V>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "at most {} points", Convex::<V>::MAX_POINTS)
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut points = [V::ZERO; CONVEX_STORAGE];
                let mut count = 0;
                while let Some(point) = seq.next_element::<V>()? {
                    if count == Convex::<V>::MAX_POINTS {
                        return Err(serde::de::Error::custom(InvalidConvex::Count));
                    }
                    points[count] = point;
                    count += 1;
                }
                Convex::validated(&points[..count]).map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_seq(Points(core::marker::PhantomData))
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
    /// If `normal` is zero or not finite (any other direction, however short
    /// or long, is normalised exactly enough to be unit), if in 3D it has no zero component, or if
    /// `offset` is not finite. All are authoring errors.
    #[must_use]
    pub fn new(normal: V, offset: f32) -> Self {
        Self::validated(normal, offset).unwrap_or_else(|invalid| panic!("{invalid}"))
    }

    fn validated(given: V, offset: f32) -> Result<Self, InvalidCut> {
        // A ramp's only square root: the sweep and the slide read the unit
        // normal this computes once.
        let Some((normal, _)) = space::direction(given) else {
            return Err(InvalidCut::Normal);
        };
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

    /// The same plane moved `by` into the solid part.
    pub(crate) fn moved_in(self, by: f32) -> Self {
        Self {
            offset: self.offset - by,
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
/// layers it is on, the game's own id for it, and how it moves.
///
/// SGL hands out no handles. `id` is the game's (an index and a generation,
/// a block kind, a tile's coordinates): SGL carries it into what a query
/// reports and uses it to break ties, nothing else. Within one
/// [`ColliderSet`](crate::ColliderSet) an id is unique: inserting an id
/// already present replaces that collider. In a slice, a
/// [`CellWorld`](crate::CellWorld) or a pair of worlds, ids may repeat.
///
/// A collider with a negative half-extent or radius, or a position, size or
/// axis that is not finite, occupies nothing: no query meets it, through any
/// world. So does one whose cut leaves nothing of its box, a cut on a shape
/// that is not a box, and a one-way platform that is not a whole box, which
/// [`Self::with_cut`] and [`Self::with_kind`] refuse to build.
///
/// A moving collider (a lift, a door, another body) carries its `velocity`,
/// which [`move_and_slide`](crate::move_and_slide) reads: a body standing on
/// it rides it, and one it has moved into is pushed out the way it moved.
/// Its `push` says whether it blocks a body ([`Push::Rigid`]) or only nudges
/// it ([`Push::Soft`]). Every other query takes the collider as it stands.
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
    /// whole box. A world offers a cut collider by its whole box. Only a box
    /// takes a cut.
    pub cut: Option<Cut<V>>,
    /// How fast it moves, in world units per second, like
    /// [`MoverState::velocity`](crate::MoverState::velocity): what it moved
    /// by this step, over the step's `dt`. Zero for a still collider. The
    /// game moves it (its `position`) and sets this before stepping the
    /// bodies near it. A velocity that is not finite moves nothing.
    pub velocity: V,
    /// Whether a solid collider blocks a body or nudges it; read only for a
    /// [`ColliderKind::Solid`] collider.
    pub push: Push,
}

impl<V: Vector> Collider<V> {
    /// The largest postcard encoding: its shape, position, kind, a `u32`
    /// varint (five bytes), a `u64` varint (ten), an optional cut, its
    /// velocity and its push. 256 bytes in 3D, 116 in 2D.
    pub const MAX_POSTCARD_BYTES: usize = Shape::<V>::MAX_POSTCARD_BYTES
        + space::postcard_bytes::<V>()
        + ColliderKind::MAX_POSTCARD_BYTES
        + 5
        + 10
        + 1
        + Cut::<V>::MAX_POSTCARD_BYTES
        + space::postcard_bytes::<V>()
        + Push::MAX_POSTCARD_BYTES;

    /// A still, rigid, solid collider on every layer.
    #[must_use]
    pub const fn new(id: u64, shape: Shape<V>, position: V) -> Self {
        Self {
            id,
            shape,
            position,
            kind: ColliderKind::Solid,
            layers: ALL_LAYERS,
            cut: None,
            velocity: V::ZERO,
            push: Push::Rigid,
        }
    }

    /// The same collider moving at `velocity`, in world units per second.
    #[must_use]
    pub const fn with_velocity(mut self, velocity: V) -> Self {
        self.velocity = velocity;
        self
    }

    /// The same collider with `push`.
    ///
    /// # Panics
    /// If `push` is [`Push::Soft`] with a limit that is negative or not
    /// finite: an authoring error.
    #[must_use]
    pub const fn with_push(mut self, push: Push) -> Self {
        if let Push::Soft(limit) = push {
            assert!(
                limit >= 0.0 && limit.is_finite(),
                "Collider: a soft push's limit must be finite and non-negative"
            );
        }
        self.push = push;
        self
    }

    /// How far it moves in `dt`: `velocity * dt`, or zero when that is not
    /// finite.
    pub(crate) fn motion(&self, dt: f32) -> V {
        let motion = self.velocity * dt;
        if space::is_finite(motion) {
            motion
        } else {
            V::ZERO
        }
    }

    /// The limit of a solid collider that nudges rather than blocks, or
    /// `None` for every other: a rigid solid, a one-way platform, a sensor.
    pub(crate) fn soft(&self) -> Option<f32> {
        match (self.kind, self.push) {
            (ColliderKind::Solid, Push::Soft(limit)) => Some(limit),
            _ => None,
        }
    }

    /// Whether a solid collider blocks a body: solid and rigid.
    pub(crate) fn rigid_solid(&self) -> bool {
        self.kind == ColliderKind::Solid && self.push == Push::Rigid
    }

    /// The same collider of `kind`.
    ///
    /// # Panics
    /// If `kind` is [`ColliderKind::OneWay`] and the collider has a cut or is
    /// not a box: a one-way platform is a whole box.
    #[must_use]
    pub const fn with_kind(mut self, kind: ColliderKind) -> Self {
        assert!(
            !(matches!(kind, ColliderKind::OneWay)
                && (self.cut.is_some() || !matches!(self.shape, Shape::Box { .. }))),
            "Collider: a one-way platform is a whole box"
        );
        self.kind = kind;
        self
    }

    /// The same collider with its box cut by `cut`: a ramp.
    ///
    /// # Panics
    /// If the collider is a one-way platform, which is a whole box, or its
    /// shape is not a box.
    #[must_use]
    pub const fn with_cut(mut self, cut: Cut<V>) -> Self {
        assert!(
            !matches!(self.kind, ColliderKind::OneWay),
            "Collider: a one-way platform is a whole box"
        );
        assert!(
            matches!(self.shape, Shape::Box { .. }),
            "Collider: only a box takes a cut"
        );
        self.cut = Some(cut);
        self
    }

    /// Whether the collider occupies any space: a finite position and a
    /// sound shape (finite, with no negative half-extent or radius), a
    /// one-way platform that is a box, and, when it is cut, a box whose cut
    /// leaves something of it on a collider that is not one-way. Every query
    /// skips one that does not, so every world agrees on it.
    pub(crate) fn occupies(&self) -> bool {
        let half = self.shape.half_extents();
        let sound = space::is_finite(self.position)
            && space::is_finite(half)
            && (0..V::AXES).all(|axis| half[axis] >= 0.0);
        let boxed = matches!(self.shape, Shape::Box { .. });
        let sound =
            sound && (boxed || (self.shape.is_sound() && self.kind != ColliderKind::OneWay));
        match self.cut {
            None => sound,
            Some(cut) => {
                sound
                    && boxed
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
    /// component, then shape, kind, layers, cut (none first), velocity and
    /// push. A result that lists colliders lists them in this order, so it
    /// never shows the order a world visited them in.
    pub(crate) fn order(&self, other: &Self) -> Ordering {
        self.id
            .cmp(&other.id)
            .then_with(|| space::compare(self.position, other.position))
            .then_with(|| self.shape.order(&other.shape))
            .then_with(|| self.kind.tag().cmp(&other.kind.tag()))
            .then_with(|| self.layers.cmp(&other.layers))
            .then_with(|| order_cuts(self.cut.as_ref(), other.cut.as_ref()))
            .then_with(|| space::compare(self.velocity, other.velocity))
            .then_with(|| self.push.order(other.push))
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
    /// (`u32`), the cut as a bool tag followed, when present, by the cut,
    /// then the velocity's bits in axis order and the push.
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
        space::write(out, self.velocity);
        self.push.canonical_write(out);
    }
}

/// How a solid collider meets a body the mover steps: it blocks it, or it
/// nudges it aside without stopping it. Only
/// [`move_and_slide`](crate::move_and_slide) and the moving queries it is
/// built on ([`slide`](crate::slide), [`snap_to_ground`](crate::snap_to_ground),
/// [`move_and_collide`](crate::move_and_collide), [`find_ledge`](crate::find_ledge))
/// read it; [`cast`](crate::cast), [`ray`](crate::ray),
/// [`overlaps`](crate::overlaps) and [`distance`](crate::distance) meet a
/// soft collider as any solid.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Push {
    /// Blocks a body from every side, and a body inside it is pushed out:
    /// a wall, a lift, a door, a boat.
    #[default]
    Rigid,
    /// Never blocks: a body moves into it, and after its move is nudged away
    /// from it by at most this many world units a step. Two bodies that walk
    /// into each other harder than that pass through each other: a crowd,
    /// or players who may push past one another.
    Soft(f32),
}

impl Push {
    /// The largest postcard encoding: the variant's index (one byte) and a
    /// soft push's `f32`.
    pub const MAX_POSTCARD_BYTES: usize = 5;

    /// A fixed total order: rigid first, then soft by limit.
    fn order(self, other: Self) -> Ordering {
        match (self, other) {
            (Self::Rigid, Self::Rigid) => Ordering::Equal,
            (Self::Rigid, Self::Soft(_)) => Ordering::Less,
            (Self::Soft(_), Self::Rigid) => Ordering::Greater,
            (Self::Soft(a), Self::Soft(b)) => a.total_cmp(&b),
        }
    }
}

impl CanonicalWrite for Push {
    /// A `u8` variant tag, `0` rigid or `1` soft, then a soft push's limit
    /// bits.
    fn canonical_write(&self, out: &mut StateHasher) {
        match *self {
            Self::Rigid => out.u8(0),
            Self::Soft(limit) => {
                out.u8(1);
                out.u32(limit.to_bits());
            }
        }
    }
}
