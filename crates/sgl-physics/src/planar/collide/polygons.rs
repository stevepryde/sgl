//! Two polygons, each grown by a radius: the separating-axis test on both
//! polygons' edge normals and the reference and incident edges clipped
//! against each other.
//!
//! Ported from `Box2D` v3.2.0 (commit
//! `178495ba35695aefe9c321e569d2277f06db3b90`), `src/manifold.c`
//! (`b2ClipPolygons`, `b2FindMaxSeparation`, its scalar path, and
//! `b2CollidePolygons`, which `b2CollidePolygonAndCapsule` calls) and
//! `src/distance.c` (`b2ComputeSegmentDistance`). Copyright 2023 Erin
//! Catto, MIT: `LICENSE-box2d.txt` beside this crate's manifest. Departures
//! and algorithm notes are in `PROVENANCE.md`.

// The source names the shapes A and B, their vertices v11, v12, v21, v22
// and the fractions f1 and f2, and tells a vertex from an edge by comparing
// a clamped fraction with 0 and 1 exactly; the port keeps those names and
// those comparisons.
#![allow(
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::float_cmp
)]

use sgl_core::math::Vec2;

use super::{Local, Point, Polygon, clamp, distance_squared, lerp, min_float, mul_add};
use crate::convex::d2::{Transform, cross_sv, dot, rotate, transform_point};

/// The closest points of two segments (`b2ComputeSegmentDistance`): the
/// fraction along each and the squared distance between them.
fn segment_distance(p1: Vec2, q1: Vec2, p2: Vec2, q2: Vec2) -> (f32, f32, f32) {
    let d1 = q1 - p1;
    let d2 = q2 - p2;
    let r = p1 - p2;
    let dd1 = dot(d1, d1);
    let dd2 = dot(d2, d2);
    let rd1 = dot(r, d1);
    let rd2 = dot(r, d2);

    let eps_sqr = f32::EPSILON * f32::EPSILON;

    let (fraction1, fraction2) = if dd1 < eps_sqr || dd2 < eps_sqr {
        // Handle all degeneracies
        if dd1 >= eps_sqr {
            // Segment 2 is degenerate
            (clamp(-rd1 / dd1, 0.0, 1.0), 0.0)
        } else if dd2 >= eps_sqr {
            // Segment 1 is degenerate
            (0.0, clamp(rd2 / dd2, 0.0, 1.0))
        } else {
            (0.0, 0.0)
        }
    } else {
        // Non-degenerate segments
        let d12 = dot(d1, d2);

        let denominator = dd1 * dd2 - d12 * d12;

        // Fraction on segment 1
        let mut f1 = 0.0;
        if denominator != 0.0 {
            // not parallel
            f1 = clamp((d12 * rd2 - rd1 * dd2) / denominator, 0.0, 1.0);
        }

        // Compute point on segment 2 closest to p1 + f1 * d1
        let mut f2 = (d12 * f1 + rd2) / dd2;

        // Clamping of segment 2 requires a do over on segment 1
        if f2 < 0.0 {
            f2 = 0.0;
            f1 = clamp(-rd1 / dd1, 0.0, 1.0);
        } else if f2 > 1.0 {
            f2 = 1.0;
            f1 = clamp((d12 - rd1) / dd1, 0.0, 1.0);
        }

        (f1, f2)
    };

    let closest1 = mul_add(p1, fraction1, d1);
    let closest2 = mul_add(p2, fraction2, d2);
    (fraction1, fraction2, distance_squared(closest1, closest2))
}

