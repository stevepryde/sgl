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
| Which triggers a body entered and left this step | [`sensors_at`, `sensor_events`](src/sensor.rs) |
| What a moving shape hits first (a projectile, a sweep for room) | [`cast`](src/query.rs) |
| What a ray hits first (a beam, line of sight) | [`ray`](src/query.rs) |
| What a pointer, a crosshair or an aimed tool is over (picking a block, a hit box, a target) | [`ray_spans`](src/span.rs) |
| How far a shape is from a collider (a hit box's reach, an explosion's falloff) | [`distance`](src/query.rs) |
| A world of many colliders the game fills, and moves by id | [`ColliderSet`](src/collider_set.rs) |
| A tile map or voxel grid the game answers cell by cell | [`CellWorld`](src/cell_world.rs) |
| A handful of colliders, or two worlds at once | a slice, or a pair `(A, B)`: [`World`](src/world.rs) |
| Vehicles bumping in a plane: cars, karts, traffic, boats, pushed crates that turn | [`planar::advance`](src/planar/advance.rs), or [`planar::time_of_impact`](src/planar/toi.rs), [`planar::manifold`](src/planar/manifold.rs) and [`planar::bump`](src/planar/bump.rs) one pair at a time |
| Triangles from a model or authored geometry, for sight lines, camera casts and ground (3D) | [`TriangleSet`](src/triangle_set.rs) |
| Ground given as a grid of heights (3D) | [`HeightField`](src/height_field.rs) |
| The ground under a wheel, a hovering racer or a thing placed on terrain (3D) | [`ground_under`](src/ground.rs) |

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
layer. `Contacts` reports `stepped`, `ledge_guarded`, `pushed_out`, `stuck`,
`crushed`, `carried_by` and `pushed_by` beside the floor, wall and ceiling
contacts. Crouching needs nothing new:
change the body's shape, and ask `overlaps` whether the taller one fits
before standing. `find_ledge(&shape, position, toward, &world, &config,
reach, mask)` returns where a body pressed against a wall would hang from
its top edge and where it would stand on it; hanging, climbing and their
timing are the game's. The [platformer example](../../examples/sgl-physics/examples/platformer.rs)
walks, jumps, climbs a step and drops through a platform.

## Moving colliders

A lift, a door, a crusher, a boat or another character is a collider the
game moves itself, each step, before it steps the bodies near it: it sets
the collider's `position` and its `velocity` (units per second, so
`velocity * dt` is what it moved this step) in its `ColliderSet` by the
collider's own id (`insert`, `set_position`, `set_velocity`, `remove`;
each returns whether the id was there), or rebuilds the set from its own
state. Then `move_and_slide`:

- carries a body whose `floor` is a moving collider as it moved, stopped by
  anything else in the way (`carried_by`), so a rider keeps its place on a
  lift going up, down or sideways; a body that leaves it, by a jump or a
  walk off the edge, keeps the platform's velocity, as Godot's
  `CharacterBody` does;
- presses a body that a moving collider moved into the way the collider
  moved (`pushed_out`, `pushed_by`), or, when something else holds it
  there, reports it `crushed` and leaves it inside nothing but the
  collider: the game decides what crushing does;
- with `with_push(Push::Soft(limit))`, never stops a body at the collider
  but nudges it off by at most `limit` a step, after its move: for crowds,
  and players who may push past one another. `pushed_by` names the soft
  collider the body moved furthest along, or none when the pushes cancel.

Only the mover reads a velocity or a push; every other query takes a
collider where it stands. Bodies against bodies are the game's: it keeps
each body's shape as a collider (rigid to block, soft to nudge) and steps
its bodies in ascending id, each against the others as they stand. A
trigger is a sensor: `sensors_at` lists the sensors a body is in, and
`sensor_events(&before, &after)` the ones it entered and left, from lists
the game keeps, so they roll back with its state.

