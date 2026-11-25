//! A point's own pairs: a ray ([`crate::ray`], a box with no extent)
//! against a ball, a capsule, a 2D polygon or a 3D triangle (a convex
//! shape of exactly three points), each in closed form with no slop, as
//! `Box2D` and `Box3D` cast rays. Every other pair a point makes (a box, a cut
//! box, a 3D hull of four or more points, a triangle with no area, a
//! capsule of no radius) stays where it was: the box sweep or the kernel.
//! A ball of no radius is a point, which a ray never meets.
//!
//! Ported from `Box3D` v0.1.0 (commit
//! `8441b4a06d6d09dcfb0b0f704df4d847d1437b92`): `b3RayCastSphere` of
//! `src/sphere.c`, `b3RayCastCapsule` of `src/capsule.c`,
//! `b3IntersectRayTriangle` of `src/simd.c` and the hit normal of
//! `b3RayCastMesh` (`src/mesh.c`), copyright 2025–2026 Erin Catto; and
//! from `Box2D` v3.2.0 (commit `178495ba35695aefe9c321e569d2277f06db3b90`):
//! `b2CastRayPolygon` of `src/geometry.c` and the edge normals of
//! `b2MakePolygon` there, copyright 2023 Erin Catto. Both MIT:
//! `LICENSE-box3d.txt` and `LICENSE-box2d.txt` beside this crate's
//! manifest. `Box2D`'s `b2CastRayCircle` is `b3RayCastSphere` operation for
//! operation, and the sphere and capsule here use only dot products, so one
//! implementation of each serves 2D and 3D (`specs/physics.md`, 7).
//! Departures are in `PROVENANCE.md`.
//!
//! The port keeps the sources' short names (`s`, `d`, `t`, `c`, `h`, `r`,
//! `u`, `v`) so it reads against them.
//!
//! **Exact.** The body is at the origin and the target at `rel`, so every
//! coordinate is body-relative. A hit's fraction and point are the true
//! crossing of the surface to within a few units in the last place of the
//! coordinates involved (the target's position relative to the ray's
//! start, its size and the ray's length). Near a tangent, where the
//! crossing is ill-conditioned, the point still lies on the surface to that
//! bound.
//!
//! **Starts.** A ray that starts strictly inside a ball, a capsule or a
//! polygon passes it ([`inside`]); one inside by no more than the
//! collider's rounding allowance meets it at once when it moves in
//! ([`resting`]), as a box does. A start read a rounding step behind the
//! surface it is on, with the far crossing ahead, is a hit at the start. A
//! triangle has no inside: a ray starting on its plane, to within the
//! rounding of its coordinates, and leaving the plane meets it at once.
//!
//! **Grazes.** A hit whose normal the ray does not approach is no contact,
//! as for every pair through the kernel: a ray tangent to a ball, or one
//! running along a polygon's edge or in a triangle's plane, passes.

#![allow(clippy::many_single_char_names)]

use super::Contact;
use crate::shape::{Cut, Shape};
use crate::space::{self, Vector};

/// How far a triangle's plane and edges reach beyond where they are
/// computed, in units of the largest coordinate of its corners relative to
/// the ray's start: eight units in the last place. A shared corner computed
/// from two triangles' own positions can differ by a few units in its last
/// place, so a ray exactly on a seam would otherwise fall between them.
const TRIANGLE_ROUNDING: f32 = 8.0 * f32::EPSILON;

/// A target a point meets exactly, in its own frame about its position.
#[derive(Clone, Copy)]
pub(super) enum Target<'a, V> {
    /// A ball of positive radius.
    Ball { radius: f32 },
    /// A capsule of positive radius.
    Capsule { half_axis: V, radius: f32 },
    /// A 2D convex polygon: its vertices, strictly convex and
    /// counter-clockwise.
    Polygon(&'a [V]),
    /// A 3D triangle with area: its corners and its unit normal, from its
    /// corners alone.
    Triangle { corners: [V; 3], normal: V },
}

