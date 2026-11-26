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
//! fixed step; [`distance`] measures a shape against one collider. What a
//! pointer or a crosshair is over is [`ray_spans`]: every collider a ray
//! touches, faces, edges and tangents included, and the stretch of the ray
//! inside each.
//!
//! In 3D, triangles a game gives once (a [`TriangleSet`], from a model's
//! positions and indices) and ground given as heights (a [`HeightField`])
//! are worlds too: what a camera's ball cast, a line of sight or
//! [`ground_under`] a wheel meets. Nothing walks on them; the mover's
//! floors are authored as simple shapes.
//!
//! A body or a collider is a [`Shape`]: a box, a ball, a capsule, or a
//! [`Convex`] polygon (2D) or hull (3D) turned as the game chose. Box
//! against box is a closed-form sweep and exact, and a [`ray`] is exact
//! against a box, a ball, a capsule, a 2D polygon and a 3D triangle; every
//! other pair goes through a convex kernel ported from `Box2D` and `Box3D`
//! (`PROVENANCE.md`), to within a slop that scales with the shapes.
//!
//! A character is a [`MoverState`] the game keeps and steps with
//! [`move_and_slide`] (Godot's name and meaning): it sets the velocity and
//! reads back the position, the velocity less what was blocked, and the
//! floor, with a step-up, a ledge guard, one-way drop-through and push-out
//! chosen per call in [`MoverOptions`]. [`move_and_collide`] stops a body at
//! the first contact, and [`find_ledge`] finds a ledge to climb.
//!
//! Vehicles that bump in a plane are [`planar`]: the time of impact of
//! bodies that turn as they move, the manifold of overlapping ones, the
//! bump, and [`planar::advance`], several bodies stepped through their
//! impacts in time order.
//! Moving colliders (lifts, doors, other bodies) carry a velocity the game
//! sets as it moves them: the mover carries a body standing on one, pushes
//! out a body one moved into (or reports it `crushed`), and nudges a body
//! off a [`Push::Soft`] one by a limit. [`sensors_at`] and
//! [`sensor_events`] tell a game which triggers a body entered and left.
//!
//! # Conventions owned here
//!
//! **Normals** point from the surface toward the body that met it. Box
//! normals are axis-aligned unit vectors; a ramp's plane (a [`Cut`]) reports
//! its own unit normal; a round or convex shape's contact reports the unit
//! normal between the shapes, which leans at an edge or a corner.
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
//! [`ray`], which know no up axis, resolve it y, then x, then z. A ramp's
//! plane comes after every axis.
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
mod convex;
mod ground;
mod height_field;
mod ledge;
mod mover;
pub mod planar;
mod plane;
mod query;
mod sensor;
mod shape;
mod slide;
mod space;
mod span;
mod surface;
mod sweep;
mod triangle_set;
mod world;

pub use cell_world::{CellColliders, CellWorld, MAX_QUERY_CELLS};
pub use collider_set::ColliderSet;
pub use config::{MoverConfig, MoverSettings};
pub use contacts::Contacts;
pub use ground::{Ground, ground_under};
pub use height_field::{HeightField, MAX_HEIGHT_SAMPLES};
pub use ledge::{Ledge, find_ledge};
pub use mover::{
    Floor, LEDGE_SEARCH_STEPS, MAX_PUSH_OUT_PLANES, MoverOptions, MoverState, move_and_slide,
};
pub use query::{Hit, Separation, cast, distance, move_and_collide, overlaps, ray};
pub use sensor::{sensor_events, sensors_at};
pub use shape::{ALL_LAYERS, Collider, ColliderKind, Convex, Cut, Push, Shape};
pub use slide::{MAX_SLIDES_2D, MAX_SLIDES_3D, slide, snap_to_ground};
pub use space::Vector;
pub use span::{RaySpan, ray_spans};
pub use surface::SurfaceError;
pub use triangle_set::{MAX_QUERY_TRIANGLES, MAX_TREE_DEPTH, MAX_TRIANGLES, TriangleSet};
pub use world::{Unanswered, World};
