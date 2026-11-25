//! The collision functions of planar shapes that overlap or nearly touch:
//! a normal and up to two points, each with its separation. Polygons are in
//! [`polygons`].
//!
//! Ported from `Box2D` v3.2.0 (commit
//! `178495ba35695aefe9c321e569d2277f06db3b90`), `src/manifold.c`
//! (`b2MakeCapsule`, `b2CollideCircles`, `b2CollideCapsuleAndCircle` and
//! `b2CollidePolygonAndCircle`), with `b2MakeBox` and the normals of
//! `b2MakePolygon` from `src/geometry.c`, the types of
//! `include/box2d/collision.h` and the vector functions of
//! `include/box2d/math_functions.h`. Copyright 2023 Erin Catto, MIT:
//! `LICENSE-box2d.txt` beside this crate's manifest. Departures and
//! algorithm notes are in `PROVENANCE.md`.
//!
//! Every function takes shape A in its own frame and shape B in its own,
//! with `xf`, B's pose in A's frame, and answers in A's frame with the
//! normal from A toward B, as the source does. The source's
//! `B2_LINEAR_SLOP` and `B2_SPECULATIVE_DISTANCE` are the caller's `slop`
//! and `speculative`.

// The source names the shapes A and B and their points cA and cB; the
// port keeps those names.
#![allow(clippy::similar_names)]

use sgl_core::math::Vec2;

use crate::convex::d2::{MAX_POINTS, Rot, Transform, dot, normalize, transform_point};

mod polygons;

pub(super) use polygons::collide_polygons;

/// A convex polygon grown by a radius, in its own frame (`b2Polygon`,
/// without the centroid the manifold does not read): its vertices
/// counter-clockwise and the outward unit normal of the edge from each
/// vertex to the next.
#[derive(Clone, Copy, Debug)]
pub(super) struct Polygon {
    vertices: [Vec2; MAX_POINTS],
    normals: [Vec2; MAX_POINTS],
    count: usize,
    radius: f32,
}

impl Polygon {
    /// A box of half-extents `half` about the origin (`b2MakeBox`).
    pub(super) fn boxed(half: Vec2) -> Self {
        let mut out = Self::empty(4, 0.0);
        out.vertices[0] = Vec2::new(-half.x, -half.y);
        out.vertices[1] = Vec2::new(half.x, -half.y);
        out.vertices[2] = Vec2::new(half.x, half.y);
        out.vertices[3] = Vec2::new(-half.x, half.y);
        out.normals[0] = Vec2::new(0.0, -1.0);
        out.normals[1] = Vec2::new(1.0, 0.0);
        out.normals[2] = Vec2::new(0.0, 1.0);
        out.normals[3] = Vec2::new(-1.0, 0.0);
        out
    }

    /// The polygon with `vertices`, strictly convex and counter-clockwise,
    /// and the normals `b2MakePolygon` computes for them: each edge turned
    /// a quarter turn clockwise and normalised.
    pub(super) fn of(vertices: &[Vec2]) -> Self {
        let count = vertices.len().min(MAX_POINTS);
        let mut out = Self::empty(count, 0.0);
        out.vertices[..count].copy_from_slice(&vertices[..count]);
        for i in 0..count {
            let i2 = if i + 1 < count { i + 1 } else { 0 };
            let edge = out.vertices[i2] - out.vertices[i];
            out.normals[i] = normalize(cross_vs(edge, 1.0));
        }
        out
    }

    /// A capsule from `p1` to `p2` grown by `radius`, as a two-sided
    /// polygon (`b2MakeCapsule`). The segment must have a direction.
    pub(super) fn capsule(p1: Vec2, p2: Vec2, radius: f32) -> Self {
        let mut out = Self::empty(2, radius);
        out.vertices[0] = p1;
        out.vertices[1] = p2;
        let axis = normalize(p2 - p1);
        let normal = right_perp(axis);
        out.normals[0] = normal;
        out.normals[1] = -normal;
        out
    }

    fn empty(count: usize, radius: f32) -> Self {
        Self {
            vertices: [Vec2::ZERO; MAX_POINTS],
            normals: [Vec2::ZERO; MAX_POINTS],
            count,
            radius,
        }
    }
}