```rust
use sgl_core::math::Vec2;
use sgl_physics::{
    ALL_LAYERS, Collider, ColliderSet, MoverConfig, MoverOptions, MoverSettings, MoverState,
    Push, Shape, move_and_slide, sensor_events, sensors_at,
};

let dt = 1.0 / 60.0;
let config = MoverConfig::new(MoverSettings {
    up: Vec2::Y,
    skin: 0.01,
    snap_distance: 0.1,
    normal_threshold: 0.7,
    block_epsilon: 1e-4,
});
let body = Shape::Box { half: Vec2::new(0.4, 0.5) };
let mut set = ColliderSet::new(2.0);
set.insert(Collider::new(1, Shape::Box { half: Vec2::new(8.0, 0.5) }, Vec2::new(0.0, -0.5)));
let mut lift = Collider::new(5, Shape::Box { half: Vec2::new(1.0, 0.25) }, Vec2::new(3.0, 0.25))
    .with_velocity(Vec2::new(0.0, 2.0));
set.insert(lift);
// Each player's body, by id, and the sensors it was in last step.
let mut players = [(10u64, MoverState::default()), (11, MoverState::default())];
players[0].1.position = Vec2::new(-2.0, 0.505);
players[1].1.position = Vec2::new(3.0, 1.005);
let mut sensors: [Vec<u64>; 2] = Default::default();
for (id, state) in &players {
    set.insert(Collider::new(*id, body, state.position).with_push(Push::Soft(0.05)));
}

// One fixed step: move the colliders first...
lift.position += lift.velocity * dt;
set.set_position(lift.id, lift.position);
set.set_velocity(lift.id, lift.velocity);
// ...then step the bodies in ascending id, each against the others.
for ((id, state), was_in) in players.iter_mut().zip(&mut sensors) {
    set.remove(*id);
    state.velocity.y -= 20.0 * dt;
    let contacts =
        move_and_slide(state, &body, dt, &set, &config, &MoverOptions::default())
            .expect("a set always answers");
    if contacts.crushed {
        // The game's rule: a crushed player loses a life.
    }
    let now = sensors_at(&body, state.position, &set, ALL_LAYERS).expect("answers");
    let (entered, left) = sensor_events(was_in, &now);
    let _ = (entered, left); // pickups, checkpoints, damage zones
    *was_in = now;
    set.insert(
        Collider::new(*id, body, state.position)
            .with_velocity(state.velocity)
            .with_push(Push::Soft(0.05)),
    );
}
```

See [physics.md](../../specs/physics.md), requirement 21, for every rule.

## Picking

`ray` asks what stops a point, so it passes what the point only touches: a
ray lying in a face, a tangent, and a collider it starts inside. A pick
wants all of those. `ray_spans` lists every collider on the masked layers
whose closed solid the ray touches, its surface included, nearest first,
each as a `RaySpan`: the fractions of the motion where the ray enters and
leaves it, and the outward normal crossed at each.

```rust
use sgl_core::math::Vec3;
use sgl_physics::{ALL_LAYERS, Collider, Shape, ray_spans};

// A target ball, aimed at from inside it.
let targets = [Collider::new(4, Shape::Ball { radius: 2.0 }, Vec3::ZERO)];
let over = ray_spans(Vec3::ZERO, Vec3::X * 8.0, &targets[..], ALL_LAYERS).unwrap();
// It starts inside (no surface crossed coming in) and leaves a quarter along.
assert_eq!((over[0].enter, over[0].enter_normal), (0.0, None));
assert_eq!((over[0].exit, over[0].exit_normal), (0.25, Some(Vec3::X)));
```

Nothing is grown to make a touch count: a ray beside a solid is not over
it, however near. The rules that differ between games stay the game's. A
ray that starts in a collider has `enter == 0.0` and no `enter_normal`:
take it as the pick, or skip to the next span. A ray lying in the face two
voxels share is over both, in id order: choose by the game's own rule.
Colliders of every kind are listed, sensors included, so a pick volume
need not block anything; pick among them by layer. It reads a box, a ramp,
a ball, a capsule and a 2D polygon, through every world. A 3D convex shape
(a triangle of a surface, a hull) is never listed: `ray` meets those. See
[physics.md](../../specs/physics.md), requirement 15.

## Round and convex shapes

Box against box, whole or cut, is a closed-form sweep and exact, and so
is a `ray` against a ball, a capsule, a 2D polygon or a 3D triangle (a
convex shape of exactly three points): it meets the surface where it is,
to a few units in the last place of the coordinates involved, as Box2D's
and Box3D's ray casts do. Every other pair, a 3D hull of four or more
points met by a ray included, goes through a convex kernel ported from
Box2D and Box3D ([provenance](PROVENANCE.md)), which makes contact a
*slop* short of touching: a cast stops between one and one and a quarter
slops before the surfaces meet. A query's slop is a hundredth of the smaller shape's extent
(at most a quarter of the smaller radius); the mover's is a sixteenth of
`skin`, and it reports the contact where the body would touch, so after
the skin back-off a round or convex body rests between `skin` and
`skin * 65 / 64` from a flat face's plane, as a box rests `skin` from it. Its
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
decided within a slop of the shapes' size, as for every kernel pair. Two
balls at one point are pushed apart along x. A body whose own step `f32`
cannot hold (a speed or spin near its limit) is left as it was for that
step, and never makes another body non-finite. See
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

