//! The convex kernel (#919): the closest points between two convex shapes
//! and the first contact of one moving against the other, for every pair the
//! closed-form box sweep does not cover.
//!
//! Ported from `Box2D` ([`d2`]) and `Box3D` ([`d3`]), MIT, Erin Catto; the tag,
//! commit and file of each are in the submodules' headers and
//! `PROVENANCE.md`, with every departure and the algorithm notes.
//!
//! A shape is a [`Proxy`]: the hull of a few points, grown by a radius. A
//! ball is one point and a radius, a capsule two, a box its corners with no
//! radius, a polygon or a hull its vertices. The hull without the radius is
//! the shape's *core*.
//!
//! Both dimensions offer the same functions under the same names:
//! `distance` and `shape_cast`, over `Proxy`, `Rot`, `MAX_POINTS` and
//! `MAX_GJK_ITERATIONS`; 2D adds `time_of_impact`. They share the result
//! types here and one shape cast, [`cast_by_advancement`].
//!
//! **Frame.** Both proxies of a call are in one frame, and nothing here
//! recentres them: the caller builds them relative to the body, as every
//! other query in the crate does, so accuracy depends on the shapes and the
//! distance between them and not on where they are in the world.
//!
//! **Tolerances are the caller's.** The sources take theirs from
//! `linearSlop`, half a centimetre under one process-wide length scale.
//! This crate works in each game's own units (pixels in one game, metres in
//! another), so every routine that needs that length takes it as `slop`.
//!
//! **Bounded and stateless.** Every loop has a named cap from the sources,
//! and reaching one returns the best answer so far under a state that says
//! so. Nothing is allocated and nothing is kept between calls: the sources'
//! simplex cache, a warm start, is not ported.

// The sources name the two shapes A and B and the simplex vertices w1, w2
// and w3. The port keeps those names so it reads against them.
#![allow(clippy::similar_names, clippy::many_single_char_names)]

use crate::space::Vector;

pub(crate) mod d2;
pub(crate) mod d3;

/// The most times a shape cast advances before it gives up (`maxIterations`
/// in `b2CastShape` and `b3ShapeCast`). Each advance runs one distance
/// query. Balls and flat faces converge in two or three; the cap is reached
/// only when a step no longer makes progress in `f32`.
pub(crate) const MAX_CAST_ITERATIONS: u32 = 20;

/// A search direction shorter than this, squared, cannot be normalised: the
/// sources' `1000 * FLT_MIN`. It guards against underflow, far below any
/// length a game uses, so it is not a tolerance and not a parameter.
const MIN_DIRECTION_LENGTH_SQUARED: f32 = 1000.0 * f32::MIN_POSITIVE;

/// How far a squared length may be from one for a normal to count as unit:
/// the sources' `100 * FLT_EPSILON` (`b2IsNormalized`, `b3IsNormalized`).
/// It is a ratio, not a length.
const UNIT_TOLERANCE: f32 = 100.0 * f32::EPSILON;

/// A convex shape: the hull of up to `N` points, grown by `radius`
/// (`b2ShapeProxy`, `b3ShapeProxy`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Proxy<V, const N: usize> {
    points: [V; N],
    count: usize,
    radius: f32,
}

impl<V: Vector, const N: usize> Proxy<V, N> {
    /// The hull of `points` grown by `radius` (`b2MakeProxy`). It needs at
    /// least one point, keeps the first `N` and, as the source does, drops
    /// the rest. The radius is zero or more.
    pub(crate) fn new(points: &[V], radius: f32) -> Self {
        Self::mapped(points, radius, |point| point)
    }

    /// The hull of `points`, each passed through `map`, grown by `radius`.
    fn mapped(points: &[V], radius: f32, map: impl Fn(V) -> V) -> Self {
        debug_assert!(!points.is_empty(), "a proxy needs a point");
        debug_assert!(radius >= 0.0, "a proxy's radius is not negative");
        // An empty slice is the single point at the frame's origin, so that
        // a release build never reads past the points.
        let count = points.len().clamp(1, N);
        let mut out = Self {
            points: [V::ZERO; N],
            count,
            radius,
        };
        for (slot, point) in out.points.iter_mut().zip(points) {
            *slot = map(*point);
        }
        out
    }

    /// The points whose hull is the shape's core.
    pub(crate) fn points(&self) -> &[V] {
        &self.points[..self.count]
    }

    /// How far the shape reaches beyond its core.
    pub(crate) fn radius(&self) -> f32 {
        self.radius
    }

    /// The same shape moved by `offset`.
    fn translated(&self, offset: V) -> Self {
        let mut out = *self;
        for point in &mut out.points[..self.count] {
            *point = *point + offset;
        }
        out
    }
}