impl<'a, V: Vector> Target<'a, V> {
    /// The exact pair `body` makes with `target`, or `None` when it has
    /// none: the body has extent, or the target is a box, a hull of four or
    /// more points, a triangle with no area, or a ball or capsule of no
    /// radius.
    pub(super) fn of(body: &Shape<V>, target: &'a Shape<V>, cut: Option<&Cut<V>>) -> Option<Self> {
        match body {
            Shape::Box { half } if *half == V::ZERO && cut.is_none() => {}
            _ => return None,
        }
        match target {
            Shape::Ball { radius } if *radius > 0.0 => Some(Self::Ball { radius: *radius }),
            Shape::Capsule { half_axis, radius } if *radius > 0.0 => Some(Self::Capsule {
                half_axis: *half_axis,
                radius: *radius,
            }),
            Shape::Convex(hull) if V::AXES == 2 => Some(Self::Polygon(hull.points())),
            Shape::Convex(hull) if hull.points().len() == 3 => {
                let [a, b, c] = [hull.points()[0], hull.points()[1], hull.points()[2]];
                let (normal, _) = space::direction(cross(b - a, c - a))?;
                Some(Self::Triangle {
                    corners: [a, b, c],
                    normal,
                })
            }
            _ => None,
        }
    }
}

/// Whether the point at the origin lies strictly inside `target` at `rel`:
/// on the surface is not inside, and a triangle has no inside.
pub(super) fn inside<V: Vector>(target: Target<'_, V>, rel: V) -> bool {
    match target {
        Target::Ball { radius } => {
            let s = -rel;
            space::dot(s, s) < radius * radius
        }
        Target::Capsule { half_axis, radius } => {
            let away = -capsule_nearest(half_axis, rel);
            space::dot(away, away) < radius * radius
        }
        Target::Polygon(points) => (0..points.len()).all(|i| polygon_edge(points, i, rel).1 > 0.0),
        Target::Triangle { .. } => false,
    }
}

/// The first contact of a point at the origin moving by `delta` with
/// `target` at `rel`: the true crossing of its surface, or `None` when the
/// ray misses it, starts strictly inside it, grazes it, moves away from it,
/// or is zero or not finite.
pub(super) fn sweep<V: Vector>(target: Target<'_, V>, delta: V, rel: V) -> Option<Contact<V>> {
    if delta == V::ZERO || !space::is_finite(delta) || inside(target, rel) {
        return None;
    }
    let (t, normal) = match target {
        Target::Ball { radius } => ball(rel, radius, delta)?,
        Target::Capsule { half_axis, radius } => capsule(rel, half_axis, radius, delta)?,
        Target::Polygon(points) => polygon(points, rel, delta)?,
        Target::Triangle { corners, normal } => triangle(corners, normal, rel, delta)?,
    };
    // A graze: the ray does not approach the face it touches.
    let approach = space::dot(delta, normal);
    if approach.is_nan() || approach >= 0.0 {
        return None;
    }
    Some(Contact {
        t: t + 0.0,
        normal,
        faces: 0,
    })
}

/// The contact for a point strictly inside `target` at `rel` by no more
/// than `allowance` along the normal of the surface it is nearest, when
/// `blocks` accepts that normal: a ray starting a rounding step inside the
/// surface it is on in world coordinates meets it at once. Polygon edges
/// are tried in order, as a box tries its axes.
pub(super) fn resting<V: Vector>(
    target: Target<'_, V>,
    rel: V,
    allowance: V,
    blocks: impl Fn(V) -> bool,
) -> Option<Contact<V>> {
    let within = |depth: f32, normal: V| depth <= space::reach(allowance, normal) && blocks(normal);
    let normal = match target {
        Target::Ball { radius } => {
            let (normal, distance) = space::direction(-rel)?;
            within(radius - distance, normal).then_some(normal)?
        }
        Target::Capsule { half_axis, radius } => {
            let (normal, distance) = space::direction(-capsule_nearest(half_axis, rel))?;
            within(radius - distance, normal).then_some(normal)?
        }
        Target::Polygon(points) => (0..points.len())
            .map(|i| polygon_edge(points, i, rel))
            .find(|&(normal, depth)| within(depth, normal))
            .map(|(normal, _)| normal)?,
        Target::Triangle { .. } => return None,
    };
    Some(Contact {
        t: 0.0,
        normal,
        faces: 0,
    })
}

