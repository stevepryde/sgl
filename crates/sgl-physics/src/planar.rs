//! Planar bumps (#921): shapes in a plane that turn as they move, for
//! vehicles. When two moving bodies first touch ([`time_of_impact`]), how
//! two overlapping ones separate ([`manifold`]), the velocities a bump
//! leaves them with ([`bump`]), and several bodies advanced together
//! through one step ([`advance`]). 2D only: a 3D game uses it in its
//! ground plane and keeps height (a banked track, a waterline, a bridge
//! above) to itself.
//!
//! It is not part of the mover: a vehicle is not a walking body. It holds
//! no state: a [`Body`] is plain data the game owns, and every function is
//! pure. What a vehicle does next, its handling, is the game's; the bump
//! only answers what the contact does.
//!
//! **Shapes.** A body is any 2D [`Shape`] in its own frame, turned by its
//! heading: a box, a ball, a capsule or a convex polygon. Its position is
//! its centre of mass and the point it turns about.
//!
//! **Headings.** The crate calls no trigonometric function, so a heading
//! is the cosine and sine of the body's angle, `(cos, sin)`, and a turn of
//! `angle` radians is `Box2D`'s normalised first-order step
//! (`b2IntegrateRotation`): it turns by `atan(angle)`, short of `angle` by
//! about a third of `angle` squared as a share (one per cent at a little
//! over a tenth of a radian per step). A game that needs an exact angle
//! keeps it itself and sets the heading from it.
//!
//! **Contact a slop apart.** Contact is decided within a small length that
//! scales with the two shapes, the *slop*, by the rule every query in the
//! crate uses: a hundredth of the smaller shape's extent, at most a quarter
//! of the smaller radius, and never below the rounding of the coordinates
//! met. [`time_of_impact`] reports contact when the surfaces come within
//! three quarters to one and a quarter slops of each other, before they
//! touch.
//!
//! **Relative.** Every pair is computed in a frame at one of its bodies,
//! so a scene several kilometres from the origin behaves as the same scene
//! at it: bit for bit when the shift keeps the differences between
//! positions exact (a power of two on a coarse grid), but for the one
//! rounding of each result back into world coordinates.
//!
//! The time of impact is the kernel's (`b2ComputeTimeOfImpact`), the
//! manifold is ported from `Box2D` 3.2's `src/manifold.c`, and the bump is
//! the single-pair sequential impulse of its `src/contact_solver.c`
//! (`PROVENANCE.md`).

use core::cmp::Ordering;

use sgl_core::math::Vec2;
use sgl_core::{CanonicalWrite, StateHasher};

use crate::convex::d2::Rot;
use crate::shape::Shape;
use crate::space;

mod advance;
mod bump;
mod collide;
mod manifold;
mod toi;

pub use advance::{Impact, MAX_IMPACTS, MAX_SEPARATION_PASSES, Settings, advance};
pub use bump::{Bump, bump};
pub use manifold::{Manifold, manifold};
pub use toi::{Toi, time_of_impact};

/// A body moving in a plane: plain data the game owns, sends and restores.
///
/// `inverse_mass` and `inverse_inertia` are zero for something a bump never
/// moves or turns (a wall, a parked prop); [`box_inverse_inertia`] and
/// [`disc_inverse_inertia`] give a uniform body's. A body whose numbers are
/// not sound occupies nothing: one with a non-finite number, a negative
/// inverse mass or inertia, a heading with no direction, or a shape that
/// occupies nothing (`Shape`'s rules). Every function here leaves it as it
/// is and meets nothing with it.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Body {
    /// The game's id for the body, which [`Impact`] reports and which
    /// orders bodies; SGL hands out no handles.
    pub id: u64,
    /// The body's shape in its own frame, centred on `position` and turned
    /// by `heading`.
    pub shape: Shape<Vec2>,
    /// The centre of mass, in world units: the point the body turns about.
    pub position: Vec2,
    /// The cosine and sine of the body's angle: where its own x axis
    /// points. Normalised before use, so `(2, 0)` is `(1, 0)`.
    pub heading: Vec2,
    /// World units per second.
    pub velocity: Vec2,
    /// Radians per second, counter-clockwise (x toward y) positive.
    pub spin: f32,
    /// One over the mass; zero for a body a bump never pushes.
    pub inverse_mass: f32,
    /// One over the moment of inertia about `position`; zero for a body a
    /// bump never turns.
    pub inverse_inertia: f32,
}

