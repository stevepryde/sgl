# Provenance and algorithm notes

`sgl-physics` is SGL's own, except its convex kernel (`src/convex.rs` and
`src/convex/`), the contact manifold and bump of its planar bodies
(`src/planar/collide.rs`, `src/planar/collide/`, `src/planar/bump.rs`) and
the plane solver of soft contact (`src/plane.rs`), which are ported from two
projects by Erin Catto, both MIT:

| Source | Tag | Commit | Licence here |
| --- | --- | --- | --- |
| [Box2D](https://github.com/erincatto/box2d) | `v3.2.0` | `178495ba35695aefe9c321e569d2277f06db3b90` | `LICENSE-box2d.txt` |
| [Box3D](https://github.com/erincatto/box3d) | `v0.1.0` | `8441b4a06d6d09dcfb0b0f704df4d847d1437b92` | `LICENSE-box3d.txt` |

Box3D's `src/distance.c` credits Dirk Gregorius with portions of the file.
The sources are not vendored; the headers of the ported files name the
functions each one follows.

| Here | From |
| --- | --- |
| `convex.rs`: `Proxy`, the result types, `cast_by_advancement` | Box2D `b2ShapeProxy`, `b2MakeProxy`, `b2DistanceOutput`, `b2CastOutput`, `b2CastShape`; Box3D `b3ShapeProxy`, `b3DistanceOutput`, `b3CastOutput`, `b3ShapeCast` |
| `convex/d2.rs`: `Proxy::offset`, `distance`, the simplex solvers, `shape_cast` | Box2D `src/distance.c`: `b2MakeOffsetProxy`, `b2ComputeShapeDistance`, `b2SolveSimplex2`, `b2SolveSimplex3`, `b2FindSupport`, `b2ComputeWitnessPoints`; `include/box2d/collision.h`, `include/box2d/math_functions.h` |
| `convex/d2.rs`: `Rot::integrated` | Box2D `include/box2d/math_functions.h`: `b2IntegrateRotation` |
| `convex/d2/toi.rs`: `Sweep`, `time_of_impact` | Box2D `src/distance.c`: `b2GetSweepTransform`, `b2MakeSeparationFunction`, `b2FindMinSeparation`, `b2EvaluateSeparation`, `b2ComputeTimeOfImpact` |
| `planar/collide.rs`: `Polygon`, the circle and capsule manifolds | Box2D `src/manifold.c`: `b2MakeCapsule`, `b2CollideCircles`, `b2CollideCapsuleAndCircle`, `b2CollidePolygonAndCircle`; `src/geometry.c`: `b2MakeBox`, the normals of `b2MakePolygon`; `include/box2d/collision.h`: `b2Polygon`, `b2ManifoldPoint`, `b2LocalManifold`; `include/box2d/math_functions.h` |
| `planar/collide/polygons.rs`: the polygon manifold | Box2D `src/manifold.c`: `b2ClipPolygons`, `b2FindMaxSeparation` (its scalar path), `b2CollidePolygons`, which `b2CollidePolygonAndCapsule` calls; `src/distance.c`: `b2ComputeSegmentDistance` |
| `planar/manifold.rs`: which shape is A | Box2D `src/contact.c`: the primary order of `b2_contactRegistry` |
| `planar/bump.rs`: `impulse` | Box2D `src/contact_solver.c`: the normal and friction rows of `b2PrepareContacts_Overflow` and `b2SolveContacts_Overflow`, and the velocity target of `b2ApplyRestitution_Overflow`, for one point |
| `convex/d3.rs`: `Rot`, `Proxy::offset`, `distance`, `shape_cast` | Box3D `src/distance.c`: `b3GetProxySupport`, `b3ShapeDistance`; `b3MakeMatrixFromQuat`, `b3MulMV`, `b3Normalize` of `include/box3d/math_functions.h`; `include/box3d/types.h`, `include/box3d/collision.h` |
| `convex/d3/simplex.rs`: the simplex and its solvers | Box3D `src/distance.c`: `b3BarycentricCoordsEdge`, `b3BarycentricCoordsTri`, `b3BarycentricCoordsTet`, `b3SolveSimplex2`, `b3SolveSimplex3`, `b3SolveSimplex4`, `b3ComputeWitnessPoints`; `src/math_internal.h` |
| `plane.rs`: `Plane`, `solve` | Box2D `src/mover.c`: `b2SolvePlanes`; `b2CollisionPlane`, `b2PlaneSolverResult` of `include/box2d/collision.h`; `b2PlaneSeparation`, `b2Dot`, `b2MulAdd`, `b2ClampFloat`, `b2AbsFloat` of `include/box2d/math_functions.h` |

Not ported: Box2D's disabled `b2ShapeCastMerged`, `b2CollideCapsules`,
the segment and chain functions of `src/manifold.c`, the SIMD path of
`b2FindMaxSeparation`, and the rest of its solver (warm starting,
softness, relaxation, rolling resistance); Box3D's `b3TimeOfImpact` and
everything after it; the debug simplex output and the time-of-impact
counters of both.

Also not ported, of Box2D's mover: `b2ClipVector`: the solver serves soft contact only, whose planes
Box2D's own documentation says not to clip a velocity against
(`clipVelocity` false), and the mover clips against rigid faces by its own
rule (`specs/physics.md`, 17); and `b2CollideMover`, which makes the
planes from a capsule: here every shape pair gives its plane through the
crate's own pair dispatch (`sweep::way_apart`).

## Departures of the plane solver

`plane.rs` follows `b2SolvePlanes` operation for operation, in its order,
generic over both dimensions (`b2Dot` summed axis by axis in order).

1. **A gap, not an overlap.** Box2D adds its linear slop to each plane's
   separation, so a mover rests a slop inside what it touches and finds
   the plane again next step. The crate's mover rests a gap from what it
   touches (its `skin`), and a soft contact's bodies must come apart once
   they stop pushing, so the solve subtracts the tolerance: it puts the
   body that far clear of each plane when the limits allow.
2. **The tolerance is the caller's.** The slop, half a centimetre scaled
   by a process-wide setting in Box2D, is the mover's `skin`, in the
   game's units, both for the gap and for the convergence test.
3. **The iteration count is a named cap,** `MAX_PLANE_ITERATIONS` (20, the
   source's literal), and the count used is not returned: it was the
   source's diagnostic.
4. **The planes' order is fixed.** The caller lists the planes in the
   crate's fixed order of colliders (`specs/physics.md`, 6), since a
   Gauss-Seidel pass depends on the order it visits them in.

## Departures

The arithmetic follows the sources operation for operation, in their order.
These are the places the port differs, each with its reason.

1. **Tolerances are the caller's.** The sources take every length tolerance
   from `linearSlop`: half a centimetre, scaled by one process-wide
   setting of how many length units make a metre. This crate keeps no such
   setting and works in each game's own units, pixels in one game and
   metres in another, so `shape_cast` and `time_of_impact` take that length
   as `slop` and derive the same two numbers from it: the target distance
   `max(slop, radii - slop)` and its tolerance `slop / 4`. Two constants
   stay: `1000 * FLT_MIN`, below which a direction cannot be normalised,
   and `100 * FLT_EPSILON`, the test that a normal is unit. Neither is a
   length a game could meet.
2. **No simplex cache.** The sources warm start each distance query from
   the simplex of the last one, kept by the caller between calls, and
   inside a cast or a time of impact between steps. The crate keeps no
   state the game does not own, and inside one call the cache only saves
   iterations, so every query here starts from the first point of each
   proxy. The time of impact still reads which points the final simplex
   used, as the source reads them from the cache, to build its separating
   axis (`Features`). Box3D's cache also compares a simplex's length, area
   or volume with the cached one and with `FLT_EPSILON` to decide whether
   to reuse it; that test goes with the cache.
3. **One frame, no transform.** The sources take shape B's pose in shape
   A's frame and bring B's points into it at the start of a query. Here
   both proxies arrive in one frame: the caller builds them so, which is
   that same step. A `Convex` shape turns its points once, with
   `Proxy::offset`, when it is built; the pair glue (`src/sweep/kernel.rs`)
   places each shape relative to the body. `time_of_impact` does it itself
   for each pose it tries. The frame is the body-relative one every other
   query in the crate uses; the kernel does not recentre.
4. **Proxies own their points.** A 2D proxy holds up to 8 points, as in
   Box2D. Box3D's proxy points at the caller's array of up to 64; here it
   holds up to 16, the largest hull the crate offers.
5. **One shape cast.** `b2CastShape` and `b3ShapeCast` are the same
   conservative advancement, so it is written once, over a distance
   function each dimension supplies.
6. **States instead of zeros.** The sources report overlap as a zero
   distance and a miss as `hit == false`. Here a distance says whether the
   cores are `Separated`, `Overlapped` or cut off at the `IterationCap`, and
   a cast says `Miss`, `Hit`, `InitialOverlap` or `Failed`.
7. **Reaching a cap is reported, with the best answer so far.**
   - Distance: at the cap both sources read the answer from a simplex whose
     last vertex was added and never solved, so its weight is stale. Here
     the answer comes from the last simplex solved, under `IterationCap`.
   - Cast: the sources return "no hit" when twenty advances were not
     enough, which a mover would read as a clear path. Here it is `Failed`,
     at the fraction reached, where the shapes are still apart.
8. **A cast never reports a hit without a unit normal.** After an advance
   the cores cannot have met, so a zero distance or a normal that is not
   unit there is rounding the slop did not absorb. Box2D asserts and then
   reports the hit with that normal; Box3D logs and reports no hit when the
   distance is positive, and a hit with a zero normal when it is zero. Here
   all three are `Failed`.
9. **An initial overlap carries its normal.** The sources leave the normal
   zero when a cast starts within the target distance. The distance query
   has already found it, so it is reported: unit when the cores are apart,
   zero when they overlap.
10. **Scalar `f32` only.** No SIMD path, no `mul_add`, no platform maths
    function: only add, subtract, multiply, divide, square root and
    comparison, in the sources' order. Neither source uses SIMD in this
    code. The kernel calls no trigonometric function; rotations arrive as a
    cosine and sine (2D) or a matrix or unit quaternion (3D).
11. **Loops are counted.** The time of impact's three `for (;;)` loops,
    which the source leaves by a counter, are `for` loops over the same
    counts, named `MAX_TOI_ITERATIONS` (20), `MAX_PUSH_BACK_ITERATIONS` (8)
    and `MAX_ROOT_ITERATIONS` (50). The distance caps are
    `MAX_GJK_ITERATIONS` (20 in 2D, 32 in 3D) and the cast's is
    `MAX_CAST_ITERATIONS` (20).

### Planar bodies

12. **The manifold's lengths are the pair's.** `b2CollidePolygons` takes
    `B2_LINEAR_SLOP` and `B2_SPECULATIVE_DISTANCE` (four times it); here
    they are the pair's slop, the query rule of the kernel's pairs, and
    four of it.
13. **Two capsules are two polygons.** Box2D collides two capsules with
    `b2CollideCapsules`; here they go through `b2CollidePolygons` as two
    two-sided polygons (`b2MakeCapsule`), as a polygon and a capsule do in
    the source, so every pair with an edge has one manifold function. A
    capsule no longer than the slop, which `b2CreateCapsuleShape` refuses
    and `b2MakeCapsule` cannot give a direction, is met as a circle.
14. **The normal runs from the first body.** Box2D swaps a pair into its
    registry's order and reports the normal from that shape; here the pair
    is swapped the same way and the normal turned back, so it always runs
    from `a` toward `b`. Points come back in the frame of the pair's first
    body's start, not shape A's.
15. **One bump, applied once.** The bump is one sequential impulse at one
    point: the normal row with restitution's velocity target
    (`restitution * relativeVelocity`) folded into it, the friction row
    clamped by that impulse, then the normal row once more, without
    restitution and only pushing, to remove any approach the friction
    left through the spins. Box2D applies restitution in a later pass
    that may pull the normal impulse back below what friction was clamped
    by; applied once, that leaves friction above its bound, so it is not
    ported. Each impulse is applied as it is found rather than
    accumulated. No restitution threshold (the game's restitution decides
    every bump), softness, speculative bias, warm start, rolling
    resistance or tangent speed.
16. **A hit meets the manifold.** The kernel's time of impact reports a
    witness of its distance query, which for two parallel faces can lie
    anywhere along them; a bump there would spin a car that met a wall
    square. A hit's contact is the manifold's at that instant: its normal,
    and the middle of its two points when their separations are within a
    slop, or else the closer one. Box2D meets a pair its time of impact
    brought together the same way, through the next step's manifold.
17. **Contacts within the step are not passed over.** The kernel's time of
    impact reports `Separated` when the axis it found still separates the
    shapes at the end of the motion; when the shapes turn, a contact that
    begins and ends within the motion is missed that way, which Box2D
    documents (`b2ComputeTimeOfImpact` "may miss some intermediate,
    non-tunneling collisions") and the comparison with parry found (two
    turning boxes that overlap for most of the step). The planar time of
    impact checks the kernel's answer: over a stretch of the motion the
    gap along the closest points' axis at its start cannot fall below its
    lesser end value through translation (it is concave in time), and
    turning moves the surfaces by at most `2 tan(phi / 2)` times their
    reach per unit of fraction, so a stretch whose bound does not clear
    half a slop is halved, its middle measured, at most `MAX_GAP_CHECKS`
    (32) distance queries. A contact found is searched to with the kernel
    again; a check that cannot settle is `Failed` where it stopped. The
    kernel itself is unchanged.

## Algorithm notes

**Proxies.** A shape is the hull of a few points grown by a radius: a ball
is one point, a capsule two, a box its corners with no radius. The hull
without the radius is the *core*. Everything is computed on cores; the
radii are added at the end.

**Distance (GJK).** The distance between two convex sets is the distance
from the origin to their Minkowski difference. GJK never builds the
difference: it keeps a simplex of up to three (2D) or four (3D) of its
vertices, each the difference of one support point of each shape. A step
solves the simplex for its part nearest the origin (a vertex, an edge, a
face), drops the vertices that part does not use, and asks both shapes for
the support point furthest toward the origin from there. It stops when the
support point is already in the simplex (nothing further that way), in 3D
also when the distance stops shrinking, and reports overlap when the
simplex encloses the origin. Box2D builds the simplex in A − B and Box3D in
B − A; each port keeps its source's sign, and both report the normal from A
toward B.

The solvers decide by Voronoi regions, from barycentric weights left
unnormalised until a region is chosen. In a running query the vertex added
last always belongs to the nearest part, so the regions of older vertices
are reached only through rounding; `the_solvers_find_the_nearest_part…`
tests every region directly, in every vertex order.

**Shape cast (conservative advancement).** Given the closest points and
the normal between the cores, B cannot reach A before it has closed the
gap along that normal, so B may advance by `(distance - target) /
(approach speed along the normal)` without passing the target. Repeat from
the new position. Against a flat face one step lands exactly; against a
corner the normal turns as B approaches and a few steps are needed.

The target is `max(slop, radii - slop)` between the cores, reached within
`slop / 4`. So two shapes with radii meet `slop` *inside* each other's
surfaces, and two without meet `slop` apart: the sources' soft contact. A
cast that begins inside the target is an `InitialOverlap`. With
`can_encroach`, one that begins inside the target but with cores more than
`2 * slop` apart is instead allowed `slop` closer, so a round body resting
on a surface can still be cast along or into it; Box2D's mover casts this
way. The crate does not encroach: its pair glue grows the moving shape's
radius by twice its slop, so the target is the radii plus one slop and
every pair, round or not, meets a slop apart (`specs/physics.md`, 19).

**Time of impact (local separating axis).** At a time when the shapes are
apart, the distance query names the features nearest each other: two
points, or a point and an edge. They define an axis that moves with the
shapes, along which the separation is a function of time alone. A root
finder (bisection alternating with false position) finds the latest time
that axis still separates the shapes by the target; the shapes cannot have
met before it. Time advances there and the search repeats with the new
nearest features. It reports `Separated` when the axis found at the start
still separates the shapes at the end, so it can miss a contact that begins
and ends within the step: a thin shape spinning fast past another. A sweep
turns by the normalised blend of its two rotations, not at a constant
angular speed.

**Where the pair stands.** The kernel computes with the points it is
given. Results are bit-equal for a pair wherever it stands only when the
caller hands over body-relative proxies (the difference of two positions
on a grid is exact) or when every sum formed from a position is exact.
`results_are_bit_identical_when_both_shapes_move_together` shows the second
on a grid of halves; a diagonal cast far from the origin advances to
positions that round differently there. Box3D's support search measures
points from the proxy's first for the same reason, and that is ported.

**The planar manifold (separating axes and clipping).** For two
polygons, `b2FindMaxSeparation` finds, on each polygon's edge normals, how
far the other polygon lies beyond them; the edge of greatest separation
is the reference edge (A's on equal terms). Clearly apart (by more than a
tenth of a slop), the closest points of the reference edge and the edge of
the other polygon most against its normal (the incident edge) decide
between a single vertex contact and the clipped edges; otherwise the
incident edge is clipped to the reference edge's extent, giving one or two
points, each midway between the surfaces with its own separation. Over
182,616 random overlapping pairs of boxes and polygons the deepest clipped
point was always as deep as the separating-axis depth, so moving a body
by the manifold's depth along its normal separates it.

**The bump.** With `r` the contact's offset from each centre, the
effective mass along a direction `d` is `1 / (mA + mB + iA (rA x d)^2 +
iB (rB x d)^2)`. Each impulse along `d` changes the bodies' relative
velocity at the contact along `d` by exactly the impulse over that mass,
and changes their kinetic energy by the impulse times the speed along `d`
plus half its square over the mass, which is never positive for an impulse
between zero and the one that stops the motion along `d` (friction, the
last normal row) or that reverses at most all of it (the first normal
row): so momentum, angular momentum about any point, and at restitution
at most 1 no gain of energy, hold.

## Fragile in the sources

Recorded so a later port update looks at them again.

- The stale weight at the iteration cap (departure 7), in both.
- Box2D's `b2SimplexVertex` comment says `w = wB - wA`; the code computes
  `wA - wB`.
- Box2D's `b2ComputeWitnessPoints` returns one point for both shapes for a
  full simplex, under a "todo why are these not equal?".
- Box2D's `b2SolveSimplex3` divides by the sum of the triangle weights with
  no test that the triangle has area. A sliver normally leaves by an edge
  or vertex region first.
- Box3D's `b3ShapeDistance` returns a zeroed output, both points at the
  frame's origin, when its final normal is not unit, which the first
  distance overflowing brings about. The port keeps that, as `Overlapped`.
- Box3D's cache metric can be negative for a tetrahedron (its own todo);
  not ported.
- `b2ComputeTimeOfImpact` notes that a sweep whose centre is at the origin
  can still have points far away (its `todo_erin`): keep the sweeps' frame
  near the shapes.
- `b2ComputeTimeOfImpact` passes over a contact that begins and ends within
  the motion when the shapes turn (departure 17).
- `b2ClipPolygons` notes that its two points can be very close (its
  "reduce to one point?" todo); kept as two.
- `b2CollideCircles` and `b2CollideCapsuleAndCircle` leave the normal zero
  for coincident centres, which no impulse or push can use.
- `b2ApplyRestitution_Overflow` can pull a contact's normal impulse back
  below the one its friction was clamped by, which further iterations
  correct (departure 15).