/// One contact point (`b2ManifoldPoint`, the parts used here): midway
/// between the two surfaces, in A's frame, and how far apart the surfaces
/// are there along the normal (negative when they overlap).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Point {
    pub point: Vec2,
    pub separation: f32,
}

/// What a collision function found (`b2LocalManifold`): the unit normal
/// from A toward B in A's frame and up to two points. No points: the
/// shapes are further apart than the speculative distance.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Local {
    pub normal: Vec2,
    pub points: [Point; 2],
    pub count: usize,
}

impl Local {
    fn push(&mut self, point: Vec2, separation: f32) {
        self.points[self.count] = Point { point, separation };
        self.count += 1;
    }
}

/// `b2CrossVS`: `v` turned a quarter turn clockwise and scaled by `s`.
fn cross_vs(v: Vec2, s: f32) -> Vec2 {
    Vec2::new(s * v.y, -s * v.x)
}

/// `b2RightPerp`.
fn right_perp(v: Vec2) -> Vec2 {
    Vec2::new(v.y, -v.x)
}

/// `b2Lerp`.
fn lerp(a: Vec2, b: Vec2, t: f32) -> Vec2 {
    Vec2::new((1.0 - t) * a.x + t * b.x, (1.0 - t) * a.y + t * b.y)
}

/// `b2MulAdd`: `a + s * b`.
fn mul_add(a: Vec2, s: f32, b: Vec2) -> Vec2 {
    Vec2::new(a.x + s * b.x, a.y + s * b.y)
}

/// `b2MulSub`: `a - s * b`.
fn mul_sub(a: Vec2, s: f32, b: Vec2) -> Vec2 {
    Vec2::new(a.x - s * b.x, a.y - s * b.y)
}

/// `b2GetLengthAndNormalize`: the length of `v` and `v` as a unit vector,
/// or zero for both when it is too short.
fn length_and_normalize(v: Vec2) -> (f32, Vec2) {
    let length_squared = v.x * v.x + v.y * v.y;
    if length_squared > 1000.0 * f32::MIN_POSITIVE {
        let length = length_squared.sqrt();
        let s = 1.0 / length;
        (length, Vec2::new(s * v.x, s * v.y))
    } else {
        (0.0, Vec2::ZERO)
    }
}

/// `b2DistanceSquared`.
fn distance_squared(a: Vec2, b: Vec2) -> f32 {
    let c = Vec2::new(b.x - a.x, b.y - a.y);
    c.x * c.x + c.y * c.y
}

/// `b2ClampFloat`.
fn clamp(a: f32, lower: f32, upper: f32) -> f32 {
    if a < lower {
        lower
    } else if a > upper {
        upper
    } else {
        a
    }
}

/// `b2MinFloat`.
fn min_float(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}

/// Two circles (`b2CollideCircles`).
pub(super) fn collide_circles(
    center_a: Vec2,
    radius_a: f32,
    center_b: Vec2,
    radius_b: f32,
    xf: Transform,
    speculative: f32,
) -> Local {
    let mut manifold = Local::default();

    let point_a = center_a;
    let point_b = transform_point(xf.p, xf.q, center_b);

    let (distance, normal) = length_and_normalize(point_b - point_a);

    let separation = distance - radius_a - radius_b;
    if separation > speculative {
        return manifold;
    }

    let c_a = mul_add(point_a, radius_a, normal);
    let c_b = mul_add(point_b, -radius_b, normal);

    manifold.normal = normal;
    manifold.push(lerp(c_a, c_b, 0.5), separation);
    manifold
}