impl Body {
    /// The largest postcard encoding: a `u64` varint (ten bytes), the
    /// largest 2D shape, three vectors and three `f32`s (112 bytes).
    pub const MAX_POSTCARD_BYTES: usize =
        10 + Shape::<Vec2>::MAX_POSTCARD_BYTES + 3 * space::postcard_bytes::<Vec2>() + 3 * 4;

    /// The heading as a unit rotation when the body's numbers are sound.
    fn sound_rotation(&self) -> Option<Rot> {
        let sound = self.shape.is_sound()
            && space::is_finite(self.position)
            && space::is_finite(self.velocity)
            && self.spin.is_finite()
            && self.inverse_mass.is_finite()
            && self.inverse_mass >= 0.0
            && self.inverse_inertia.is_finite()
            && self.inverse_inertia >= 0.0;
        if !sound {
            return None;
        }
        // The crate's one normalising routine (physics.md 12): a heading of
        // any finite length names its direction, however short or long.
        space::direction(self.heading).map(|(unit, _)| Rot {
            c: unit.x,
            s: unit.y,
        })
    }

    /// Whether nothing a bump does moves or turns it.
    fn fixed(&self) -> bool {
        self.inverse_mass == 0.0 && self.inverse_inertia == 0.0
    }

    /// A fixed total order over bodies: id, then position, heading and
    /// velocity component by component, spin, the inverse mass and inertia,
    /// then shape. [`advance`] processes bodies in this order.
    fn order(&self, other: &Self) -> Ordering {
        self.id
            .cmp(&other.id)
            .then_with(|| space::compare(self.position, other.position))
            .then_with(|| space::compare(self.heading, other.heading))
            .then_with(|| space::compare(self.velocity, other.velocity))
            .then_with(|| self.spin.total_cmp(&other.spin))
            .then_with(|| self.inverse_mass.total_cmp(&other.inverse_mass))
            .then_with(|| self.inverse_inertia.total_cmp(&other.inverse_inertia))
            .then_with(|| self.shape.order(&other.shape))
    }
}

impl CanonicalWrite for Body {
    /// `id` (`u64`), the shape, then the bits of `position`, `heading` and
    /// `velocity` in axis order, `spin`, `inverse_mass` and
    /// `inverse_inertia`.
    fn canonical_write(&self, out: &mut StateHasher) {
        out.u64(self.id);
        self.shape.canonical_write(out);
        space::write(out, self.position);
        space::write(out, self.heading);
        space::write(out, self.velocity);
        out.u32(self.spin.to_bits());
        out.u32(self.inverse_mass.to_bits());
        out.u32(self.inverse_inertia.to_bits());
    }
}

/// The inverse moment of inertia of a uniform box of half-extents `half`
/// and `mass` about its centre: `3 / (mass * (half.x² + half.y²))`. An
/// infinite mass (something immovable) gives zero, and so does a box with
/// no size, which no contact can turn.
#[must_use]
pub fn box_inverse_inertia(mass: f32, half: Vec2) -> f32 {
    inverse(mass * (half.x * half.x + half.y * half.y) / 3.0)
}

/// The inverse moment of inertia of a uniform disc of `radius` and `mass`
/// about its centre: `2 / (mass * radius²)`. An infinite mass gives zero,
/// and so does a disc with no size.
#[must_use]
pub fn disc_inverse_inertia(mass: f32, radius: f32) -> f32 {
    inverse(mass * radius * radius / 2.0)
}

fn inverse(moment: f32) -> f32 {
    if moment > 0.0 { 1.0 / moment } else { 0.0 }
}

/// How a body moves over a step: a translation, and a turn in radians
/// (counter-clockwise positive) by the crate's rotation rule (see the
/// module's notes on headings).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    /// How far the body's centre moves, in world units.
    pub translation: Vec2,
    /// How far it turns, in radians.
    pub turn: f32,
}

impl Motion {
    /// The motion `body`'s velocity and spin give it over `dt` seconds.
    #[must_use]
    pub fn of(body: &Body, dt: f32) -> Self {
        Self {
            translation: body.velocity * dt,
            turn: body.spin * dt,
        }
    }

    fn is_sound(&self) -> bool {
        space::is_finite(self.translation) && self.turn.is_finite()
    }
}

/// Where two bodies touch: a point midway between their surfaces, in world
/// units, and the unit normal from the first body toward the second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    /// The contact point.
    pub point: Vec2,
    /// Unit normal from body `a` toward body `b`.
    pub normal: Vec2,
}

impl Contact {
    fn moved(self, by: Vec2) -> Self {
        Self {
            point: by + self.point,
            ..self
        }
    }
}