/// The two contact points of a reference edge and an incident edge
/// (`b2ClipPolygons`): the incident edge clipped to the reference edge's
/// extent, each point midway between the surfaces.
fn clip_polygons(
    poly_a: &Polygon,
    poly_b: &Polygon,
    edge_a: usize,
    edge_b: usize,
    flip: bool,
) -> Local {
    let mut manifold = Local::default();

    // reference polygon, then incident polygon
    let (poly1, i11, i12, poly2, i21, i22) = if flip {
        (
            poly_b,
            edge_b,
            if edge_b + 1 < poly_b.count {
                edge_b + 1
            } else {
                0
            },
            poly_a,
            edge_a,
            if edge_a + 1 < poly_a.count {
                edge_a + 1
            } else {
                0
            },
        )
    } else {
        (
            poly_a,
            edge_a,
            if edge_a + 1 < poly_a.count {
                edge_a + 1
            } else {
                0
            },
            poly_b,
            edge_b,
            if edge_b + 1 < poly_b.count {
                edge_b + 1
            } else {
                0
            },
        )
    };

    let normal = poly1.normals[i11];

    // Reference edge vertices
    let v11 = poly1.vertices[i11];
    let v12 = poly1.vertices[i12];

    // Incident edge vertices
    let v21 = poly2.vertices[i21];
    let v22 = poly2.vertices[i22];

    let tangent = cross_sv(1.0, normal);

    let lower1 = 0.0;
    let upper1 = dot(v12 - v11, tangent);

    // Incident edge points opposite of tangent due to CCW winding
    let upper2 = dot(v21 - v11, tangent);
    let lower2 = dot(v22 - v11, tangent);

    // Are the segments disjoint?
    if upper2 < lower1 || upper1 < lower2 {
        return manifold;
    }

    let mut v_lower = if lower2 < lower1 && upper2 - lower2 > f32::EPSILON {
        lerp(v22, v21, (lower1 - lower2) / (upper2 - lower2))
    } else {
        v22
    };

    let mut v_upper = if upper2 > upper1 && upper2 - lower2 > f32::EPSILON {
        lerp(v22, v21, (upper1 - lower2) / (upper2 - lower2))
    } else {
        v21
    };

    let separation_lower = dot(v_lower - v11, normal);
    let separation_upper = dot(v_upper - v11, normal);

    let r1 = poly1.radius;
    let r2 = poly2.radius;

    // Put contact points at midpoint, accounting for radii
    v_lower = mul_add(v_lower, 0.5 * (r1 - r2 - separation_lower), normal);
    v_upper = mul_add(v_upper, 0.5 * (r1 - r2 - separation_upper), normal);

    let radius = r1 + r2;

    if flip {
        manifold.normal = -normal;
        manifold.push(v_upper, separation_upper - radius);
        manifold.push(v_lower, separation_lower - radius);
    } else {
        manifold.normal = normal;
        manifold.push(v_lower, separation_lower - radius);
        manifold.push(v_upper, separation_upper - radius);
    }

    manifold
}

/// The largest separation of `poly2` from `poly1` along `poly1`'s edge
/// normals, and the edge it is found on (`b2FindMaxSeparation`, its
/// scalar path).
fn find_max_separation(poly1: &Polygon, poly2: &Polygon) -> (f32, usize) {
    let count1 = poly1.count;
    let count2 = poly2.count;
    let n1s = &poly1.normals;
    let v1s = &poly1.vertices;
    let v2s = &poly2.vertices;

    let mut best_index = 0;
    let mut max_separation = -f32::MAX;
    for i in 0..count1 {
        // Get poly1 normal in frame2.
        let n = n1s[i];
        let v1 = v1s[i];

        // Find the deepest point for normal i.
        let mut si = f32::MAX;
        for v2 in &v2s[..count2] {
            let sij = dot(n, *v2 - v1);
            if sij < si {
                si = sij;
            }
        }

        if si > max_separation {
            max_separation = si;
            best_index = i;
        }
    }

    (max_separation, best_index)
}

