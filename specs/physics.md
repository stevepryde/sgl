# Physics

`sgl-physics` is SGL's kinematic physics for 2D and 3D games
([D-94](decisions.md)): bodies move because the game says so, and SGL finds
what they hit and where they end up. It has no rigid bodies, forces, joints
or solver, holds no loop, and keeps no state the game does not own. A game
whose play is stacking, joints or rag dolls adds Rapier itself.

This slice gives box bodies against box colliders, whole or cut by one
plane into ramps (#915). Later slices (the mover step, round bodies, moving
colliders, planar bumps, surfaces) are planned in #914 and are not available
until they land.

## Requirements

1. **Boundary.** `sgl-physics` depends on `sgl-core` and no other SGL crate,
   with an optional `serde` feature. It has no I/O, clocks or threads, runs
   on the CPU, and builds the same for native, `wasm32` and a headless
   server, without wgpu or winit.
2. **Pure and plain.** Every function's result depends only on its arguments
   and on what the world answers. Every value a game keeps between steps is
   plain data the game owns: `Copy`, with no handle into SGL and no cache
   behind it. Restoring an earlier value and repeating the same calls
   repeats the same results.
3. **Near is as good as far.** A query is computed relative to the body, so
   its accuracy depends on the sizes of the shapes and the distance between
   them, not on how far they are from the origin (a driving game's route runs
   for kilometres).
4. **The same on every target.** The crate uses only IEEE-754 add, subtract,
   multiply, divide, square root, comparison, minimum, maximum and floor, in
   a fixed order, with no fused multiply-add and no platform maths function.
   Iterative routines run a fixed number of steps or stop on a comparison of
   such values. Equal inputs give bit-equal results on native and `wasm32`.
5. **Ready for the network.** State types serialise with the `serde` feature
   and each documents its largest postcard size (`MAX_POSTCARD_BYTES`), so a
   game can see what fits `sgl-net`'s `MAX_LATEST_STATE_BYTES`. They
   implement `CanonicalWrite`, so a client and a server compare one digest to
   find a desync. Nothing in the crate knows about `sgl-net`. A
   `MoverConfig` serialises as the `MoverSettings` it was built from and is
   validated again, to the same bits, when deserialised.
6. **Order never shows.** A result never depends on the order a world visits
   colliders in, or on the order a set was built in: contacts at the same
   instant resolve to the lowest id, then the lowest position, compared
   component by component (then the normal, the shape and the cut, so the
   order is total). A list of colliders is in a fixed order (id, position,
   shape, kind, layers, cut) and holds each collider once, however many
   times a world visited it. A set is derived data: a game may keep it or rebuild it from
   its own state at any step and get the same results.
7. **One implementation for each pair of shapes,** and one mover for every
   body shape, generic over the sealed `Vector` trait for `Vec2` and `Vec3`:
   no 2D-only or 3D-only copy of an algorithm. Box against box, whole or
   cut, is the closed-form sweep and is exact. For other pairs a query's
   tolerance scales with the sizes of the two shapes, and the mover's
   tolerances derive from the game's `skin`; never a fixed length. `cast`,
   `ray` and `overlaps` take no length. A 3D query with no motion along z, among
   colliders that span the body on z, gives bit-equal x and y to the 2D
   query.
8. **Bounded work.** No query walks a range the caller's data can make
   unbounded; a query over a cap answers `Unanswered` and the caller moves
   nothing.
9. **Colliders.** A `Collider` is a `Shape` (`Box { half }` in this slice)
   at a position (its centre), a `ColliderKind` (`Solid`, `OneWay` or
   `Sensor`), `layers: u32`, `id: u64` and an optional `Cut` (16, Ramps).
   The id is the game's: SGL carries it into results and uses it to break
   ties, and hands out no handles.
   Within one `ColliderSet` an id is unique, and inserting an id already
   present replaces that collider; in a slice, a cell world or a pair, ids
   may repeat. A query takes a mask and meets a collider when
   `layers & mask != 0`. A sensor never blocks; a one-way platform blocks
   only motion into a floor-classified face (one within `normal_threshold`
   of the configured up axis). A collider with a negative half-extent, or a
   position or half-extent that is not finite, occupies nothing: every query
   skips it, through every world. So does a cut collider whose cut leaves
   nothing of its box, and a one-way platform with a cut.
