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
| Step a character: velocity in; position, velocity and floor out, with step-up, ledge guard, drop-through and push-out | [`move_and_slide`](src/mover.rs) |
| Move until the first contact and stop there (a projectile, a pushed crate, a bounce of the game's own) | [`move_and_collide`](src/query.rs) |
| Find a ledge a body pressed against a wall could hang from and climb onto | [`find_ledge`](src/ledge.rs) |
| Move a body by a delta, sliding along what it meets | [`slide`](src/slide.rs) |
| Keep a walking body on the ground across seams and down steps | [`snap_to_ground`](src/slide.rs) |
| What is in a trigger or a hit box | [`overlaps`](src/query.rs) |
| What a moving shape hits first (a projectile, a sweep for room) | [`cast`](src/query.rs) |
| What a ray hits first (a beam, line of sight, picking a block) | [`ray`](src/query.rs) |
| How far a shape is from a collider (a hit box's reach, an explosion's falloff) | [`distance`](src/query.rs) |
| A world of many colliders the game fills | [`ColliderSet`](src/collider_set.rs) |
| A tile map or voxel grid the game answers cell by cell | [`CellWorld`](src/cell_world.rs) |
| A handful of colliders, or two worlds at once | a slice, or a pair `(A, B)`: [`World`](src/world.rs) |
| Vehicles bumping in a plane: cars, karts, traffic, boats, pushed crates that turn | [`planar::advance`](src/planar/advance.rs), or [`planar::time_of_impact`](src/planar/toi.rs), [`planar::manifold`](src/planar/manifold.rs) and [`planar::bump`](src/planar/bump.rs) one pair at a time |

A body is a `Shape` at a position (its centre): `Shape::Box { half }`,
`Shape::Ball { radius }`, `Shape::Capsule { half_axis, radius }` (the
segment from `-half_axis` to `half_axis` grown by `radius`; an upright
character has `half_axis` along `up`), or `Shape::Convex`, a convex polygon
of up to 8 vertices in 2D (`Convex::polygon(points, cos, sin)`) or hull of
up to 16 points in 3D (`Convex::hull(points, quaternion)`), turned by a
rotation the game supplies: a rotated box, a wall at an angle, a ramp of
any slope. Every shape is a body and a collider for every query and the
mover. A collider adds a `ColliderKind` (`Solid`, `OneWay`,
`Sensor`), the layers it is on and the game's own `id: u64`, which queries
report: SGL hands out no handles. A collider's box may be cut by one plane
into a ramp, a slope tile or a slope block: `with_cut(Cut::new(normal,
offset))` keeps the box where `normal · (p - centre) <= offset`. In 3D the
normal has a zero component (a ramp turning about one axis: upright,
inverted or sideways); `Cut::new` panics otherwise. Only a box takes a cut,
and a one-way platform is a whole box. Queries meet a ramp as the shape it is, and a contact on its
plane reports the plane's normal. An id is unique within a `ColliderSet`
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

## The mover

`move_and_slide(&mut state, &shape, dt, &world, &config, &options)` is one
fixed step of a character, Godot's `move_and_slide`. The game keeps a
`MoverState { position, velocity, floor }` (plain data; `floor` is the
`Floor { normal, id }` it stands on, or `None`) and sets `velocity` each
step: gravity, jumping, speeds, flying and water are its own. The step
moves by `velocity * dt`, snaps a walking body to the ground, removes from
`velocity` the part into every face it touched, in order (a landing zeroes
the fall, a head-bump the rise, a wall the part into it; in a corner or a
wedge it keeps only what runs along both), and sets `floor`: for a walking
body, what lies under it where it ends. A body standing on a walkable ramp
stays put under gravity. A body that starts inside a solid is pushed out
the shortest way, at most its own full extent along each axis, ramps'
normals included, or left where it is and reported `stuck`. A state whose
position, velocity or body is not finite, or whose step would end where
`f32` cannot hold the position, is left as it was.

`MoverOptions` are per call, because they change with the body's state:

| Option | Effect |
| --- | --- |
| `step_height` | Tallest step a walking body climbs without a jump; `0` climbs nothing |
| `ledge_drop` | `Some(depth)` keeps a walking body from walking off a drop deeper than `depth` and `snap_distance` (crouch-walking); it still walks down ramps; `None` guards nothing |
| `drop_through` | Passes one-way platforms for this step: hold it to drop through the one stood on |
| `mask` | Layers the body collides with |

`MoverOptions::default()` climbs nothing, guards nothing and meets every
layer. `Contacts` reports `stepped`, `ledge_guarded`, `pushed_out` and `stuck`
beside the floor, wall and ceiling contacts. Crouching needs nothing new:
change the body's shape, and ask `overlaps` whether the taller one fits
before standing. `find_ledge(&shape, position, toward, &world, &config,
reach, mask)` returns where a body pressed against a wall would hang from
its top edge and where it would stand on it; hanging, climbing and their
timing are the game's. The [platformer example](../../examples/sgl-physics/examples/platformer.rs)
walks, jumps, climbs a step and drops through a platform.

## Round and convex shapes

Box against box, whole or cut, is a closed-form sweep and exact. Every
other pair goes through a convex kernel ported from Box2D and Box3D
([provenance](PROVENANCE.md)), which makes contact a *slop* short of
touching: a cast stops between one and one and a quarter slops before the
surfaces meet. A query's slop is a hundredth of the smaller shape's extent
(at most a quarter of the smaller radius); the mover's is a sixteenth of
`skin`, and it reports the contact where the body would touch, so after
the skin back-off a round or convex body rests between `skin` and
`skin * 65 / 64` from a flat face, as a box rests `skin` from it. Its
contact normal is the one between the shapes, so a ball slides around an
outside corner and over the seams between flush tiles where a box
catches. A ball or a capsule meeting a one-way platform's edge while
falling lands on it. A step-up carries a round body far enough over the
step's edge to stand on it. A body deep inside a round or convex collider
(a ball whose centre is inside a box) is not moved by `slide`; the
mover's push-out frees it where its candidates can. See
[physics.md](../../specs/physics.md), requirement 19, for every rule.

## Planar bumps

`sgl_physics::planar` is for vehicles that collide in a plane: a 3D game
uses it in its ground plane and keeps height to itself. It is not the
mover and has no world: the game owns a slice of `planar::Body` (its `id`,
a 2D `Shape` in its own frame, `position`, `heading` as `(cos, sin)`,
`velocity`, `spin` and inverse mass and inertia, zero for a wall or a
parked prop; `box_inverse_inertia` and `disc_inverse_inertia` give a
uniform body's) and steps it each tick:

```rust
use sgl_core::math::Vec2;
use sgl_physics::Shape;
use sgl_physics::planar::{self, Body, Settings};

let half = Vec2::new(2.0, 1.0);
let car = |id, x: f32, vx: f32| Body {
    id,
    shape: Shape::Box { half },
    position: Vec2::new(x, 0.0),
    heading: Vec2::new(1.0, 0.0),
    velocity: Vec2::new(vx, 0.0),
    spin: 0.0,
    inverse_mass: 1.0 / 1200.0,
    inverse_inertia: planar::box_inverse_inertia(1200.0, half),
};
let mut cars = [car(1, 0.0, 30.0), car(2, 4.25, 0.0)];
let impacts = planar::advance(&mut cars, 1.0 / 60.0, &Settings::new(0.2, 0.5), |_, _| true);
assert_eq!((impacts[0].a, impacts[0].b), (1, 2));
```

`advance` finds the earliest time of impact among the pairs, moves every
body to it, bumps that pair and carries on, at most `MAX_IMPACTS` (64)
times a step, then pushes apart whatever still overlaps, and returns the
impacts in time order (ids, when, where, normal, impulse) for a crunch, a
score or a broken combo. Its `collide` closure leaves out pairs apart in
height (a bridge over a road). A bump keeps momentum, adds no energy at
restitution 1 or below, and is the game's to scale; handling stays the
game's. A turn by `spin * dt` follows Box2D's rotation step, which turns by
`atan(spin * dt)`: keep an exact angle in the game if it matters. Contact is
decided within a slop of the shapes' size, as for every kernel pair. See
[physics.md](../../specs/physics.md), requirement 20.

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
the blocked flags ignore. `new` panics on a zero or non-finite `up`, a
threshold outside `(0, 1)` or a negative or non-finite length; any other
`up`, however short or long, is normalised to a unit vector. `skin` must be
larger than the spacing of `f32` values where the bodies are (about the
coordinate times 2^-23): at 65,536 units a skin of 0.01 is too small.

A move slides at most `MAX_SLIDES_2D` (3) times in 2D and `MAX_SLIDES_3D`
(4) in 3D, stops a `skin` short of each contact along its path, breaks an
exact tie between axes at a corner toward `up` (so a body landing exactly on
a ledge's corner lands), and never pushes a body
out of geometry it starts inside: it may leave or slide, never go deeper.
After a floor (a face within `normal_threshold` of `up`, a walkable ramp
included) the motion keeps all of its part across `up`, so ramps and
landings do not slow a body; after a wall or a ceiling (a ramp too steep to
stand on included) it slides along the face. Between two faces it follows
their crease. `snap_to_ground` keeps a body on a ramp it walks down.
`Contacts` reports floor, wall and ceiling contacts, the last floor,
ceiling and wall normals and the ids of the last floor, wall and ceiling
met.

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
`Contacts::<Vec3>::MAX_POSTCARD_BYTES`, 102), to budget against `sgl-net`'s
`MAX_LATEST_STATE_BYTES`. All implement `sgl_core::CanonicalWrite`, so a
client and its server can compare one digest to find a desync.

The crate knows nothing of `sgl-net`: a networked game steps the mover in
one function its server and its client's `sgl_core::prediction::Predictor`
both call, sends `MoverState` as its snapshot, and replays unacknowledged
inputs from it. The [networked mover](../../examples/sgl-physics/examples/net_mover.rs)
does this over `sgl-net`, with the budget for a latest-state payload (24
players' 3D `MoverState`), and its [test](../../tests/sgl-physics/tests/net.rs)
fails if a state stops surviving the trip.

Read the [physics contract](../../specs/physics.md) before changing
behaviour. [Parity fixtures](../../tests/sgl-physics/tests/parity.rs),
[property tests](../../tests/sgl-physics/tests/properties.rs), the
[state round trip](../../tests/sgl-physics/tests/state.rs), the
[mover tests](../../tests/sgl-physics/tests/mover.rs), the
[game scenarios](../../tests/sgl-physics/tests/scenarios.rs), the
[round-body tests](../../tests/sgl-physics/tests/round.rs), the
[planar tests](../../tests/sgl-physics/tests/planar.rs) and the
comparisons with parry ([kernel](../../tests/sgl-physics/tests/oracle_parry.rs),
[planar](../../tests/sgl-physics/tests/oracle_parry_planar.rs))
exercise the cross-target, geometric, serialisation and mover boundaries. Run
`cargo test -p sgl-physics -p sgl-physics-tests` for focused native checks.
See [Physics](../../docs/README.md#physics) to choose a game's physics.