/// `b3RayCastSphere`: a ray from the origin by `delta` against a ball of
/// `radius` about `center`, as its fraction and the unit normal there. The
/// start is not inside.
fn ball<V: Vector>(center: V, radius: f32, delta: V) -> Option<(f32, V)> {
    // Shift the ray so the ball's centre is the origin.
    let s = -center;
    let rr = radius * radius;
    let (d, length) = space::direction(delta)?;
    // The closest point on the ray's line to the centre: dot(s + t d, d) = 0.
    let t = -space::dot(s, d);
    let c = s + d * t;
    let cc = space::dot(c, c);
    if cc > rr {
        // The line passes outside the ball.
        return None;
    }
    // Pythagoras.
    let h = (rr - cc).sqrt();
    let mut fraction = t - h;
    if fraction < 0.0 {
        // Both crossings behind: the ray moves away. Otherwise the start,
        // which is not inside, is on the surface and read a rounding step
        // behind it: a hit at the start.
        if t + h < 0.0 {
            return None;
        }
        fraction = 0.0;
    }
    if length < fraction {
        return None;
    }
    let hit = s + d * fraction;
    let (normal, _) = space::direction(hit)?;
    Some((fraction / length, normal))
}

/// `b3RayCastCapsule`: a ray from the origin by `delta` against the
/// capsule of `half_axis` and `radius` about `rel`, as its fraction and the
/// unit normal there. The start is not inside.
fn capsule<V: Vector>(rel: V, half_axis: V, radius: f32, delta: V) -> Option<(f32, V)> {
    let r = radius;
    let rr = r * r;
    // A capsule with no axis is a ball.
    let Some((axis, half_length)) = space::direction(half_axis) else {
        return ball(rel, r, delta);
    };
    let (c1, c2) = (rel - half_axis, rel + half_axis);
    let length = half_length * 2.0;
    // From the first centre to the ray's start.
    let s = -c1;
    // The start projected onto the axis, and its offset from the axis.
    let u = space::dot(s, axis);
    let sc = s - axis * u;
    let sc2 = space::dot(sc, sc);
    if sc2 < rr {
        // Within the infinite cylinder but, the caller found, not the
        // capsule: the ray can meet only the end cap nearest the start.
        return ball(c1 + axis * clamp(u, length), r, delta);
    }
    let (ray_axis, ray_length) = space::direction(delta)?;
    // Where the ray's end projects onto the axis.
    let v = u + space::dot(delta, axis);
    if (u < -r && v < -r) || (length + r < u && length + r < v) {
        return None;
    }
    // Closest points between the ray's line and the axis (Real-Time
    // Collision Detection, 5.1.9): t1 along the axis, t2 along the ray.
    let a12 = space::dot(axis, ray_axis);
    let det = 1.0 - a12 * a12;
    let tr = if det < f32::EPSILON {
        // Nearly parallel: the ray against the circle the cylinder makes
        // seen along its axis, from the start.
        let perp = ray_axis - axis * a12;
        let perp2 = space::dot(perp, perp);
        let beta = space::dot(sc, perp);
        let gamma = sc2 - rr;
        let disc = beta * beta - perp2 * gamma;
        if beta >= 0.0 || disc < 0.0 {
            return None;
        }
        // The near root, written to avoid cancellation as the ray nears
        // parallel.
        gamma / (-beta + disc.sqrt())
    } else {
        let inv_det = 1.0 / det;
        let sa1 = u;
        let sa2 = space::dot(s, ray_axis);
        let t1 = (sa1 - a12 * sa2) * inv_det;
        let t2 = (a12 * sa1 - sa2) * inv_det;
        let g = (s + ray_axis * t2) - axis * t1;
        let g2 = space::dot(g, g);
        if g2 > rr {
            return None;
        }
        // The infinite cylinder's crossings, from the closest point.
        let h = ((rr - g2) * inv_det).sqrt();
        let near = t2 - h;
        // As for a ball: both behind is moving away; otherwise the start,
        // outside the cylinder, is on it, read a rounding step behind.
        if near < 0.0 && t2 + h >= 0.0 {
            0.0
        } else {
            near
        }
    };
    if tr < 0.0 || ray_length < tr {
        return None;
    }
    // Where along the axis the ray meets the cylinder: past an end, it
    // meets that end's cap or nothing.
    let tc = u + tr * a12;
    if tc < 0.0 {
        return ball(c1, r, delta);
    }
    if length < tc {
        return ball(c2, r, delta);
    }
    let p = s + ray_axis * tr;
    let (normal, _) = space::direction(p - axis * tc)?;
    Some((clamp(tr / ray_length, 1.0), normal))
}

/// `b3ClampFloat(value, 0, upper)`, by comparison alone.
fn clamp(value: f32, upper: f32) -> f32 {
    if value < 0.0 {
        0.0
    } else if value < upper {
        value
    } else {
        upper
    }
}

