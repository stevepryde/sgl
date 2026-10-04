use super::*;
use crate::asset::{CpuMesh, Vertex};
use glam::camera;
use wasm_bindgen_test::wasm_bindgen_test;

fn mesh(triangles: impl IntoIterator<Item = [Vec3; 3]>) -> CpuMesh {
    let vertices: Vec<_> = triangles
        .into_iter()
        .flatten()
        .map(|p| Vertex {
            tangent: [0.0; 4],
            lightmap_bounds: [0., 0., 1., 1.],
            lightmap_uv: [0.; 2],
            position: p.to_array(),
            normal: Vec3::Z.to_array(),
            uv: [0.; 2],
            color: [1.; 4],
        })
        .collect();
    let indices = (0..vertices.len() as u32).collect();
    CpuMesh {
        vertices,
        indices,
        material: 0,
        deformation: Default::default(),
    }
}

// Independent oracle clips each actual triangle, rather than testing any bound.
// The six inequalities are WebGPU's homogeneous clip volume.
fn clipped_triangle(mut polygon: Vec<DVec4>) -> bool {
    for plane in 0..6 {
        let distance = |p: DVec4| match plane {
            0 => p.x + p.w,
            1 => p.w - p.x,
            2 => p.y + p.w,
            3 => p.w - p.y,
            4 => p.z,
            _ => p.w - p.z,
        };
        let mut output = Vec::new();
        for edge in 0..polygon.len() {
            let a = polygon[edge];
            let b = polygon[(edge + 1) % polygon.len()];
            let da = distance(a);
            let db = distance(b);
            if da >= 0. {
                output.push(a);
            }
            if (da >= 0.) != (db >= 0.) {
                output.push(a.lerp(b, da / (da - db)));
            }
        }
        polygon = output;
    }
    !polygon.is_empty()
}

#[wasm_bindgen_test(unsupported = test)]
fn retained_ranges_never_drop_clipped_triangles_under_affine_cameras() {
    let mesh = mesh((-20..20).flat_map(|z| {
        (-20..20).map(move |x| {
            let p = Vec3::new(x as f32 * 3., 0., z as f32 * 3.);
            [p, p + Vec3::new(2., 0., 0.), p + Vec3::new(0., 2., -1.)]
        })
    }));
    let ranges = MeshRanges::new(&mesh.vertices, &mesh.indices);
    let projection = crate::perspective(1.1, 1.3, 0.5);
    for location in [Vec3::ZERO, Vec3::new(1_000_000., 100., -300_000.)] {
        for scale in [Vec3::ONE, Vec3::new(-2., 0.3, 1.4)] {
            for yaw in [0., 0.7, 1.9] {
                let pose = Mat4::from_translation(location)
                    * Mat4::from_rotation_y(yaw)
                    * Mat4::from_scale(scale);
                let view = camera::rh::view::look_at_mat4(
                    location + Vec3::new(1., 3., 4.),
                    location - Vec3::Z * 10.,
                    Vec3::Y,
                );
                let jitter = [0.013, -0.007];
                let frustum = Frustum::new(view, projection, pose, jitter);
                let mut submitted = vec![false; mesh.indices.len() / 3];
                ranges.visible(Some(&frustum), |range| {
                    for primitive in range.start / 3..range.end / 3 {
                        assert!(
                            !submitted[primitive as usize],
                            "duplicate primitive submission"
                        );
                        submitted[primitive as usize] = true;
                    }
                });
                for (primitive, triangle) in mesh.vertices.chunks_exact(3).enumerate() {
                    let clip = triangle
                        .iter()
                        .map(|v| {
                            let p = Vec3::from_array(v.position).as_dvec3().extend(1.);
                            let mut p =
                                projection.as_dmat4() * (view.as_dmat4() * (pose.as_dmat4() * p));
                            p.x += 2. * f64::from(jitter[0]) * p.w;
                            p.y += 2. * f64::from(jitter[1]) * p.w;
                            p
                        })
                        .collect();
                    if clipped_triangle(clip) {
                        assert!(
                            submitted[primitive],
                            "dropped visible primitive {primitive}"
                        );
                    }
                }
            }
        }
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn whole_world_bounds_do_not_prevent_eliminating_distant_ranges() {
    // A single indexed mesh covers five widely separated patches. The camera
    // sees only the middle patch; whole-mesh culling would retain every patch.
    let mesh = mesh((-2..=2).flat_map(|patch| {
        (0..257).map(move |triangle| {
            let p = Vec3::new(patch as f32 * 100. + (triangle % 16) as f32 * 0.01, 0., -5.);
            [p, p + Vec3::X * 0.01, p + Vec3::Y * 0.01]
        })
    }));
    let frustum = Frustum::new(
        Mat4::IDENTITY,
        crate::perspective(1., 1., 0.1),
        Mat4::IDENTITY,
        [0.; 2],
    );
    let ranges = MeshRanges::new(&mesh.vertices, &mesh.indices);
    let mut submitted = Vec::new();
    ranges.visible(Some(&frustum), |range| {
        submitted.extend(range.start / 3..range.end / 3)
    });
    assert!(
        submitted.len() < mesh.indices.len() / 3 / 2,
        "large batched mesh retained most offscreen geometry"
    );
    for visible in 514..771 {
        assert!(submitted.contains(&visible));
    }
    assert!(
        submitted.windows(2).all(|pair| pair[0] < pair[1]),
        "source primitive order changed"
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn clipping_retains_crossing_and_roundoff_boundary_geometry() {
    let identity = Frustum::new(Mat4::IDENTITY, Mat4::IDENTITY, Mat4::IDENTITY, [0.; 2]);
    for (lo, hi) in [
        ([-2., -2., 0.5], [2., 2., 0.5]), // covers viewport, no vertex inside
        ([-0.1, -0.1, -0.1], [0.1, 0.1, 0.1]), // z=0 clip-plane crossing
        ([1., 0., 0.5], [1., 1., 0.5]),   // side-plane tangent
        ([1. + f32::EPSILON, 0., 0.5], [1. + f32::EPSILON, 1., 0.5]),
    ] {
        assert!(
            identity.classify([Vec3::from_array(lo), Vec3::from_array(hi)]) != Relation::Outside
        );
    }
    for center in [
        Vec3::new(-2., 0., 0.5),
        Vec3::new(2., 0., 0.5),
        Vec3::new(0., -2., 0.5),
        Vec3::new(0., 2., 0.5),
        Vec3::new(0., 0., -1.),
        Vec3::new(0., 0., 2.),
    ] {
        assert!(
            identity.classify([center - Vec3::splat(0.1), center + Vec3::splat(0.1)])
                == Relation::Outside
        );
    }
}