10. **Worlds.** A query reaches colliders only through the `World` trait.
    `colliders_near` visits every collider that may overlap a closed region;
    `colliders_along`, which the moving queries (`cast`, `ray`,
    `slide`, `snap_to_ground`) use, visits every collider that may
    overlap a closed box swept by a motion, and defaults to
    `colliders_near` over the box enclosing the start and the end. Either
    may visit more, in any order and more than once, or answer
    `Unanswered`. A world offers every collider whose box, grown on each
    axis by the collider's rounding allowance `4ε(|position| + half)` (`ε`
    is `f32::EPSILON`), meets the region: a face computed relative to the
    body may lie that far from where it lies in world coordinates. It is
    implemented for a slice (every collider), `ColliderSet`, `CellWorld`, a
    pair `(A, B)` (both; unanswered when either is) and references to them.
    - `ColliderSet` buckets colliders in a uniform grid of the game's cell
      size. An insert buckets at most a fixed number of cells (1024) and
      keeps a larger collider apart; a query visits at most about as many
      cells as the set holds colliders, plus the oversized colliders. It
      buckets each collider by its box grown by its rounding allowance. An
      inverted or non-finite box addresses no cells. It never refuses a
      query.
    - `CellWorld` is a tile or voxel grid the game answers cell by cell: a
      cell size and a function from an integer cell to that cell's
      colliders in cell-local units (the cell runs from 0 to 1), converted
      by SGL as `(cell + local) * cell_size`. A cell's colliders lie within
      it, so it grows the region by `8ε(largest coordinate + cell_size)`,
      which covers their rounding allowance, and walks every cell the grown
      region meets, a cell whose upper face the region starts on included.
      Along a motion it cuts the motion into as many equal steps as it
      crosses cells along its longest axis and walks, step by step, the
      cells each step's swept box meets that the step before did not, each
      cell once. It walks at most `MAX_QUERY_CELLS` (4096) cells for one
      query, counting only the cells it walks; past that, or past the `i32`
      grid, it answers `Unanswered` without asking about any cell. An
      unloaded cell is the game's policy: it emits a full box or nothing.
    - A query pads its region by a rounding allowance that scales with its
      own coordinates (`8ε` of the largest), and a world covers the
      collider's, so a collider reached exactly at the end of a motion is
      offered by any world. Padding only adds candidates.
11. **Sweep.** The box sweep is a closed-form slab test in the body's frame,
    returning `t ∈ [0, 1]` and a unit normal pointing from the surface
    toward the body, from the last axis to enter: axis-aligned, or a cut's
    normal. Boxes that touch enter at exactly `t = 0`, read from the same
    edges as the overlap test, and touching is not overlap. An exact tie between entering axes
    resolves, for `slide` and `snap_to_ground`, to the axis most
    along `up` (on equal lengths y, then x, then z), then the others y, x,
    z; `cast` and `ray`, which have no up, resolve it y, then x, then z. A
    cut's axis comes after every cardinal axis: it decides only when it
    enters strictly last.
    Already overlapping, moving away, and zero or non-finite motion give no
    contact. A box with no extent sweeps as a ray.
12. **Mover settings.** `MoverConfig` is constructed only through `new`,
    from a `MoverSettings` whose public fields name each value; its own
    fields are private with read-only accessors, so callers cannot bypass
    validation or change the normalised up axis. `new` refuses an
    `up` whose length is not finite and normalisable (including finite
    components whose length overflows), a `normal_threshold` outside
    `(0, 1)`, and a negative or non-finite `skin`, `snap_distance` or
    `block_epsilon`. It names the up axis, skin, snap distance, floor
    threshold and blocked tolerance in the game's units, with no defaults.
    Results mirror exactly between y-up and y-down worlds.