/// The point of the capsule's segment nearest the origin, relative to the
/// origin: the clamped projection `b3RayCastCapsule` makes for a start
/// within its cylinder, by the same operations as [`capsule`].
fn capsule_nearest<V: Vector>(half_axis: V, rel: V) -> V {
    let Some((axis, half_length)) = space::direction(half_axis) else {
        return rel;
    };
    let c1 = rel - half_axis;
    c1 + axis * clamp(space::dot(-c1, axis), half_length * 2.0)
}

/// The outward unit normal of a polygon's edge from vertex `i` to the
/// next, from its own vertices (`b2MakePolygon`), and how far the origin
/// lies inside that edge: `dot(normal, vertex - origin)`, positive inside.
fn polygon_edge<V: Vector>(points: &[V], i: usize, rel: V) -> (V, f32) {
    let edge = points[(i + 1) % points.len()] - points[i];
    let mut outward = V::ZERO;
    outward[0] = edge[1];
    outward[1] = -edge[0];
    // A strictly convex polygon has no edge of zero length.
    let normal = space::direction(outward).map_or(V::ZERO, |(normal, _)| normal);
    (normal, space::dot(normal, rel + points[i]))
}

/// `b2CastRayPolygon`: a ray from the origin by `delta` clipped against the
/// half-planes of the polygon's edges, as its fraction and the normal of
/// the edge it enters by last (the first such edge on a tie). The start is
/// not inside.
fn polygon<V: Vector>(points: &[V], rel: V, delta: V) -> Option<(f32, V)> {
    let (mut lower, mut upper) = (0.0f32, 1.0f32);
    let mut entered: Option<V> = None;
    for i in 0..points.len() {
        // p = a d; dot(normal, p - v) = 0; so a = numerator / denominator.
        let (normal, numerator) = polygon_edge(points, i, rel);
        let denominator = space::dot(normal, delta);
        if denominator == 0.0 {
            // Parallel to the edge, and outside it or along its line.
            if numerator <= 0.0 {
                return None;
            }
        } else if denominator < 0.0
            && (numerator < lower * denominator || (entered.is_none() && numerator <= 0.0))
        {
            // The ray enters this half-plane later than any before; a
            // start on the edge's line enters it at once.
            lower = numerator / denominator;
            entered = Some(normal);
        } else if denominator > 0.0 && numerator < upper * denominator {
            // The ray leaves this half-plane sooner.
            upper = numerator / denominator;
        }
        if upper < lower {
            return None;
        }
    }
    entered.map(|normal| (lower, normal))
}

/// `b3IntersectRayTriangle`, two-sided: a ray from the origin by `delta`
/// against the triangle of `corners` about `rel` with unit `normal`, as its
/// fraction and the normal turned against the ray.
fn triangle<V: Vector>(corners: [V; 3], normal: V, rel: V, delta: V) -> Option<(f32, V)> {
    let at = [rel + corners[0], rel + corners[1], rel + corners[2]];
    let largest = at.iter().fold(0.0, |largest, &corner| {
        space::max(largest, space::largest_abs(corner))
    });
    let reach = TRIANGLE_ROUNDING * largest;
    // The plane: dot(normal, a delta - corner) = 0.
    let denominator = space::dot(normal, delta);
    if denominator == 0.0 || !denominator.is_finite() {
        // In or parallel to the plane: a graze.
        return None;
    }
    let numerator = space::dot(normal, at[0]);
    let mut lambda = numerator / denominator;
    if lambda < 0.0 {
        // Behind the start, unless the start is on the plane.
        if numerator.abs() > reach {
            return None;
        }
        lambda = 0.0;
    }
    if lambda > 1.0 {
        return None;
    }
    // Which side of each edge the ray passes, by the volume of the edge and
    // its midpoint with the ray: all on the triangle's side for a ray
    // entering it from the side the normal faces, all opposite from behind.
    // A ray passing within `reach` of an edge meets the triangle, so a
    // seam between two has no gap.
    let side = if denominator < 0.0 { 1.0 } else { -1.0 };
    for (j, k) in [(1, 2), (2, 0), (0, 1)] {
        let edge = corners[k] - corners[j];
        let mid = (at[j] + at[k]) * 0.5;
        let volume = space::dot(cross(edge, mid), delta) * side;
        if volume < 0.0 {
            let span = space::direction(cross(edge, delta)).map_or(0.0, |(_, length)| length);
            if volume < -(reach * span) {
                return None;
            }
        }
    }
    let toward = if denominator < 0.0 { normal } else { -normal };
    Some((lambda, toward))
}

