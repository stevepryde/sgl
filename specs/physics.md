# Physics

`sgl-physics` is SGL's kinematic physics for 2D and 3D games
([D-94](decisions.md)): bodies move because the game says so, and SGL finds
what they hit and where they end up. It has no rigid bodies, forces, joints
or solver, holds no loop, and keeps no state the game does not own. A game
whose play is stacking, joints or rag dolls adds Rapier itself.

This slice gives box bodies against box colliders, whole or cut by one
plane into ramps (#915), the mover step (#916), and round bodies (balls and
capsules) and convex colliders met through a convex kernel ported from
Box2D and Box3D (#919, 19). Later slices (moving colliders, planar bumps,
surfaces) are planned in #914 and are not available until they land.

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
   cut, is the closed-form sweep and is exact. Every other pair goes
   through the convex kernel (19), on one dispatch on the pair, in
   `sweep.rs`; nothing else in the crate assumes a body is a box beyond the
   bounding half-extents the worlds use. For those pairs a query's
   tolerance scales with the sizes of the two shapes, and the mover's
   tolerances derive from the game's `skin`; never a fixed length. `cast`,
   `ray`, `overlaps` and `distance` take no length. Among box pairs, a 3D
   query with no motion along z, among colliders that span the body on z,
   gives bit-equal x and y to the 2D query; the kernel's two dimensions are
   different solvers, so a pair through it agrees between them only to its
   tolerance.
8. **Bounded work.** No query walks a range the caller's data can make
   unbounded; a query over a cap answers `Unanswered` and the caller moves
   nothing.
9. **Colliders.** A `Collider` is a `Shape` (`Box { half }`, `Ball {
   radius }`, `Capsule { half_axis, radius }` or `Convex`, 19) at a
   position (its centre), a `ColliderKind` (`Solid`, `OneWay` or
   `Sensor`), `layers: u32`, `id: u64` and an optional `Cut` (16, Ramps).
   The id is the game's: SGL carries it into results and uses it to break
   ties, and hands out no handles.
   Within one `ColliderSet` an id is unique, and inserting an id already
   present replaces that collider; in a slice, a cell world or a pair, ids
   may repeat. A query takes a mask and meets a collider when
   `layers & mask != 0`. A sensor never blocks; a one-way platform blocks
   only motion into a floor-classified face (one within `normal_threshold`
   of the configured up axis). A collider with a negative half-extent or
   radius, or a position, half-extent, radius or axis that is not finite,
   occupies nothing: every query skips it, through every world. So does a
   cut collider whose cut leaves nothing of its box, a cut on a shape that
   is not a box, and a one-way platform that is not a whole box (cut, or
   not a box); `Collider::with_cut` and `with_kind` refuse to build them.
10. **Worlds.** A query reaches colliders only through the `World` trait.
    `colliders_near` visits every collider that may overlap a closed region;
    `colliders_along`, which the moving queries (`cast`, `ray`,
    `slide`, `snap_to_ground`, `move_and_collide`) use, visits every collider that may
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
    - Worlds find every shape by its bounding half-extents, centred on its
      position: a ball's radius, a capsule's axis plus radius on each axis,
      a convex shape's largest coordinate on each axis. A kernel pair makes
      contact a slop apart (19), so the kernel could find one with a
      collider just outside the region a world offers. A query therefore
      meets a collider through the kernel only when the collider's box,
      grown by its rounding allowance, meets the body's box swept by the
      motion (or, for `overlaps`, its box) padded by half the query's pad:
      a test every world's offer covers, so the result never depends on the
      world, and the allowances above need not grow. A collider it leaves
      out the body never reaches; it may end within the slop of one without
      contact.
