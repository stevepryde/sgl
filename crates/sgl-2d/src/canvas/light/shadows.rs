//! CPU shadow extrusion and the GPU vertex-buffer budget.

use glam::Vec2;

/// One vertex is the Float32x2 position consumed by `vs_shadow`.
pub(super) type ShadowVertex = [f32; 2];

/// A single frame shares one buffer; draw-range endpoints must fit u32 too.
pub(super) fn max_shadow_vertices(max_buffer_size: u64) -> u64 {
    (max_buffer_size / size_of::<ShadowVertex>() as u64).min(u64::from(u32::MAX))
}

/// Power-of-two growth, stopping at the device/draw budget when it is smaller.
pub(super) fn grown_shadow_capacity(needed: u64, max_vertices: u64) -> u64 {
    needed.next_power_of_two().min(max_vertices)
}

/// Extrude shadow geometry for one light: for **every** edge `(a, b)` of every
/// occluder polygon near the light, append the quad
/// `[a, b, b + dir(b)·far, a + dir(a)·far]` (as two triangles, 6 vertices)
/// where `dir(v) = normalize(v - light_pos)`.
///
/// When the edge subtends more than 90° at the light (a long wall close to
/// it), the straight far edge of that quad would pass close to the light and
/// leave the shadow behind the wall lit. Such an edge gets a third far
/// vertex along its normal, away from the light (a convex pentagon, 9
/// vertices), so every far edge stays at least `far·cos 45°` from the light.
/// Pass `far ≥ 2 × half_extent.max_element()` (so `far·cos 45°` reaches the
/// footprint's corners) and the shadow covers everything behind the edge
/// inside the footprint.
///
/// The union over all edges is exactly "everything behind the first surface
/// seen from the light" (Godot's occlusion shape): front-edge quads cover the
/// polygon interior and beyond; back-edge quads are subsets of that union —
/// so the result is winding-independent. Polygons whose AABB misses the
/// light's footprint AABB (`pos ± half_extent`) are culled.
pub fn shadow_triangles(
    light_pos: Vec2,
    half_extent: Vec2,
    occluders: &[Vec<Vec2>],
    far: f32,
    out: &mut Vec<[f32; 2]>,
) {
    shadow_triangles_limited(light_pos, half_extent, occluders, far, out, u64::MAX);
}