/// The cross product of two 3D vectors, in the order `Box3D` writes it.
fn cross<V: Vector>(a: V, b: V) -> V {
    let mut out = V::ZERO;
    out[0] = a[1] * b[2] - a[2] * b[1];
    out[1] = a[2] * b[0] - a[0] * b[2];
    out[2] = a[0] * b[1] - a[1] * b[0];
    out
}

#[cfg(test)]
mod tests {
    use crate::shape::{Collider, Convex, Shape};
    use sgl_core::math::{Quat, Vec2, Vec3};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// The first hit of a ray among `world`, as `(t, normal, id)`.
    fn ray<V: crate::Vector>(from: V, by: V, world: &[Collider<V>]) -> Option<(f32, V, u64)> {
        crate::ray(from, by, world, u32::MAX)
            .unwrap()
            .map(|hit| (hit.t, hit.normal, hit.id))
    }

    fn close<V: crate::Vector>(got: (f32, V, u64), t: f32, normal: V) -> bool {
        let off = (0..V::AXES).fold(0.0f32, |most, axis| {
            most.max((got.1[axis] - normal[axis]).abs())
        });
        (got.0 - t).abs() <= 4.0 * f32::EPSILON && off <= 4.0 * f32::EPSILON
    }

    /// Defect: a ray against a ball stopped short of it (the kernel's
    /// slop), past it, with a normal off the radius, or a start on, inside
    /// or grazing it read wrong. Oracle: the 3-4-5 triangle. A ball of
    /// radius 5 at (100, 50); a ray along y = 53 from x = 90 by 20 meets
    /// it where x = 100 - sqrt(25 - 9) = 96, after 6 of 20: t = 0.3,
    /// normal (-4, 3) / 5, exactly in `f32`. Along y = 55 it is tangent at
    /// (100, 55): a graze, no contact. From (95, 50), on the surface,
    /// moving in it meets it at once, facing -x; moving out, nothing. From
    /// (99, 50), inside, it passes the ball and meets the box behind, whose
    /// face at x = 110 is 11 of its 20 away. The same in 3D.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_meets_a_ball_where_the_quadratic_puts_it() {
        let ball = Collider::new(1, Shape::Ball { radius: 5.0 }, Vec2::new(100.0, 50.0));
        let behind = Collider::new(2, Shape::Box { half: Vec2::ONE }, Vec2::new(111.0, 50.0));
        let world = [ball, behind];
        assert_eq!(
            ray(Vec2::new(90.0, 53.0), Vec2::new(20.0, 0.0), &world),
            Some((0.3, Vec2::new(-0.8, 0.6), 1))
        );
        assert_eq!(
            ray(Vec2::new(90.0, 55.0), Vec2::new(20.0, 0.0), &world),
            None
        );
        assert_eq!(
            ray(Vec2::new(95.0, 50.0), Vec2::new(10.0, 0.0), &world),
            Some((0.0, Vec2::NEG_X, 1))
        );
        assert_eq!(
            ray(Vec2::new(95.0, 50.0), Vec2::new(-10.0, 0.0), &world),
            None
        );
        assert_eq!(
            ray(Vec2::new(99.0, 50.0), Vec2::new(20.0, 0.0), &world),
            Some((0.55, Vec2::NEG_X, 2))
        );

