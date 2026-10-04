//! CPU triangle extraction and ray intersections for caller-owned scene geometry.
use super::asset::CpuMesh;
use glam::Vec3;

/// Expand valid triangle-list mesh indices into positions for geometric queries.
///
/// Indices must address existing vertices. Incomplete trailing index groups are
/// ignored, matching triangle-list topology. No material sidedness is applied.
pub fn triangles(meshes: &[CpuMesh]) -> Vec<[Vec3; 3]> {
    meshes
        .iter()
        .flat_map(|m| {
            m.indices.as_chunks::<3>().0.iter().map(|i| {
                [
                    Vec3::from_array(m.vertices[i[0] as usize].position),
                    Vec3::from_array(m.vertices[i[1] as usize].position),
                    Vec3::from_array(m.vertices[i[2] as usize].position),
                ]
            })
        })
        .collect()
}

/// Return the nearest positive, two-sided ray intersection, bounded by `max`.
///
/// `ray` must be a unit direction for the result to be a distance in world units.
/// Hits at the origin are ignored; a missed ray returns `max`. These CPU queries
/// do not account for material visibility or transparency.
pub fn obstructed_distance(triangles: &[[Vec3; 3]], origin: Vec3, ray: Vec3, max: f32) -> f32 {
    let mut closest = max;
    for [a, b, c] in triangles {
        let e1 = *b - *a;
        let e2 = *c - *a;
        let p = ray.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-6 {
            continue;
        }
        let inv = 1.0 / det;
        let t = origin - *a;
        let u = t.dot(p) * inv;
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let q = t.cross(e1);
        let v = ray.dot(q) * inv;
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        let distance = e2.dot(q) * inv;
        if distance > 0.0 && distance < closest {
            closest = distance;
        }
    }
    closest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::Vertex;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test(unsupported = test)]
    fn indexed_surfaces_return_the_nearest_hit_from_either_side() {
        let meshes: Vec<_> = [5.0, 2.0]
            .into_iter()
            .map(|z| CpuMesh {
                vertices: [[0., 0., z], [1., 0., z], [1., 1., z], [0., 1., z]]
                    .map(|position| Vertex {
                        tangent: [0.0; 4],
                        lightmap_bounds: [0., 0., 1., 1.],
                        lightmap_uv: [0.; 2],
                        position,
                        normal: [0., 0., 1.],
                        uv: [0.; 2],
                        color: [1.; 4],
                    })
                    .into(),
                indices: vec![2, 0, 1, 2, 3, 0],
                material: 0,
                deformation: Default::default(),
            })
            .collect();
        let faces = triangles(&meshes);
        // Two squares at z=2 and z=5. Rays sample both triangles of each square;
        // the expected distances follow directly from these planes.
        for (x, y) in [(0.25, 0.75), (0.75, 0.25)] {
            let origin = Vec3::new(x, y, 0.);
            assert!((obstructed_distance(&faces, origin, Vec3::Z, 10.) - 2.).abs() < 1e-6);
            assert!(
                (obstructed_distance(&faces, origin + Vec3::Z * 6., -Vec3::Z, 10.) - 1.).abs()
                    < 1e-6
            );
            assert_eq!(obstructed_distance(&faces, origin, -Vec3::Z, 10.), 10.);
            assert_eq!(obstructed_distance(&faces, origin, Vec3::X, 10.), 10.);
            assert_eq!(obstructed_distance(&faces, origin, Vec3::Z, 1.), 1.);
        }
        assert_eq!(
            obstructed_distance(&faces, Vec3::new(2., 2., 0.), Vec3::Z, 10.),
            10.
        );
    }
}
