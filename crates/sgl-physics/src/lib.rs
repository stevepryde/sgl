//! Kinematic physics for 2D and 3D games: shapes, worlds, queries and a
//! mover. Bodies move because the game says so; SGL finds what they hit and
//! where they end up. There are no rigid bodies, forces, joints or solver,
//! no loop, and no state the game does not own.
//!
//! Every type and function is generic over [`Vector`], implemented for
//! `Vec2` and `Vec3`: one implementation serves both dimensions.
//!
//! A game describes its world as a [`ColliderSet`] it fills, a slice of
//! [`Collider`]s, a [`CellWorld`] it answers cell by cell (a tile map or a
//! voxel grid), or a pair of those, and asks it [`slide`],
//! [`snap_to_ground`], [`overlaps`], [`cast`] and [`ray`] inside its own
//! fixed step.
//!
//! # Conventions owned here
//!
//! **Normals** point from the surface toward the body that met it. Box
//! normals are axis-aligned unit vectors.
//!
//! **No up axis is assumed.** [`MoverConfig::up`] names it: a y-up 2D game
//! passes `Vec2::Y`, a y-down one `Vec2::NEG_Y`, a 3D game `Vec3::Y`. Faces
//! are classified by `normal · up`, so floor, ceiling, one-way and ground
//! snap mirror exactly between conventions.
//!
//! **Units** are the game's. Cell sizes, skin and snap distance are
//! parameters, never constants.
//!
//! **Identity** is the game's: a [`Collider`]'s `id` is a `u64` the game
//! chooses, which queries report. SGL hands out no handles. Ids are unique
//! within a [`ColliderSet`] (inserting one already there replaces it) and
//! may repeat in a slice, a [`CellWorld`] or a pair.
//!
//! **Ties between axes.** When a box meets another exactly at an edge or a
//! corner, two axes enter at the same instant. The mover
//! ([`slide`], [`snap_to_ground`]) resolves such a tie to the axis
//! most along [`MoverConfig::up`], then y, then x, then z, so a body arriving
//! exactly at a ledge's corner lands whichever axis is up. [`cast`] and
//! [`ray`], which know no up axis, resolve it y, then x, then z.
//!
//! # Contracts
//!
//! - **Pure and plain.** A result depends only on the arguments and what the
//!   world answers. Every value a game keeps is `Copy` data with no handle
//!   into SGL, so restoring an earlier value and repeating the same calls
//!   repeats the same results. With the `serde` feature the state types
//!   serialise, each with its largest postcard size as `MAX_POSTCARD_BYTES`,
//!   and all implement `CanonicalWrite` for a desync digest.
//! - **Near is as good as far.** A query works relative to the body, so its
//!   accuracy depends on the shapes' sizes and the distance between them,
//!   not on their distance from the origin.
//! - **The same on every target.** Only IEEE-754 add, subtract, multiply,
//!   divide, square root, comparison, minimum, maximum and floor, in a fixed
//!   order, with no fused multiply-add and no platform maths function. Equal
//!   inputs give bit-equal results on native and `wasm32`.
//! - **Order never shows.** Contacts at the same instant resolve to the
//!   lowest id, then the lowest position component by component, never to
//!   the order a world visits colliders in or a set was built in.
//! - **Bounded work.** No query walks a range the caller's data can make
//!   unbounded; a query over a world's cap is [`Unanswered`] and moves
//!   nothing.
//!
//! See `specs/physics.md` in the repository for the full contract.

mod cell_world;
mod collider_set;
mod config;
mod contacts;
mod query;
mod shape;
mod slide;
mod space;
mod sweep;
mod world;

pub use cell_world::{CellColliders, CellWorld, MAX_QUERY_CELLS};
pub use collider_set::ColliderSet;
pub use config::{MoverConfig, MoverSettings};
pub use contacts::Contacts;
pub use query::{Hit, cast, overlaps, ray};
pub use shape::{ALL_LAYERS, Collider, ColliderKind, Shape};
pub use slide::{MAX_SLIDES_2D, MAX_SLIDES_3D, slide, snap_to_ground};
pub use space::Vector;
pub use world::{Unanswered, World};