/// What a distance query found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DistanceState {
    /// The cores are apart: the normal is a unit vector and the points are
    /// the closest pair.
    Separated,
    /// The cores overlap, or touch closer than `f32` can tell apart. The
    /// distance is zero, the normal is zero and both points are one point
    /// the cores share.
    Overlapped,
    /// The iteration cap was reached before the search settled. The values
    /// are those of the last simplex solved: the best answer so far.
    IterationCap,
}

/// The closest points between two proxies (`b2DistanceOutput`,
/// `b3DistanceOutput`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ClosestPoints<V> {
    /// Whether the cores are apart. It describes the cores even when the
    /// radii were applied: two balls that overlap are `Separated` at
    /// distance zero while their centres differ.
    pub state: DistanceState,
    /// The closest point on shape A: on its core, or with the radii applied
    /// on its surface.
    pub point_a: V,
    /// The closest point on shape B, likewise.
    pub point_b: V,
    /// Unit normal from A toward B; zero when the cores overlap.
    pub normal: V,
    /// The distance between the cores, or with the radii applied between
    /// the surfaces, where it stops at zero.
    pub distance: f32,
    /// How many support points the search took.
    pub iterations: u32,
}

/// How a shape cast ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CastState {
    /// The moving shape does not come within the target distance of the
    /// fixed one over the motion. The other fields are zero.
    Miss,
    /// First contact: the fraction, a unit normal and the point.
    Hit,
    /// The shapes are within the target distance before any motion:
    /// touching or overlapping. The fraction is zero, the point is one
    /// between the two surfaces, and the normal is the unit normal between
    /// the cores, or zero when the cores themselves overlap.
    InitialOverlap,
    /// The cast gave up: it reached [`MAX_CAST_ITERATIONS`], or `f32` could
    /// not tell the cores apart after an advance. The shapes are still
    /// apart at the fraction reported, which is the best answer so far; the
    /// normal and point are those of the last distance query and the normal
    /// may be zero. A mover stops there.
    Failed,
}

/// The first contact of a moving shape with a fixed one (`b2CastOutput`,
/// `b3CastOutput`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Cast<V> {
    /// How the cast ended.
    pub state: CastState,
    /// The fraction of the translation travelled, in `[0, max_fraction)`.
    pub fraction: f32,
    /// Unit normal at the contact, from the fixed shape A toward the moving
    /// shape B: from the surface toward the body when the body is B.
    pub normal: V,
    /// The contact point on the surface of the fixed shape A.
    pub point: V,
    /// How many distance queries the cast ran.
    pub iterations: u32,
}