11. **Sweep.** Between boxes, the sweep is a closed-form slab test in the
    body's frame, returning `t ∈ [0, 1]` and a unit normal pointing from the surface
    toward the body, from the last axis to enter: axis-aligned, or a cut's
    normal. Boxes that touch enter at exactly `t = 0`, read from the same
    edges as the overlap test, and touching is not overlap. An exact tie between entering axes
    resolves, for `slide` and `snap_to_ground`, to the axis most
    along `up` (on equal lengths y, then x, then z), then the others y, x,
    z; `cast` and `ray`, which have no up, resolve it y, then x, then z. A
    cut's axis comes after every cardinal axis: it decides only when it
    enters strictly last.
    Already overlapping, moving away, and zero or non-finite motion give no
    contact. A box with no extent sweeps as a ray. Every other pair is
    swept by the kernel (19).
12. **Mover settings.** `MoverConfig` is constructed only through `new`,
    from a `MoverSettings` whose public fields name each value; its own
    fields are private with read-only accessors, so callers cannot bypass
    validation or change the normalised up axis. `new` refuses an
    `up` that is zero or not finite, a `normal_threshold` outside
    `(0, 1)`, and a negative or non-finite `skin`, `snap_distance` or
    `block_epsilon`. Every other `up` is a direction, however short or
    long: the crate normalises every direction (`up`, a cut's normal, a
    convex shape's rotation, a separation's normal) by one routine that
    divides by the largest absolute component before squaring, so neither
    underflow nor overflow occurs and the result is unit to within a few
    units in the last place. It names the up axis, skin, snap distance, floor
    threshold and blocked tolerance in the game's units, with no defaults.
    Results mirror exactly between y-up and y-down worlds.
    A stated limit: `skin` must be larger than the spacing of `f32` values
    at the body's distance from the origin (about the coordinate times
    2^-23); below that a body can end a rounding step or two inside a
    solid. At 65,536 units, where the spacing is 1/128, a skin of 0.01 is
    too small.