/// Two polygons, each grown by its radius (`b2CollidePolygons`).
///
/// The separating-axis test on both polygons' edge normals finds the
/// reference edge and its separation. Polygons further apart than the
/// speculative distance give nothing. When they are clearly apart (by more
/// than a tenth of `slop` along an axis) the closest features of the
/// reference and incident edges decide between a single vertex contact and
/// the clipped edges; otherwise the incident edge is clipped against the
/// reference edge.
// One function, as in the source, so it reads against it.
#[allow(clippy::too_many_lines)]
pub(crate) fn collide_polygons(
    polygon_a: &Polygon,
    polygon_b: &Polygon,
    xf: Transform,
    slop: f32,
    speculative: f32,
) -> Local {
    let origin = polygon_a.vertices[0];
    let linear_slop = slop;
    let speculative_distance = speculative;

    // Shift to the origin in frame A for round-off, a pure translation in
    // A's frame
    let xfs = Transform {
        p: xf.p - origin,
        q: xf.q,
    };

    let mut local_poly_a = Polygon::empty(polygon_a.count, polygon_a.radius);
    local_poly_a.vertices[0] = Vec2::ZERO;
    local_poly_a.normals[0] = polygon_a.normals[0];
    for i in 1..local_poly_a.count {
        local_poly_a.vertices[i] = polygon_a.vertices[i] - origin;
        local_poly_a.normals[i] = polygon_a.normals[i];
    }

    // Put polyB in polyA's frame to reduce round-off error
    let mut local_poly_b = Polygon::empty(polygon_b.count, polygon_b.radius);
    for i in 0..local_poly_b.count {
        local_poly_b.vertices[i] = transform_point(xfs.p, xfs.q, polygon_b.vertices[i]);
        local_poly_b.normals[i] = rotate(xfs.q, polygon_b.normals[i]);
    }

    let radius = local_poly_a.radius + local_poly_b.radius;

    let (separation_a, mut edge_a) = find_max_separation(&local_poly_a, &local_poly_b);
    if separation_a > speculative_distance + radius {
        return Local::default();
    }

    let (separation_b, mut edge_b) = find_max_separation(&local_poly_b, &local_poly_a);
    if separation_b > speculative_distance + radius {
        return Local::default();
    }

    // Find incident edge
    let flip = if separation_a >= separation_b {
        let search_direction = local_poly_a.normals[edge_a];

        // Find the incident edge on polyB
        edge_b = incident_edge(&local_poly_b, search_direction);
        false
    } else {
        let search_direction = local_poly_b.normals[edge_b];

        // Find the incident edge on polyA
        edge_a = incident_edge(&local_poly_a, search_direction);
        true
    };

    let mut manifold;

    // Using slop here to ensure vertex-vertex normal vectors can be safely
    // normalized
    if separation_a > 0.1 * linear_slop || separation_b > 0.1 * linear_slop {
        // Edges are disjoint. Find closest points between reference edge
        // and incident edge. Reference edge on polygon A
        let i11 = edge_a;
        let i12 = if edge_a + 1 < local_poly_a.count {
            edge_a + 1
        } else {
            0
        };
        let i21 = edge_b;
        let i22 = if edge_b + 1 < local_poly_b.count {
            edge_b + 1
        } else {
            0
        };

        let v11 = local_poly_a.vertices[i11];
        let v12 = local_poly_a.vertices[i12];
        let v21 = local_poly_b.vertices[i21];
        let v22 = local_poly_b.vertices[i22];

        let (fraction1, fraction2, distance_squared) = segment_distance(v11, v12, v21, v22);
        let distance = distance_squared.sqrt();
        let separation = distance - radius;

        if distance - radius > speculative_distance {
            // This can happen in the vertex-vertex case
            return Local::default();
        }

        // Attempt to clip edges
        manifold = clip_polygons(&local_poly_a, &local_poly_b, edge_a, edge_b, flip);

        let mut min_separation = f32::MAX;
        for point in &manifold.points[..manifold.count] {
            min_separation = min_float(min_separation, point.separation);
        }

        // Does vertex-vertex have substantially larger separation?
        if separation + 0.1 * linear_slop < min_separation {
            let vertices = if fraction1 == 0.0 && fraction2 == 0.0 {
                // v11 - v21
                Some((v11, v21))
            } else if fraction1 == 0.0 && fraction2 == 1.0 {
                // v11 - v22
                Some((v11, v22))
            } else if fraction1 == 1.0 && fraction2 == 0.0 {
                // v12 - v21
                Some((v12, v21))
            } else if fraction1 == 1.0 && fraction2 == 1.0 {
                // v12 - v22
                Some((v12, v22))
            } else {
                None
            };
            if let Some((v1, v2)) = vertices {
                let mut normal = v2 - v1;
                let inv_distance = 1.0 / distance;
                normal.x *= inv_distance;
                normal.y *= inv_distance;

                let c1 = mul_add(v1, local_poly_a.radius, normal);
                let c2 = mul_add(v2, -local_poly_b.radius, normal);

                manifold.normal = normal;
                manifold.points[0] = Point {
                    point: lerp(c1, c2, 0.5),
                    separation: distance - radius,
                };
                manifold.count = 1;
            }
        }
    } else {
        // Polygons overlap
        manifold = clip_polygons(&local_poly_a, &local_poly_b, edge_a, edge_b, flip);
    }

    // Undo the origin shift so points are in frame A
    for point in &mut manifold.points[..manifold.count] {
        point.point += origin;
    }

    manifold
}

/// The edge of `polygon` whose normal is most against `direction`: the
/// incident edge, the first of equals.
fn incident_edge(polygon: &Polygon, direction: Vec2) -> usize {
    let mut edge = 0;
    let mut min_dot = f32::MAX;
    for (i, normal) in polygon.normals[..polygon.count].iter().enumerate() {
        let d = dot(direction, *normal);
        if d < min_dot {
            min_dot = d;
            edge = i;
        }
    }
    edge
}
