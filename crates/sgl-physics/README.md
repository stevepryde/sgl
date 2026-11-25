# sgl-physics

Kinematic physics for 2D and 3D games. Bodies move because the game says
so; SGL finds what they hit and where they end up. There are no rigid
bodies, forces, joints or solver, no loop, and no state the game does not
own: the game calls these functions inside its own fixed step. It depends on
`sgl-core` only, so a headless server builds it without a GPU.

Every type and function is generic over `Vector`, implemented for
`sgl_core::math::Vec2` and `Vec3`: one implementation serves both.

## Choose a function

| Need | API |
| --- | --- |
| Move a body by a delta, sliding along what it meets | [`slide`](src/slide.rs) |
| Keep a walking body on the ground across seams and down steps | [`snap_to_ground`](src/slide.rs) |
| What is in a trigger or a hit box | [`overlaps`](src/query.rs) |
| What a moving shape hits first (a projectile, a sweep for room) | [`cast`](src/query.rs) |
| What a ray hits first (a beam, line of sight, picking a block) | [`ray`](src/query.rs) |
| A world of many colliders the game fills | [`ColliderSet`](src/collider_set.rs) |
| A tile map or voxel grid the game answers cell by cell | [`CellWorld`](src/cell_world.rs) |
| A handful of colliders, or two worlds at once | a slice, or a pair `(A, B)`: [`World`](src/world.rs) |

A body is a `Shape` (in this release, `Shape::Box { half }`) at a position
(its centre). A collider adds a `ColliderKind` (`Solid`, `OneWay`,
`Sensor`), the layers it is on and the game's own `id: u64`, which queries
report: SGL hands out no handles. An id is unique within a `ColliderSet`
(inserting it again replaces the collider) and may repeat in a slice, a
`CellWorld` or a pair. Every query takes a layer `mask` (`ALL_LAYERS` for
everything).

```rust
use sgl_core::math::Vec2;
use sgl_physics::{
    ALL_LAYERS, Collider, ColliderSet, MoverConfig, MoverSettings, Shape, slide,
};

let mut level = ColliderSet::new(2.0);
level.insert(Collider::new(7, Shape::Box { half: Vec2::new(8.0, 0.5) }, Vec2::ZERO));
let config = MoverConfig::new(MoverSettings {
    up: Vec2::Y,
    skin: 0.01,
    snap_distance: 2.0 / 32.0,
    normal_threshold: 0.7,
    block_epsilon: 1e-4,
});
let body = Shape::Box { half: Vec2::new(0.4, 0.5) };
let mut position = Vec2::new(0.0, 3.0);
let (moved, contacts) =
    slide(&body, position, Vec2::new(0.5, -4.0), &level, ALL_LAYERS, &config)
        .expect("a set always answers");
position += moved;
assert!(contacts.floor && contacts.last_floor_id == Some(7));
```

## Worlds

A `ColliderSet` buckets colliders in a grid of the game's cell size (a
spatial hash bucket, not a tile size). It is derived data: a game may keep
it, or rebuild it from its own state at any step in any order, and get the
same results.

A `CellWorld` takes a cell size and a function from an integer cell to that
cell's colliders, which it pushes in cell-local units: the cell runs from 0
to 1 along each axis, so a full block is `Shape::Box { half: splat(0.5) }`
at `splat(0.5)`. A collider must lie within its cell. What an unloaded cell
holds is the game's policy: a full box keeps bodies out, nothing lets them
through. One query walks at most `MAX_QUERY_CELLS` (4096) cells; a moving
query walks only the cells along its motion, so a long ray is answered
where the box enclosing it would not be. A larger walk is `Unanswered`, and
the body moves nothing.

## Settings

`MoverConfig::new(MoverSettings { up, skin, snap_distance, normal_threshold,
block_epsilon })` holds the game's units and axis convention and has no
defaults: `Vec2::Y`
for a y-up 2D world, `Vec2::NEG_Y` for a y-down one, `Vec3::Y` for SGL3D's.
`skin` is the gap a contact leaves; `normal_threshold` (`normal · up`)
separates floors and ceilings from walls; `block_epsilon` is the shortfall
the blocked flags ignore. `new` panics on an unusable `up`, a threshold
outside `(0, 1)` or a negative or non-finite length.

A move slides at most `MAX_SLIDES_2D` (2) times in 2D and `MAX_SLIDES_3D`
(3) in 3D, stops a `skin` short of each contact along its path, breaks an
exact tie between axes at a corner toward `up` (so a body landing exactly on
a ledge's corner lands), and never pushes a body
out of geometry it starts inside: it may leave or slide, never go deeper.
`Contacts` reports floor, wall and ceiling contacts, the last floor and
ceiling normals and the ids of the last floor, wall and ceiling met.

## Determinism and the network

Results are bit-equal on native and `wasm32` (only IEEE-754 basic
arithmetic, square root, comparison and floor, in a fixed order), never
depend on the order a world visits its colliders in (ties go to the lowest
id, then the lowest position, however many times a world visits a
collider), and are computed relative to the body, so a
body far from the origin is as accurate as one near it. Accuracy at a
contact follows the larger of the two shapes and their distance: split a
very long collider (a whole level's floor) into pieces.

With the `serde` feature the plain types serialise; each documents its
largest postcard size as `MAX_POSTCARD_BYTES` (for example
`Contacts::<Vec3>::MAX_POSTCARD_BYTES`, 86), to budget against `sgl-net`'s
`MAX_LATEST_STATE_BYTES`. All implement `sgl_core::CanonicalWrite`, so a
client and its server can compare one digest to find a desync.

Read the [physics contract](../../specs/physics.md) before changing
behaviour. [Parity fixtures](../../tests/sgl-physics/tests/parity.rs),
[property tests](../../tests/sgl-physics/tests/properties.rs) and the
[state round trip](../../tests/sgl-physics/tests/state.rs) exercise the
cross-target, geometric and serialisation boundaries. Run
`cargo test -p sgl-physics -p sgl-physics-tests` for focused native checks.
See [Physics](../../docs/README.md#physics) to choose a game's physics.