/// A capsule and a circle (`b2CollideCapsuleAndCircle`).
pub(super) fn collide_capsule_and_circle(
    (p1, p2, radius_a): (Vec2, Vec2, f32),
    center_b: Vec2,
    radius_b: f32,
    xf: Transform,
    speculative: f32,
) -> Local {
    let mut manifold = Local::default();

    // Compute circle position in the frame of the capsule.
    let p_b = transform_point(xf.p, xf.q, center_b);

    // Compute closest point
    let e = p2 - p1;

    // dot(p - pA, e) = 0
    // dot(p - (p1 + s1 * e), e) = 0
    // s1 = dot(p - p1, e)
    let s1 = dot(p_b - p1, e);
    let s2 = dot(p2 - p_b, e);
    let p_a = if s1 < 0.0 {
        // p1 region
        p1
    } else if s2 < 0.0 {
        // p2 region
        p2
    } else {
        // circle colliding with segment interior
        let s = s1 / dot(e, e);
        mul_add(p1, s, e)
    };

    let (distance, normal) = length_and_normalize(p_b - p_a);

    let separation = distance - radius_a - radius_b;
    if separation > speculative {
        return manifold;
    }

    let c_a = mul_add(p_a, radius_a, normal);
    let c_b = mul_add(p_b, -radius_b, normal);

    manifold.normal = normal;
    manifold.push(lerp(c_a, c_b, 0.5), separation);
    manifold
}

/// A polygon and a circle (`b2CollidePolygonAndCircle`).
pub(super) fn collide_polygon_and_circle(
    polygon_a: &Polygon,
    center_b: Vec2,
    radius_b: f32,
    xf: Transform,
    speculative: f32,
) -> Local {
    let mut manifold = Local::default();

    // Compute circle position in the frame of the polygon.
    let center = transform_point(xf.p, xf.q, center_b);
    let radius_a = polygon_a.radius;
    let radius = radius_a + radius_b;

    // Find the min separating edge.
    let mut normal_index = 0;
    let mut separation = -f32::MAX;
    let vertex_count = polygon_a.count;
    let vertices = &polygon_a.vertices;
    let normals = &polygon_a.normals;

    for i in 0..vertex_count {
        let s = dot(normals[i], center - vertices[i]);
        if s > separation {
            separation = s;
            normal_index = i;
        }
    }

    if separation > radius + speculative {
        return manifold;
    }

    // Vertices of the reference edge.
    let vert_index1 = normal_index;
    let vert_index2 = if vert_index1 + 1 < vertex_count {
        vert_index1 + 1
    } else {
        0
    };
    let v1 = vertices[vert_index1];
    let v2 = vertices[vert_index2];

    // Compute barycentric coordinates
    let u1 = dot(center - v1, v2 - v1);
    let u2 = dot(center - v2, v1 - v2);

    if u1 < 0.0 && separation > f32::EPSILON {
        // Circle center is closest to v1 and safely outside the polygon
        let normal = normalize(center - v1);
        separation = dot(center - v1, normal);
        if separation > radius + speculative {
            return manifold;
        }

        let c_a = mul_add(v1, radius_a, normal);
        let c_b = mul_sub(center, radius_b, normal);
        manifold.normal = normal;
        manifold.push(lerp(c_a, c_b, 0.5), dot(c_b - c_a, normal));
    } else if u2 < 0.0 && separation > f32::EPSILON {
        // Circle center is closest to v2 and safely outside the polygon
        let normal = normalize(center - v2);
        separation = dot(center - v2, normal);
        if separation > radius + speculative {
            return manifold;
        }

        let c_a = mul_add(v2, radius_a, normal);
        let c_b = mul_sub(center, radius_b, normal);
        manifold.normal = normal;
        manifold.push(lerp(c_a, c_b, 0.5), dot(c_b - c_a, normal));
    } else {
        // Circle center is between v1 and v2. Center may be inside polygon
        let normal = normals[normal_index];
        manifold.normal = normal;

        // cA is the projection of the circle center onto to the reference edge
        let c_a = mul_add(center, radius_a - dot(center - v1, normal), normal);

        // cB is the deepest point on the circle with respect to the reference edge
        let c_b = mul_sub(center, radius_b, normal);

        // The contact point is the midpoint
        manifold.push(lerp(c_a, c_b, 0.5), separation - radius);
    }

    manifold
}

/// `b2InvMulTransforms` for poses given as a position and a rotation.
pub(super) fn relative(position_a: Vec2, q_a: Rot, position_b: Vec2, q_b: Rot) -> Transform {
    crate::convex::d2::inv_mul_transforms(
        Transform {
            p: position_a,
            q: q_a,
        },
        Transform {
            p: position_b,
            q: q_b,
        },
    )
}