13. **Move and slide.** `slide` slides a body against a world,
    sweeps every finite nonzero remainder without an absolute motion cutoff,
    slides at most a fixed number of times (`MAX_SLIDES_2D`, 3: a floor, a
    ramp and a wall; `MAX_SLIDES_3D`, 4: a floor, a ramp and the two walls
    of a corner; raised by one each for ramps, #915), and never moves the
    body more than a rounding step into a solid collider. A body that starts inside one
    may move out or along it, and its least penetration depth never grows.
    A round or convex body inside a collider through the kernel may move
    where a way out is known and is otherwise not moved (19).
    `slide` does not push bodies out; `move_and_slide` does (17). Skin back-off
    shortens the whole swept motion to leave the configured gap along the
    contact normal, never reversing it or moving off the tested segment.
    The motion left after a contact is everything the pass did not apply,
    the skin back-off included: after a wall or ceiling contact, sliding
    projects it onto the face, removing only its part into the face, so a
    body pushed into a wall keeps its whole fall and its whole run along
    the wall; after a floor contact, it turns it along the face (16,
    Ramps). `snap_to_ground` probes `snap_distance` along `-up`, grounds
    only on a floor-classified face met first (a steeper face met first
    leaves the body where it is), and backs off along its probe. A
    one-way platform blocks a body within `skin` (plus rounding) below its
    floor face, never a body deeper inside it. A motion, position or body
    size that is not finite moves nothing and reports no contact, and so
    does a finite position and motion whose sum `f32` cannot hold (a
    `snap_to_ground` probe included): a finite body never moves to an
    infinite place.
14. **Contacts.** `Contacts` records the floor, wall and ceiling contacts of
    a move, the last floor, ceiling and wall normals, the ids of the last
    floor, wall and ceiling met, the motion asked for and applied, and
    whether motion against or along `up` was asked for and not achieved by
    more than `block_epsilon`. The motion applied is measured relative to
    the body, as the sum of the translations the move applied, never as a
    difference of absolute positions, so `effective` and the blocked flags
    read the same near the origin and far from it (3). A `move_and_slide` step also reports whether
    it climbed a step (`stepped`), its ledge guard shortened the move
    (`ledge_guarded`), it pushed the body out (`pushed_out`) or the body is
    `stuck`. The last-contact fields report; the mover's velocity rule
    reads every face met (17), not them.
15. **Queries.** `overlaps` lists every collider on the masked layers the
    body's shape overlaps, sensors included, in the fixed order, each once.
    A cut collider is met as the shape it is: a ray, a cast or an overlap
    passes through the part its cut removed. Touching is not overlap for
    box pairs and for shapes with a radius; two radius-less shapes met
    through the kernel that touch exactly may read either way.
    `cast` reports the first non-sensor collider a moving shape meets (`t`,
    normal and id), meeting a one-way platform as a whole box. A body
    overlapping a collider by no more than the collider's rounding allowance
    on one axis, moving into that face, meets it at `t = 0`: in world
    coordinates it touches; through the kernel, by no more than its slop. A
    collider the body overlaps more deeply at the start is passed. For a
    kernel pair `t` is where the shapes come within the contact band (19),
    before they touch. `ray` is a cast of a box with no extent.
    `distance` measures a shape at a position against one collider, of any
    kind or layer: the gap between their surfaces and the unit normal from
    the collider toward the shape, zero and no normal when they touch or
    overlap, and `None` when the collider occupies nothing or the shape or
    position is not sound. Two whole boxes are measured in closed form,
    every other pair by the kernel's GJK, a cut box as the solid part its
    plane leaves.
    `move_and_collide` (Godot's name and meaning) moves a shape by a delta
    until the first collider that blocks it as it would block `slide`,
    stops a `skin` short along its path, and returns the translation and
    the contact; it does not slide. A body stopped so and moved along the
    face is not blocked by it. Like `slide`, it moves nothing and reports
    no contact for a motion, position or body size that is not finite, or
    a finite position and motion whose sum `f32` cannot hold.
16. **Ramps.** A collider may be cut by one plane through its box; its solid
    part is the box on the plane's inner side. In 3D the plane's normal has
    one zero component. The sweep against a cut box is exact, and a contact
    on the cut reports the plane's normal.
    - `Cut` is a unit `normal` and an `offset` from the box's centre: the
      solid part is the box where `normal · (p − position) ≤ offset`. Its
      constructor normalises the normal (the ramp's only square root) as
      `up` is normalised (12), so a normal however short or long, its
      squared length underflowing or overflowing `f32`, comes out unit, and
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
      the face (13), except that a face tilted toward `up` and too
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
17. **Mover.** `move_and_slide` (Godot's name and meaning) is one fixed
    step of a character over a `MoverState { position, velocity, floor }`
    the game owns: plain `Copy` data, `floor` the normal and id of the
    floor it stands on or `None`, serialised and hashed like the other
    state types. It takes the game's `dt` and `MoverOptions { step_height,
    ledge_drop, drop_through, mask }` per call (lengths finite and
    non-negative, or it panics). A negative or non-finite `dt`, a
    non-finite velocity, a state whose position or body shape is not
    finite, and finite inputs whose sum `f32` cannot hold (a position plus
    the motion a stage would apply) change nothing in the state and report
    no contact. The step keeps what it applies as a translation from where
    the body started: each stage starts from the translation so far, never
    from a rounded absolute position, `effective` and the blocked flags
    read it (14), and the position written back is the start plus it. In
    order, it:
    - pushes a body that starts inside a solid collider out the shortest
      way that frees it, at most its own size along each axis: the body's
      full extent on that axis (twice its half-extent). It tries at
      most `MAX_PUSH_OUT_PLANES` (8) distances per axis, each putting the
      body's box a `skin` from a face of a collider's box (a round or
      convex collider offers the faces of the box that encloses it) or
      from a ramp's plane; a ramp's distance on one axis is worked out
      from the distances on the others (one ramp's plane per place), one
      more place moves the body along each ramp's normal a `skin` out of
      its plane, and one more along the normal out of each kernel overlap
      whose cores are apart, a `skin` clear of it (19). When more
      distances than the cap are in reach on an axis, it keeps the nearest
      (staying put, zero, among them), and of two equally near the negative
      one. The nearest free place wins; of two equally near, the one most
      along `up`, then the lowest component by component, x first. Whether
      a place is free is decided by each collider's real shape, cut
      included (`overlaps`). One it cannot free stays untouched and reports
      `stuck`. One-way colliders and sensors never push;
    - moves by `velocity * dt` with one `slide`, whose floor rule (16)
      keeps a walk's motion across `up` whole, on flat floors and ramps; a
      walking body is one on a floor at the start and not rising along
      `up`;
    - climbs a step when a walking body is stopped by a wall and
      `step_height` is not zero: lift by up to `step_height` (a ceiling
      shortens it), the same move across `up`, settle by the lift plus
      `snap_distance`; kept only if it ends on a floor (a ramp's included)
      and gains more than `block_epsilon` of distance along the requested
      direction across `up` over the plain move. A body with a radius (a
      ball, a capsule) has its lowest point a radius behind its leading
      surface, so settling at the step's edge it would meet the edge along
      a normal leaning off the floor and never climb; it moves across
      further by `radius * (1 - sqrt(1 - normal_threshold²)) + 2 * skin`,
      which brings that point far enough past the edge for the edge to be
      a floor under it. A box moves the requested distance only;
    - with `ledge_drop` set, guards a walking body whose move ends with no
      floor within `ledge_drop` or `snap_distance`, whichever is further
      (plus `skin`), below it, so the ground a walker follows down a ramp
      is never a ledge: the move across
      `up` is made again one world axis at a time, each ending at the last
      supported point, found by halving `LEDGE_SEARCH_STEPS` (16) times;
      the body may overhang until its trailing edge reaches the edge, and
      motion along the edge is kept;
    - snaps a walking body to the ground where it ends;
    - removes from `velocity`, for every face the step met in the order
      met, the part that points into it, by the rules `slide` applies to
      the motion it has left (13, 16): a floor removes the part against
      `up`; a wall or a ceiling projects it onto the face; a slope too
      steep to stand on does too and gives it no height; and a later face
      never turns it into an earlier one (a floor counting as `up`), so it
      follows their crease or stops. A body wedged between faces, or
      pinned in a corner, keeps no velocity into any of them;
    - sets `floor`: for a walking body, the floor the snap finds under it
      where it ends, or `None` once it has walked off one, whatever it
      touched on the way; for any other body, the last floor it met, so a
      landing sets it.

    A body standing on a walkable ramp under gravity stays where it is:
    the floor rule takes the fall out of the motion and the velocity
    alike, and leaves no part across `up` to slide down the slope with.
    A resting body stays where it is to within the skin: on its first
    steps it may close up to `skin` of its gap along `-up`, and once a
    step leaves it where it was its position is bit-equal step to step.

    `drop_through` passes every one-way collider for the step. Any
    `Unanswered` query leaves the state exactly as it was and returns
    `Unanswered`. Nothing is cached between calls. Gravity, jumping,
    speeds, flying and water are the game's: how it sets the velocity.
18. **Ledges.** `find_ledge` finds the top edge of the wall a body is
    pressed against (within twice `skin` along a direction across `up`)
    when that edge lies within `reach` of the body's top, above or below,
    and returns where the body would hang (beside the wall, its top level
    with the edge plus `skin`), where it would stand (a `skin` above the
    top and past the edge) and the id of what it would stand on. It
    refuses a ledge with anything solid where the body would hang, beside
    the wall as it rises, or where it would move to stand, and a one-way
    collider's side is never a wall. Hanging and climbing are the game's.
19. **Round and convex shapes.** `Shape::Ball { radius }` (a circle, a
    sphere), `Shape::Capsule { half_axis, radius }` (the segment from
    `-half_axis` to `half_axis` grown by `radius`) and `Shape::Convex`, a
    convex polygon of up to 8 vertices in 2D and the convex hull of up to
    16 points in 3D, are bodies and colliders for every query and for the
    mover. A `Convex` is built by `Convex::polygon(points, cos, sin)` (2D)
    or `Convex::hull(points, quaternion)` (3D) from the game's points about
    the shape's centre and a rotation the game supplies (normalised first,
    as `up` is (12); the crate calls no trigonometric function); it stores the turned
    points inline, so `Shape` stays `Copy`, and refuses non-finite points,
    too many or too few (3 to 8 in 2D, 1 to 16 in 3D), and in 2D vertices
    that are not strictly convex and counter-clockwise. It serialises as
    its turned points and is validated again, to the same bits, when
    deserialised. Its largest postcard size is `Convex::MAX_POSTCARD_BYTES`
    (193 in 3D, 65 in 2D); `Shape::MAX_POSTCARD_BYTES` grows to that plus
    a tag (194, 66) and `Collider::MAX_POSTCARD_BYTES` with it (239, 103).
    - **The kernel.** Every pair but box against box goes through
      `crate::convex`, GJK distance and the conservative-advancement shape
      cast ported from Box2D 3.2 and Box3D 0.1 (`PROVENANCE.md`): each
      shape is a proxy, a few points grown by a radius (a ball one point, a
      capsule two, a box its corners, a cut box the corners and plane
      crossings of its solid part, a convex shape its points), in the
      body's frame.
    - **The slop.** The kernel cannot tell radius-less shapes that touch
      from shapes that overlap a little, so a kernel pair makes contact
      when the shapes come within a small length, the slop, of each other:
      a cast stops the body between `slop` and `1.25 * slop` short of the
      target and never past it (the kernel's target between the cores is
      `max(slop, radii - slop)`, reached within a quarter slop, so the
      crate grows the moving proxy by twice the slop to make it the radii
      plus one slop). A query's slop is a hundredth of the smaller shape's
      extent (its largest half-extent), a contact error no game sees at
      play scale, and at most a quarter of the smaller radius, which keeps
      a contact on a round surface within a third of a radius of where the
      surfaces touch; a shape with no extent (a ray) or no radius does not
      count. The mover's slop (`slide`, `snap_to_ground`,
      `move_and_collide`, `move_and_slide`, `find_ledge`) is a sixteenth of
      `skin`, or the query's when the skin is zero. Either is at least 32
      units in the last place of the largest coordinate the cast meets,
      eight times the rounding of the distance the cast must land within a
      quarter slop of, so `f32` can reach it.
    - **The resting gap.** The mover reports a kernel contact where the
      body would touch: the kernel's fraction plus the slop (no more than
      the skin) over the approach speed along the contact normal. The skin
      back-off then leaves the body between `skin` and `skin + slop / 4`
      (`skin * 65 / 64`) from a flat face, the same as a box to within a
      sixty-fourth of the skin, and at least `skin` from an edge or a
      corner (the distance between convex shapes along a straight motion
      is convex, so stepping back along the motion opens it at least as
      fast as along the normal). With a zero skin it rests between `slop`
      and `1.25 * slop` away. A query (`cast`, `ray`) reports the kernel's
      own fraction, so a projectile stepped to a hit is a slop clear of the
      surface and its next cast meets it at `t = 0` when moving in.
    - **Never a clear path.** A cast that reaches the kernel's iteration
      cap, or whose result `f32` cannot tell from overlap, stops the body
      where it gave up (the shapes are still apart there), against the
      last normal found when the motion approaches it and otherwise against
      the motion itself. A hit whose normal the motion does not approach
      (a graze) is no contact.
    - **Overlapping bodies.** Through the kernel, a body overlaps a
      collider when their cores overlap or their surfaces do by any amount.
      When the cores are apart, the normal between them is a way out. When
      they overlap (a ball whose centre is inside a box), the body stepped
      back along its motion by the slop is tried; if the cores are apart
      there, that normal is the way out, and otherwise none is known.
      `slide`, `snap_to_ground` and `move_and_collide` let an overlapping
      body move along or out of a way out and block motion into it; with
      none known they block every motion, so the body does not move. A
      cast passes a collider the body overlaps by more than the slop
      measured back along its motion. `move_and_slide` pushes a body out
      (17), the kernel's way out among its candidates. Guaranteed: a body
      that starts clear of every collider never ends inside one by more
      than rounding, and one that starts overlapping is never moved deeper
      along a known way out and not moved at all without one. Not
      guaranteed: the kernel measures no depth for overlapping cores, so a
      body deep inside a round or convex collider is freed only by the
      push-out's candidates, and is `stuck` when none frees it.
    - **One-way platforms.** A one-way platform is a whole box. A kernel
      contact with it lies on one or more of its faces; it blocks when its
      normal is a floor and the motion goes into it, or when it lies on a
      floor-classified face (edges and corners included) and the motion
      goes into that face. A ball or capsule meeting the platform's edge
      while falling lands on it, or, where the edge's normal leans past the
      floor threshold, is turned off it as a solid box would turn it, and
      never passes into it; rising through it passes.
    - **Normals.** A kernel contact's normal is the kernel's, not
      axis-aligned at an edge or a corner, so a round body slides around
      an outside corner and over seams between flush colliders; ties
      between contacts at one instant break as for boxes (6).

## Acceptance

- The frozen fixtures in `tests/sgl-physics/tests/parity.rs`, seeded walks
  through a fixed 2D and a fixed 3D level hashed every step, with `slide`
  and with `move_and_slide` (steps, a guarded stretch, drop-through and a
  push-out), for a box body and for a round one (a ball in 2D, a capsule in
  3D, the levels gaining a ball, a capsule and a convex collider), produce
  identical values on native and `wasm32`.
- The property tests in `tests/sgl-physics/tests/properties.rs` hold: a move
  never ends inside a solid through any world; the same level as a slice, a
  set, a cell world or sets built in two orders, large colliders and motions
  ending exactly on their faces included, moves bit-equal; a long cast
  through a cell world matches the slice; a scene translated by 2^17 moves
  bit-equal; a cast from exactly on a face meets it; among boxes, a 3D move
  with no z motion matches the 2D move bit for bit.
- The round-body tests in `tests/sgl-physics/tests/round.rs` hold: every
  new pair, in each dimension, meets where hand-worked geometry puts it
  within the contact band, and the mover rests within its stated gap;
  `overlaps` and `distance` read the shapes themselves; random balls and
  capsules never end inside a box, slab, ramp, ball, capsule or convex
  collider, by the shapes' own definitions sampled, and move bit-equal
  through a slice, a set and a cell world; a ball crosses flush tiles and
  voxel seams as it crosses one long box, and goes around an outside
  corner keeping its speed; a ball lands on a one-way platform's edges and
  rises through it; a capsule walker lands, crosses ramps at its
  flat-ground speed, climbs a step, stops at a guarded ledge and is pushed
  out of a crate; beams and swept balls stop at the first thing on their
  layers; a projectile stepped to each hit never passes a wall.
- The kernel agrees with a second engine, parry, over thousands of seeded
  pairs in each dimension (`tests/sgl-physics/tests/oracle_parry.rs`,
  native only).
- A body state serialised with postcard and restored continues bit-equal to
  one that was not, and a `MoverState` restored and replayed through the
  same inputs is bit-equal to the forward run
  (`tests/sgl-physics/tests/state.rs`).
- The ramp tests in `tests/sgl-physics/tests/ramps.rs` hold: hand-worked
  sweeps against a 2D ramp and each kind of 3D ramp; a move never ends
  inside a cut box, by the box's own definition sampled; walking over ramps
  covers the same distance per step as flat ground and stays grounded down
  them; a ramp too steep to stand on holds a body still at its foot and
  gives a level push no height; a body in the
  crease of a ramp and a wall penetrates neither, and one wedged in a crease
  follows it; ramps mirror across each axis and between y-up and y-down;
  and a cell world of ramps moves bit-equal to the slice.
- The mover's requirements hold in 2D and 3D
  (`tests/sgl-physics/tests/mover.rs`), and so do the game scenarios of a
  voxel walker through a `CellWorld`, on ramps included, and a 2D platformer
  (`tests/sgl-physics/tests/scenarios.rs`).
