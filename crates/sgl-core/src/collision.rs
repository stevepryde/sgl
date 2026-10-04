//! Pure swept-AABB collision: boxes, a closed-form sweep, a uniform broadphase
//! grid, and kinematic move-and-slide resolution for a platformer body.
//!
//! Nothing here knows about a player, a renderer, or a clock. A game builds a
//! [`ColliderSet`] from its world, then drives [`move_and_collide`] and
//! [`snap_to_ground`] over it with its own [`CollisionConfig`].
//!
//! # Conventions owned here
//!
//! **Normals.** A surface normal points *from the surface toward the moving
//! body* — the face the body ran into pushes back along it. Returned normals
//! are axis-aligned unit vectors signed against the motion on the entering
//! axis.
//!
//! **Axis agnostic.** The module never assumes which way is up.
//! [`CollisionConfig::up`] names the world's up axis: a y-up game passes
//! `Vec2::Y`, a y-down game passes `Vec2::NEG_Y`. Faces are classified by
//! `normal · up`, so floor, ceiling, one-way, and ground-snap semantics mirror
//! exactly between the two.
//!
//! **Units.** Every distance is in the caller's world units. Grid cell size,
//! skin, and snap distance are parameters, not constants, so a 32 px-per-unit
//! platformer and a 1 px-per-unit puzzle share one implementation.

mod aabb;
mod collider_set;
mod sweep;

pub use aabb::{Aabb, Hit, sweep_aabb};
pub use collider_set::{Collider, ColliderFlags, ColliderSet};
pub use sweep::{CollisionConfig, Contacts, move_and_collide, snap_to_ground};