        let ball = Collider::new(
            1,
            Shape::Ball { radius: 5.0 },
            Vec3::new(100.0, 50.0, -20.0),
        );
        assert_eq!(
            ray(
                Vec3::new(90.0, 50.0, -17.0),
                Vec3::new(20.0, 0.0, 0.0),
                &[ball]
            ),
            Some((0.3, Vec3::new(-0.8, 0.0, 0.6), 1))
        );
    }

    /// Defect: a ray against a capsule stopped short, meeting the cylinder
    /// beyond an end or an end cap from the side, or a graze or a start on
    /// or inside it read wrong. Oracle: hand-worked geometry. A capsule of
    /// half-axis (0, 2, 0) and radius 1.25 at (10, 0, 0), its side at x =
    /// 8.75 and its caps about (10, ±2, 0). Along x at y = 0.5 from x = 5
    /// by 10 a ray meets the side after 3.75: t = 0.375, facing -x. Down
    /// from (10, 8, 0) by 10 it meets the top at y = 3.25: t = 0.475. Along
    /// x at y = 2.75, 0.75 above the cap's centre, it meets the cap where x
    /// = 10 - 1, after 4: t = 0.4, normal (-1, 0.75, 0) / 1.25. Up the line
    /// x = 8.75 it runs along the side, tangent to both caps: a graze. From
    /// (8.75, 0.5, 0), on the side, moving in it meets it at once; from
    /// (9.5, 0.3, 0), inside, it passes it. In 2D, the same capsule.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_meets_a_capsule_on_its_side_or_its_caps() {
        let capsule = Shape::Capsule {
            half_axis: Vec3::new(0.0, 2.0, 0.0),
            radius: 1.25,
        };
        let world = [Collider::new(1, capsule, Vec3::new(10.0, 0.0, 0.0))];
        let v = Vec3::new;
        assert_eq!(
            ray(v(5.0, 0.5, 0.0), v(10.0, 0.0, 0.0), &world),
            Some((0.375, Vec3::NEG_X, 1))
        );
        assert_eq!(
            ray(v(10.0, 8.0, 0.0), v(0.0, -10.0, 0.0), &world),
            Some((0.475, Vec3::Y, 1))
        );
        assert_eq!(
            ray(v(5.0, 2.75, 0.0), v(10.0, 0.0, 0.0), &world),
            Some((0.4, v(-0.8, 0.6, 0.0), 1))
        );
        assert_eq!(ray(v(8.75, -10.0, 0.0), v(0.0, 20.0, 0.0), &world), None);
        assert_eq!(
            ray(v(8.75, 0.5, 0.0), v(5.0, 0.0, 0.0), &world),
            Some((0.0, Vec3::NEG_X, 1))
        );
        assert_eq!(ray(v(9.5, 0.3, 0.0), v(10.0, 0.0, 0.0), &world), None);

        let capsule = Shape::Capsule {
            half_axis: Vec2::new(0.0, 2.0),
            radius: 1.25,
        };
        let world = [Collider::new(1, capsule, Vec2::new(10.0, 0.0))];
        assert_eq!(
            ray(Vec2::new(5.0, 2.75), Vec2::new(10.0, 0.0), &world),
            Some((0.4, Vec2::new(-0.8, 0.6), 1))
        );
    }

    /// Defect: a polygon's edges clipped on the wrong side, the wrong edge
    /// reported, a slide along an edge met, a corner graze missed, or a
    /// start on or inside it read wrong. Oracle: hand-worked geometry. The
    /// triangle (-2, -2), (2, -2), (-2, 2) about (20, 10): its long edge is
    /// x + y = 0 about the centre, facing (1, 1) / sqrt(2). A ray along y =
    /// 1 from x = 3 by -8 meets it at x = -1, half way; one up from (0, -5)
    /// by 10 meets the bottom at y = -2 after 3: t = 0.3, facing -y. Up the
    /// left edge's line x = -2 it slides along it: nothing. Through the
    /// corner (2, -2) from (3, -1) by (-2, -2) it touches only that corner,
    /// entering the long edge's half-plane as it leaves the bottom's: t =
    /// 0.5, facing (1, 1) / sqrt(2). From (0, -2), on the bottom, moving up
    /// it meets it at once; from the centre it passes it.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_meets_a_polygon_by_its_edges() {
        let points = [
            Vec2::new(-2.0, -2.0),
            Vec2::new(2.0, -2.0),
            Vec2::new(-2.0, 2.0),
        ];
        let shape = Shape::Convex(Convex::polygon(&points, 1.0, 0.0));
        let centre = Vec2::new(20.0, 10.0);
        let world = [Collider::new(1, shape, centre)];
        let at = |x: f32, y: f32| centre + Vec2::new(x, y);
        let diagonal = Vec2::splat(core::f32::consts::FRAC_1_SQRT_2);
        let hit = ray(at(3.0, 1.0), Vec2::new(-8.0, 0.0), &world).expect("the long edge");
        assert!(close(hit, 0.5, diagonal), "{hit:?}");
        assert_eq!(
            ray(at(0.0, -5.0), Vec2::new(0.0, 10.0), &world),
            Some((0.3, Vec2::NEG_Y, 1))
        );
        assert_eq!(ray(at(-2.0, -5.0), Vec2::new(0.0, 10.0), &world), None);
        let hit = ray(at(3.0, -1.0), Vec2::new(-2.0, -2.0), &world).expect("the corner");
        assert!(close(hit, 0.5, diagonal), "{hit:?}");
        assert_eq!(
            ray(at(0.0, -2.0), Vec2::new(0.0, 4.0), &world),
            Some((0.0, Vec2::NEG_Y, 1))
        );
        assert_eq!(ray(at(-0.5, -0.5), Vec2::new(0.0, 4.0), &world), None);
    }

    /// Defect: a ray against a triangle stopped short of its plane, met
    /// from one side only, with a normal not the plane's or not against the
    /// ray, a ray in its plane met, an edge missed or a start on it read
    /// wrong. Oracle: hand-worked geometry. The triangle (6, 0, 0), (0, 3,
    /// 0), (0, 0, 3) about (40, -20, 10) lies on x + 2y + 2z = 6 about it,
    /// normal (1, 2, 2) / 3. A ray from its origin by (4, 4, 4) reaches the
    /// plane when 20 t = 6, at t = 0.3, facing -(1, 2, 2) / 3; one back from
    /// (2, 2, 2) by (-4, -4, -4) when 10 - 20 t = 6, at 0.2, facing (1, 2,
    /// 2) / 3. A level triangle (-10, 0, -10), (10, 0, -10), (0, 0, 10)
    /// about (0, 0.5, 0): down from (1, 5.5, 1) by 10, half way, facing up;
    /// exactly on its edge z = -10 too; a hundredth beyond it, nothing; a
    /// ray in its plane, nothing, from beside it or from on it; a ray
    /// starting on it and leaving its plane meets it at once to either
    /// side, facing back along it, however shallow its way off the plane.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_meets_a_triangle_on_either_side() {
        let v = Vec3::new;
        let slanted = Convex::hull(
            &[v(6.0, 0.0, 0.0), v(0.0, 3.0, 0.0), v(0.0, 0.0, 3.0)],
            Quat::IDENTITY,
        );
        let centre = v(40.0, -20.0, 10.0);
        let world = [Collider::new(1, Shape::Convex(slanted), centre)];
        let third = v(1.0, 2.0, 2.0) * (1.0 / 3.0);
        let hit = ray(centre, v(4.0, 4.0, 4.0), &world).expect("the front");
        assert!(close(hit, 0.3, -third), "{hit:?}");
        let hit = ray(centre + v(2.0, 2.0, 2.0), v(-4.0, -4.0, -4.0), &world).expect("the back");
        assert!(close(hit, 0.2, third), "{hit:?}");

        let level = Convex::hull(
            &[v(-10.0, 0.0, -10.0), v(10.0, 0.0, -10.0), v(0.0, 0.0, 10.0)],
            Quat::IDENTITY,
        );
        let world = [Collider::new(1, Shape::Convex(level), v(0.0, 0.5, 0.0))];
        let down = v(0.0, -10.0, 0.0);
        assert_eq!(ray(v(1.0, 5.5, 1.0), down, &world), Some((0.5, Vec3::Y, 1)));
        assert_eq!(
            ray(v(0.0, 5.5, -10.0), down, &world),
            Some((0.5, Vec3::Y, 1))
        );
        assert_eq!(ray(v(0.0, 5.5, -10.01), down, &world), None);
        assert_eq!(ray(v(-20.0, 0.5, 0.0), v(40.0, 0.0, 0.0), &world), None);
        assert_eq!(ray(v(1.0, 0.5, 1.0), v(4.0, 0.0, -3.0), &world), None);
        assert_eq!(
            ray(v(1.0, 0.5, 1.0), v(0.0, -1.0, 0.0), &world),
            Some((0.0, Vec3::Y, 1))
        );
        assert_eq!(
            ray(v(1.0, 0.5, 1.0), v(4.0, 1e-3, -3.0), &world),
            Some((0.0, Vec3::NEG_Y, 1))
        );
        assert_eq!(
            ray(v(1.0, 0.5, 1.0), v(0.0, 1.0, 0.0), &world),
            Some((0.0, Vec3::NEG_Y, 1))
        );
    }

    /// Defect: a ray starting on a surface, whose start rounds a hair
    /// outside it or inside it, passes into the shape instead of meeting it
    /// (a ball's or capsule's near crossing read behind the start, a
    /// triangle's plane a rounding step behind, a start a rounding step
    /// inside not met). Oracle: the requirement that a ray starting on a
    /// surface moving in meets it at once, and the geometry of where the
    /// starts are: 400 seeded points on a ball, a capsule's side and caps,
    /// a polygon's edges and a slanted triangle, none on a binary grid,
    /// each with a motion into the shape. Each is met, at a `t` whose
    /// travel is within rounding (16 ε of the coordinates' 20) of the
    /// start.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_ray_starting_on_a_surface_moving_in_meets_it_at_once() {
        let mut seed = 0x0923_0b5e_u64;
        let mut draw = |low: f32, high: f32| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            #[allow(clippy::cast_precision_loss)]
            let unit = (seed >> 40) as f32 / 16_777_216.0;
            low + (high - low) * unit
        };
        let bound = 16.0 * f32::EPSILON * 20.0;
        let check = |world: &[Collider<Vec3>], from: Vec3, by: Vec3| {
            let hit = ray(from, by, world).unwrap_or_else(|| panic!("from {from} by {by}"));
            assert!(hit.0 * by.length() <= bound, "from {from} by {by}: {hit:?}");
        };
        let centre = Vec3::new(12.3, -4.1, 7.9);
        let ball = [Collider::new(1, Shape::Ball { radius: 1.7 }, centre)];
        let half_axis = Vec3::new(0.3, 1.1, -0.2);
        let capsule = [Collider::new(
            1,
            Shape::Capsule {
                half_axis,
                radius: 0.9,
            },
            centre,
        )];
        let corners = [
            Vec3::new(1.3, -0.7, 0.1),
            Vec3::new(-0.9, 0.3, 1.7),
            Vec3::new(0.2, 1.1, -1.3),
        ];
        let triangle = [Collider::new(
            1,
            Shape::Convex(Convex::hull(&corners, Quat::IDENTITY)),
            centre,
        )];
        let normal = (corners[1] - corners[0])
            .cross(corners[2] - corners[0])
            .normalize();
        for _ in 0..100 {
            let u = Vec3::new(draw(-1.0, 1.0), draw(-1.0, 1.0), draw(-1.0, 1.0)).normalize();
            let inward = -u * 3.0 + Vec3::new(draw(-1.0, 1.0), draw(-1.0, 1.0), draw(-1.0, 1.0));
            check(&ball, centre + u * 1.7, inward);
            let along = half_axis * draw(-1.3, 1.3);
            let core = along.clamp_length_max(half_axis.length());
            let side = (u - half_axis.normalize() * u.dot(half_axis.normalize())).normalize();
            let cap = if u.dot(along) < 0.0 { -u } else { u };
            let out = if along.length() > half_axis.length() {
                cap
            } else {
                side
            };
            check(
                &capsule,
                centre + core + out * 0.9,
                -out * 3.0 + inward * 0.1,
            );
            let (a, b) = (draw(0.0, 1.0), draw(0.0, 1.0));
            let (a, b) = if a + b > 1.0 {
                (1.0 - a, 1.0 - b)
            } else {
                (a, b)
            };
            let on = corners[0] + (corners[1] - corners[0]) * a + (corners[2] - corners[0]) * b;
            let side = if draw(0.0, 1.0) < 0.5 { 1.0 } else { -1.0 };
            check(&triangle, centre + on, normal * side + inward * 0.1);
        }
        let points = [
            Vec2::new(-1.3, -0.9),
            Vec2::new(1.7, -1.1),
            Vec2::new(0.9, 1.3),
            Vec2::new(-1.1, 0.7),
        ];
        let polygon = Shape::Convex(Convex::polygon(&points, 0.6, 0.8));
        let Shape::Convex(turned) = polygon else {
            unreachable!()
        };
        let centre = Vec2::new(-7.3, 3.9);
        let world = [Collider::new(1, polygon, centre)];
        for i in 0..100 {
            let (p, q) = (turned.points()[i % 4], turned.points()[(i + 1) % 4]);
            let on = centre + p + (q - p) * draw(0.0, 1.0);
            let into = (centre - on) + Vec2::new(draw(-0.3, 0.3), draw(-0.3, 0.3));
            let hit = ray(on, into, &world).unwrap_or_else(|| panic!("from {on} by {into}"));
            assert!(
                hit.0 * into.length() <= bound,
                "from {on} by {into}: {hit:?}"
            );
        }
    }
}
