# Provenance and algorithm notes

`sgl-physics` is SGL's own, except its convex kernel (`src/convex.rs` and
`src/convex/`), which is ported from two projects by Erin Catto, both MIT:

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
| `convex/d2/toi.rs`: `Sweep`, `time_of_impact` | Box2D `src/distance.c`: `b2GetSweepTransform`, `b2MakeSeparationFunction`, `b2FindMinSeparation`, `b2EvaluateSeparation`, `b2ComputeTimeOfImpact` |
| `convex/d3.rs`: `Rot`, `Proxy::offset`, `distance`, `shape_cast` | Box3D `src/distance.c`: `b3GetProxySupport`, `b3ShapeDistance`; `b3MakeMatrixFromQuat`, `b3MulMV`, `b3Normalize` of `include/box3d/math_functions.h`; `include/box3d/types.h`, `include/box3d/collision.h` |
| `convex/d3/simplex.rs`: the simplex and its solvers | Box3D `src/distance.c`: `b3BarycentricCoordsEdge`, `b3BarycentricCoordsTri`, `b3BarycentricCoordsTet`, `b3SolveSimplex2`, `b3SolveSimplex3`, `b3SolveSimplex4`, `b3ComputeWitnessPoints`; `src/math_internal.h` |

Not ported: Box2D's `b2ComputeSegmentDistance` and its disabled
`b2ShapeCastMerged`; Box3D's `b3TimeOfImpact` and everything after it; the
debug simplex output and the time-of-impact counters of both.

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
   both proxies arrive in one frame: the caller builds them so, with
   `Proxy::offset` when a shape is turned or moved, which is that same
   step. `time_of_impact` does it itself for each pose it tries. The frame
   is the body-relative one every other query in the crate uses; the kernel
   does not recentre.
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
way.

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