13. **Move and slide.** `slide` slides a body against a world,
    sweeps every finite nonzero remainder without an absolute motion cutoff,
    slides at most a fixed number of times (`MAX_SLIDES_2D`, 3: a floor, a
    ramp and a wall; `MAX_SLIDES_3D`, 4: a floor, a ramp and the two walls
    of a corner; raised by one each for ramps, #915), and never moves the
    body more than a rounding step into a solid collider. A body that starts inside one
    may move out or along it, and its least penetration depth never grows. It
    does not push bodies out: the game keeps them clear. Skin back-off
    shortens the whole swept motion to leave the configured gap along the
    contact normal, never reversing it or moving off the tested segment.
    After a wall or ceiling contact, sliding projects the motion left after
    the contact time, excluding the backed-off portion; after a floor
    contact, it turns everything the pass did not apply along the face (16,
    Ramps). `snap_to_ground` probes `snap_distance` along `-up`, grounds
    only on a floor-classified face met first (a steeper face met first
    leaves the body where it is), and backs off along its probe. A
    one-way platform blocks a body within `skin` (plus rounding) below its
    floor face, never a body deeper inside it. A non-finite motion moves
    nothing.
14. **Contacts.** `Contacts` records the floor, wall and ceiling contacts of
    a move, the last floor and ceiling normals, the ids of the last floor,
    wall and ceiling met, the motion asked for and applied, and whether
    motion against or along `up` was asked for and not achieved by more
    than `block_epsilon`.
15. **Queries.** `overlaps` lists every collider on the masked layers the
    body's shape overlaps, sensors included, in the fixed order, each once.
    A cut collider is met as the shape it is: a ray, a cast or an overlap
    passes through the part its cut removed.
    `cast` reports the first non-sensor collider a moving shape meets (`t`,
    normal and id), meeting a one-way platform as a whole box. A body
    overlapping a collider by no more than the collider's rounding allowance
    on one axis, moving into that face, meets it at `t = 0`: in world
    coordinates it touches. A collider the body overlaps more deeply at the
    start is passed. `ray` is a cast of a box with no extent.
16. **Ramps.** A collider may be cut by one plane through its box; its solid
    part is the box on the plane's inner side. In 3D the plane's normal has
    one zero component. The sweep against a cut box is exact, and a contact
    on the cut reports the plane's normal.
    - `Cut` is a unit `normal` and an `offset` from the box's centre: the
      solid part is the box where `normal · (p − position) ≤ offset`. Its
      constructor normalises the normal (the ramp's only square root) and
      refuses a zero or non-finite normal, a non-finite offset, and, in 3D,
      a normal with no zero component; `Collider::with_cut` and
      `with_kind` refuse a one-way platform with a cut. A cut serialises as
      the normal it was built from and its offset, and is validated again,
      to the same bits, when deserialised. A `CellWorld` scales a cut's
      offset with its box; every world offers a cut collider by its box.
    - The sweep is the slab test on the cardinal axes, by the solid part's
      extent along each, and on the cut's normal, by the plane: a box and a
      cut box are separated on one of those axes exactly when the normal
      has a zero component in 3D. A body that overlaps a cut box may leave
      it or move along it, never deeper, its axis of least penetration
      chosen among those axes.
    - Motion left after a contact with a floor-classified face turns along
      that face and keeps its horizontal length, so a walkable slope does
      not slow a body. Motion left after any other contact is projected onto
      the face, as before, except that a face tilted toward `up` and too
      steep to stand on gives it no height it did not already have: a body
      pushed at the foot of such a slope stops there and holds still, and
      one falling onto it slides down it. A slide never moves a body into a
      face it has
      already touched in the same move: between two such faces the
      remaining motion follows their crease, and among three it stops.
    - "Motion left" after a floor is everything the pass did not apply,
      the skin back-off included, so a body walking or landing on any floor
      covers exactly its motion across `up` when nothing else stops it. Its
      part across `up` is kept whole, direction included, and only its part
      along `up` changes to keep it on the face. In 2D two faces that are not
      parallel meet at a point, so their crease stops the motion; faces
      parallel to one already met add no constraint.

## Acceptance

- The frozen fixtures in `tests/sgl-physics/tests/parity.rs`, a seeded walk
  through a fixed 2D and a fixed 3D level hashed every step, produce
  identical values on native and `wasm32`.
- The property tests in `tests/sgl-physics/tests/properties.rs` hold: a move
  never ends inside a solid through any world; the same level as a slice, a
  set, a cell world or sets built in two orders, large colliders and motions
  ending exactly on their faces included, moves bit-equal; a long cast
  through a cell world matches the slice; a scene translated by 2^17 moves
  bit-equal; a cast from exactly on a face meets it; a 3D move with no z
  motion matches the 2D move bit for bit.
- A body state serialised with postcard and restored continues bit-equal to
  one that was not (`tests/sgl-physics/tests/state.rs`).
- The ramp tests in `tests/sgl-physics/tests/ramps.rs` hold: hand-worked
  sweeps against a 2D ramp and each kind of 3D ramp; a move never ends
  inside a cut box, by the box's own definition sampled; walking over ramps
  covers the same distance per step as flat ground and stays grounded down
  them; a ramp too steep to stand on holds a body still at its foot and
  gives a level push no height; a body in the
  crease of a ramp and a wall penetrates neither, and one wedged in a crease
  follows it; ramps mirror across each axis and between y-up and y-down;
  and a cell world of ramps moves bit-equal to the slice.