/// Append complete edge extrusions until the frame-wide vertex budget is full.
/// Stop at the first edge that does not fit; never emit a partial quad/pentagon.
/// Return true on exhaustion so later lights also drop their geometry.
pub(super) fn shadow_triangles_limited(
    light_pos: Vec2,
    half_extent: Vec2,
    occluders: &[Vec<Vec2>],
    far: f32,
    out: &mut Vec<[f32; 2]>,
    max_vertices: u64,
) -> bool {
    if max_vertices.saturating_sub(out.len() as u64) < 6 {
        return true;
    }
    let range_min = light_pos - half_extent;
    let range_max = light_pos + half_extent;
    for poly in occluders {
        if poly.len() < 2 {
            continue;
        }
        // AABB cull against the light footprint.
        let mut min = poly[0];
        let mut max = poly[0];
        for p in &poly[1..] {
            min = min.min(*p);
            max = max.max(*p);
        }
        if min.x > range_max.x || max.x < range_min.x || min.y > range_max.y || max.y < range_min.y
        {
            continue;
        }
        let extrude = |v: Vec2| -> Option<Vec2> {
            let d = v - light_pos;
            let len = d.length();
            if len < 1e-4 {
                return None; // Vertex on the light: degenerate, skip the edge.
            }
            Some(v + d / len * far)
        };
        let mut j = poly.len() - 1;
        for i in 0..poly.len() {
            let (a, b) = (poly[j], poly[i]);
            j = i;
            let (Some(ea), Some(eb)) = (extrude(a), extrude(b)) else {
                continue;
            };
            let (da, db) = ((ea - a).normalize(), (eb - b).normalize());
            let edge = b - a;
            let side = edge.perp_dot(a - light_pos);
            // Past 90°, cap the far side with a vertex along the edge's
            // normal away from the light, as far from it as the farther
            // extruded end. The span is over 90° only when the light's foot
            // on the edge's line lies between `a` and `b`, so the normal
            // splits it into two parts under 90° each, and the vertex lies
            // beyond the straight far edge, keeping the pentagon convex. A
            // light exactly on the edge's line has no area behind it.
            let capped = da.dot(db) < 0.0 && side != 0.0;
            let edge_vertices = if capped { 9 } else { 6 };
            if max_vertices.saturating_sub(out.len() as u64) < edge_vertices {
                return true;
            }
            if capped {
                let reach = (ea - light_pos).length().max((eb - light_pos).length());
                let normal = (edge.perp() * side.signum()).normalize();
                let em = light_pos + normal * reach;
                out.extend_from_slice(&[
                    a.to_array(),
                    b.to_array(),
                    eb.to_array(),
                    a.to_array(),
                    eb.to_array(),
                    em.to_array(),
                    a.to_array(),
                    em.to_array(),
                    ea.to_array(),
                ]);
                continue;
            }
            out.extend_from_slice(&[
                a.to_array(),
                b.to_array(),
                eb.to_array(),
                a.to_array(),
                eb.to_array(),
                ea.to_array(),
            ]);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // --- shadow extrusion -----------------------------------------------------

    fn square() -> Vec<Vec2> {
        vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(32.0, 0.0),
            Vec2::new(32.0, 32.0),
            Vec2::new(0.0, 32.0),
        ]
    }

    fn point_in_tri(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
        let sign = |p1: Vec2, p2: Vec2, p3: Vec2| (p1 - p3).perp_dot(p2 - p3);
        let d1 = sign(p, a, b);
        let d2 = sign(p, b, c);
        let d3 = sign(p, c, a);
        let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(has_neg && has_pos)
    }

    fn in_shadow(verts: &[[f32; 2]], p: Vec2) -> bool {
        verts.chunks_exact(3).any(|t| {
            point_in_tri(
                p,
                Vec2::from_array(t[0]),
                Vec2::from_array(t[1]),
                Vec2::from_array(t[2]),
            )
        })
    }

    /// A square occluder with the light to its left: points behind (right of)
    /// the square are shadowed, points beside or in front are lit, and the
    /// square's own interior is shadowed past the front face (Godot's "behind
    /// the first surface" shape).
    #[wasm_bindgen_test(unsupported = test)]
    fn shadow_covers_behind_the_occluder() {
        let light = Vec2::new(-50.0, 16.0);
        let mut verts = Vec::new();
        shadow_triangles(light, Vec2::splat(500.0), &[square()], 4096.0, &mut verts);
        assert!(!verts.is_empty());
        assert_eq!(verts.len() % 3, 0);

        assert!(in_shadow(&verts, Vec2::new(100.0, 16.0)), "behind the wall");
        assert!(in_shadow(&verts, Vec2::new(16.0, 16.0)), "wall interior");
        assert!(!in_shadow(&verts, Vec2::new(-20.0, 16.0)), "light side");
        assert!(!in_shadow(&verts, Vec2::new(16.0, -60.0)), "beside/above");
        assert!(!in_shadow(&verts, Vec2::new(16.0, 90.0)), "beside/below");
    }

    /// #319: a light 2 px from a long wall (the edge subtends nearly 180°),
    /// or 0.05 px from a 2000 px one, still shadows everything behind the
    /// wall inside its footprint: with a straight far edge, `(0, 200)` and
    /// `(0, 290)` were lit.
    #[wasm_bindgen_test(unsupported = test)]
    fn shadow_behind_a_long_wall_close_to_the_light_reaches_the_footprint() {
        let walls = [
            vec![Vec2::new(-500.0, 2.0), Vec2::new(500.0, 10.0)],
            vec![Vec2::new(-1000.0, 0.05), Vec2::new(1000.0, 0.05)],
        ];
        let half = Vec2::splat(300.0);
        let windings = walls
            .iter()
            .flat_map(|wall| [wall.clone(), wall.iter().rev().copied().collect()]);
        for winding in windings {
            let mut verts = Vec::new();
            shadow_triangles(
                Vec2::ZERO,
                half,
                &[winding],
                2.0 * half.max_element(),
                &mut verts,
            );
            for p in [
                Vec2::new(0.0, 200.0),
                Vec2::new(0.0, 290.0),
                Vec2::new(290.0, 290.0),
            ] {
                assert!(in_shadow(&verts, p), "{p:?} behind the wall is lit");
            }
            for p in [
                Vec2::new(0.0, -200.0),
                Vec2::new(290.0, -290.0),
                Vec2::new(0.0, -0.5),
            ] {
                assert!(
                    !in_shadow(&verts, p),
                    "{p:?} on the light's side is shadowed"
                );
            }
        }
    }

    /// The result is winding-independent (all edges extrude; the union is the
    /// same region).
    #[wasm_bindgen_test(unsupported = test)]
    fn shadow_is_winding_independent() {
        let light = Vec2::new(-50.0, 16.0);
        let mut reversed = square();
        reversed.reverse();
        let mut a = Vec::new();
        let mut b = Vec::new();
        shadow_triangles(light, Vec2::splat(500.0), &[square()], 4096.0, &mut a);
        shadow_triangles(light, Vec2::splat(500.0), &[reversed], 4096.0, &mut b);
        for p in [
            Vec2::new(100.0, 16.0),
            Vec2::new(16.0, 16.0),
            Vec2::new(-20.0, 16.0),
            Vec2::new(16.0, -60.0),
        ] {
            assert_eq!(in_shadow(&a, p), in_shadow(&b, p), "{p:?}");
        }
    }

    /// Occluders outside the light's footprint AABB are culled entirely.
    #[wasm_bindgen_test(unsupported = test)]
    fn shadow_culls_far_occluders() {
        let mut verts = Vec::new();
        shadow_triangles(
            Vec2::new(1000.0, 1000.0),
            Vec2::splat(100.0), // footprint (900..1100)² — square at origin is far away
            &[square()],
            4096.0,
            &mut verts,
        );
        assert!(verts.is_empty());
    }

    /// Degenerate case: a vertex exactly on the light produces no NaN
    /// geometry (its edges are skipped).
    #[wasm_bindgen_test(unsupported = test)]
    fn shadow_skips_vertex_on_light() {
        let mut verts = Vec::new();
        shadow_triangles(
            Vec2::ZERO,
            Vec2::splat(500.0),
            &[square()],
            4096.0,
            &mut verts,
        );
        assert!(verts.iter().flatten().all(|c| c.is_finite()));
    }

    /// #630: the byte budget is injected, but the oracle is the region
    /// actually shadowed. The first wall stays; a later wall and edges that
    /// would only fit partially contribute no shadow.
    #[wasm_bindgen_test(unsupported = test)]
    fn limited_geometry_keeps_whole_edges_and_drops_surplus() {
        for (wall, retained, probe) in [
            (
                vec![Vec2::new(2.0, -0.5), Vec2::new(2.0, 0.5)],
                6,
                Vec2::new(10.0, 0.0),
            ),
            (
                vec![Vec2::new(-20.0, 2.0), Vec2::new(20.0, 2.0)],
                9,
                Vec2::new(0.0, 10.0),
            ),
        ] {
            // Leave only part of the next edge's space; it must be dropped.
            for spare in 0..6 {
                let byte_limit = (retained + spare) * 8 + 3;
                let budget = max_shadow_vertices(byte_limit);
                let mut verts = Vec::new();
                assert!(shadow_triangles_limited(
                    Vec2::ZERO,
                    Vec2::splat(100.0),
                    std::slice::from_ref(&wall),
                    200.0,
                    &mut verts,
                    budget,
                ));
                assert_eq!(verts.len() as u64, retained, "partial edge survived");
                assert!(in_shadow(&verts, probe), "retained wall lost its shadow");
                assert!(!in_shadow(&verts, -probe), "light side became shadowed");
                // Growth must also stop at the byte budget even when the
                // required geometry rounds to a larger power of two.
                let capacity = grown_shadow_capacity(verts.len() as u64, budget);
                assert!(capacity >= verts.len() as u64);
                assert!(capacity * size_of::<ShadowVertex>() as u64 <= byte_limit);
            }
        }

        // Accumulation across lights consumes the same remaining budget.
        let mut verts = Vec::new();
        let budget = max_shadow_vertices(14 * 8);
        assert!(!shadow_triangles_limited(
            Vec2::ZERO,
            Vec2::splat(100.0),
            &[vec![Vec2::new(2.0, -0.5), Vec2::new(2.0, 0.5)]],
            200.0,
            &mut verts,
            budget,
        ));
        assert!(shadow_triangles_limited(
            Vec2::ZERO,
            Vec2::splat(100.0),
            &[vec![Vec2::new(-20.0, 2.0), Vec2::new(20.0, 2.0)]],
            200.0,
            &mut verts,
            budget,
        ));
        assert!(in_shadow(&verts, Vec2::new(10.0, 0.0)));
        assert!(
            !in_shadow(&verts, Vec2::new(0.0, 10.0)),
            "surplus wall cast a shadow"
        );
    }
}