/// The larger of two floats as the sources' `b2MaxFloat` takes it.
fn max_float(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

/// The dot product summed in axis order, as `b2Dot` and `b3Dot` sum it.
fn dot<V: Vector>(a: V, b: V) -> f32 {
    let mut sum = a[0] * b[0];
    for axis in 1..V::AXES {
        sum += a[axis] * b[axis];
    }
    sum
}

/// Whether `v` is a unit vector (`b2IsNormalized`, `b3IsNormalized`).
pub(crate) fn is_unit<V: Vector>(v: V) -> bool {
    (1.0 - dot(v, v)).abs() < UNIT_TOLERANCE
}

/// The target distance between the cores at which a cast or a time of
/// impact reports contact, and the tolerance on reaching it, from the
/// caller's `slop`: `max(slop, radii - slop)` and a quarter of `slop`, as
/// the sources derive them from `linearSlop`.
///
/// Shapes with a radius therefore meet `slop` *inside* each other's
/// surfaces, and shapes without one `slop` apart: the sources' soft
/// contact.
fn target_and_tolerance(total_radius: f32, slop: f32) -> (f32, f32) {
    debug_assert!(slop > 0.0, "slop is a positive length");
    (max_float(slop, total_radius - slop), 0.25 * slop)
}

/// The shape cast of both dimensions, by conservative advancement
/// (`b2CastShape`, `b3ShapeCast`): shape `b` moves by `translation_b`
/// against the fixed shape `a`.
///
/// Each step asks `core_distance` how far apart the cores are and along
/// which normal, then moves `b` as far as that normal allows without
/// passing the target distance. The cores cannot meet before the plane
/// between them is crossed, so no step overshoots.
///
/// - `max_fraction`: how much of the translation to consider, usually 1.
/// - `can_encroach`: lets shapes that begin closer than the target, with
///   cores more than `2 * slop` apart, move `slop` closer instead of
///   reporting [`CastState::InitialOverlap`]. A mover sets it so a round
///   body resting on a surface can still slide along it.
/// - `slop`: see [`target_and_tolerance`]. It must be positive; at zero the
///   cast cannot converge and ends [`CastState::Failed`] at the cap.
fn cast_by_advancement<V: Vector, const N: usize>(
    a: &Proxy<V, N>,
    b: &Proxy<V, N>,
    translation_b: V,
    max_fraction: f32,
    can_encroach: bool,
    slop: f32,
    core_distance: impl Fn(&Proxy<V, N>, &Proxy<V, N>) -> ClosestPoints<V>,
) -> Cast<V> {
    let (mut target, tolerance) = target_and_tolerance(a.radius + b.radius, slop);

    let mut out = Cast {
        state: CastState::Miss,
        fraction: 0.0,
        normal: V::ZERO,
        point: V::ZERO,
        iterations: 0,
    };
    let mut fraction = 0.0;
    let mut moved = *b;
    // The last query, for the answer when the cap is reached.
    let mut last_normal = V::ZERO;
    let mut last_point = V::ZERO;

    for iteration in 0..MAX_CAST_ITERATIONS {
        out.iterations += 1;

        let found = core_distance(a, &moved);
        last_normal = found.normal;
        last_point = found.point_a + found.normal * a.radius;

        if found.distance < target + tolerance {
            if iteration == 0 {
                if can_encroach && found.distance > 2.0 * slop {
                    target = found.distance - slop;
                } else {
                    // Initial overlap: a point between the two surfaces.
                    let c1 = found.point_a + found.normal * a.radius;
                    let c2 = found.point_b + found.normal * -b.radius;
                    out.state = CastState::InitialOverlap;
                    out.normal = found.normal;
                    out.point = c1 * (1.0 - 0.5) + c2 * 0.5;
                    return out;
                }
            } else {
                out.fraction = fraction;
                out.normal = found.normal;
                out.point = last_point;
                // An advance never brings the cores together, so a zero
                // distance or a normal that is not unit here is rounding
                // the caller's slop is too small to absorb.
                out.state = if found.state == DistanceState::Overlapped || !is_unit(found.normal) {
                    CastState::Failed
                } else {
                    CastState::Hit
                };
                return out;
            }
        }

        // Are the shapes approaching each other?
        let denominator = dot(translation_b, found.normal);
        if denominator >= 0.0 {
            return out;
        }

        // Advance the sweep.
        fraction += (target - found.distance) / denominator;
        if fraction >= max_fraction {
            return out;
        }
        moved = b.translated(translation_b * fraction);
    }

    out.state = CastState::Failed;
    out.fraction = fraction;
    out.normal = last_normal;
    out.point = last_point;
    out
}

#[cfg(test)]
pub(crate) mod testing {
    //! What the kernel's tests share: a fixed random sequence and the
    //! window in which a cast may report a contact.

    #[track_caller]
    pub(crate) fn assert_near(actual: f32, expected: f32, tolerance: f32) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{actual} is not within {tolerance} of {expected}"
        );
    }

    /// Asserts that a cast reports first contact where geometry puts it.
    ///
    /// `touch` is the fraction at which the two surfaces exactly touch and
    /// `closing_speed` how fast they close along the normal per unit
    /// fraction, both worked by hand in the caller. The kernel's contract
    /// is that it reports contact when the cores are
    /// `max(slop, radii - slop)` apart, give or take a quarter of `slop` on
    /// the far side: so `slop` past touching for shapes with a radius, and
    /// `slop` short of it for shapes without.
    #[track_caller]
    pub(crate) fn assert_contact_fraction(
        fraction: f32,
        touch: f32,
        closing_speed: f32,
        total_radius: f32,
        slop: f32,
    ) {
        let gap = if total_radius - slop > slop {
            -slop
        } else {
            slop - total_radius
        };
        let earliest = touch - (gap + 0.25 * slop) / closing_speed;
        let latest = touch - gap / closing_speed;
        assert!(
            fraction >= earliest - 1e-5 && fraction <= latest + 1e-5,
            "fraction {fraction} is outside {earliest}..{latest}"
        );
    }

    /// A fixed sequence of numbers for the property tests (a 32-bit linear
    /// congruential generator, Numerical Recipes' constants), so every run
    /// and every target sees the same cases.
    pub(crate) struct Sequence(u32);

    impl Sequence {
        pub(crate) fn new(seed: u32) -> Self {
            Self(seed)
        }

        /// The next number in `[0, 1)`, a multiple of 2^-16.
        pub(crate) fn unit(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            f32::from(u16::try_from(self.0 >> 16).expect("sixteen bits")) / 65536.0
        }

        /// The next number in `[low, high)`.
        pub(crate) fn range(&mut self, low: f32, high: f32) -> f32 {
            low + (high - low) * self.unit()
        }
    }
}