## Surfaces

Two 3D worlds hold geometry a game gives once, for queries: walking on
them is not offered, and the mover's floors are still authored as boxes,
ramps and convex shapes. The mover accepts a surface world and meets each
triangle as a solid convex collider, returning finite state from every
step, but a body can catch on a seam between two flat triangles and stop,
and one whose centre is below a triangle is pushed out beneath it.

- `TriangleSet::new(positions, indices, ids)` takes triangles, three
  indices each, with an id per triangle or one for all (the surface kind a
  query reports). It refuses non-finite positions, indices that are not
  triples or name no position, and more than `MAX_TRIANGLES` (2^20)
  triangles (`SurfaceError`); a triangle with no area, or too large for
  its edges' products to fit `f32`, is dropped and counted
  (`degenerate()`). It builds a tree once (Box3D's median split,
  never deeper than `MAX_TREE_DEPTH`, 64) and offers one query at most
  `MAX_QUERY_TRIANGLES` (4096) triangles, past which the query is
  `Unanswered`; a long ray visits only the triangles along it.
- `HeightField::new(heights, columns, rows, cell, origin, ids)` is a grid
  of `columns` by `rows` samples on the xz plane, y up, sample `(column,
  row)` at `origin + (column * cell, heights[row * columns + column], row *
  cell)`, each cell two triangles split from its `(column + 1, row)` corner
  to its `(column, row + 1)` corner, with an id per cell or one for all, at
  most `MAX_HEIGHT_SAMPLES` (2^22). A query walks its cells as a
  `CellWorld` does, at most `MAX_QUERY_CELLS`; outside the grid there is
  no ground.
- `ground_under(point, max_height, depth, &world, mask)` is the highest
  surface straight down (`-y`) through `point`, from `max_height` above it
  to `depth` below it, with its height, its normal (pointing up) and its id,
  so a deck over a vehicle is not taken for the floor under it. It asks any
  `World<Vec3>`.

`ray` and `cast` meet both worlds as they meet any other: a chase camera
casts a `Shape::Ball` from the vehicle toward where it wants to be and
stops where the cast does; a sight line is a `ray`. Each triangle is a
convex collider of its three corners, two-sided. A ray or `ground_under`
meets it exactly: the height is the surface's own, to a few units in the
last place of the coordinates involved (micrometres on a 20 m triangle),
and the normal is the triangle's own plane normal, turned toward the ray,
so coplanar neighbours give the same normal across their seam and a
wheel neither floats nor rocks there. A ray passing within rounding of a
shared edge meets one of the triangles; seams have no gaps. A ray moving in
a triangle's plane meets nothing. A ball cast
meets a triangle through the kernel, a slop short of it, as every kernel
pair does.

`sgl-3d` loads a model's positions and indices (`CpuMesh`); a game copies
them into a `TriangleSet` itself, in world units after its own instance
transform. Neither crate depends on the other, and the GPU is never read.

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
`Contacts::<Vec3>::MAX_POSTCARD_BYTES`, 125), to budget against `sgl-net`'s
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
[moving-collider tests](../../tests/sgl-physics/tests/moving.rs) and
[scenarios](../../tests/sgl-physics/tests/scenarios_moving.rs), the
[round-body tests](../../tests/sgl-physics/tests/round.rs), the
[planar tests](../../tests/sgl-physics/tests/planar.rs), the
[surface tests](../../tests/sgl-physics/tests/surfaces.rs) and
[beside moving colliders](../../tests/sgl-physics/tests/surfaces_moving.rs), and the
comparisons with parry ([kernel](../../tests/sgl-physics/tests/oracle_parry.rs),
[planar](../../tests/sgl-physics/tests/oracle_parry_planar.rs),
[surfaces](../../tests/sgl-physics/tests/oracle_parry_surfaces.rs))
exercise the cross-target, geometric, serialisation and mover boundaries. Run
`cargo test -p sgl-physics -p sgl-physics-tests` for focused native checks.
See [Physics](../../docs/README.md#physics) to choose a game's physics.
