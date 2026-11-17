use super::*;
use glam::{DMat4, DVec3};
use wasm_bindgen_test::wasm_bindgen_test;

/// The range the faces are taken at; directions are tested 1 m out, well
/// within it and beyond the near plane.
const RANGE: f32 = 10.;
/// Sample points across each face's square, per side.
const GRID: usize = 49;
/// How far, in radians, a direction in a face's pyramid can lie from the
/// nearest of its grid's directions: the grid spaces points `h = 2 / (GRID -
/// 1)` apart on the face's square, 1 m from the light, so any point of it is
/// within `h / √2` of one, and a segment of that plane, at least 1 m from the
/// light, subtends at most its length in radians.
const GRID_MARGIN: f64 = 2. / (GRID - 1) as f64 / std::f64::consts::SQRT_2;

/// Each face's clip-from-light, as the renderer draws it.
fn face_clips() -> [DMat4; 6] {
    std::array::from_fn(|face| {
        View::local_shadow_face(face as u8, Vec3::ZERO, RANGE)
            .view_projection()
            .as_dmat4()
    })
}

/// The faces whose square holds unit `direction`.
fn holding(direction: DVec3) -> u8 {
    let mut faces = 0;
    for (face, clip) in face_clips().into_iter().enumerate() {
        let p = clip * direction.extend(1.);
        if p.w > 0. && p.x.abs() <= p.w && p.y.abs() <= p.w {
            faces |= 1 << face;
        }
    }
    faces
}

/// The faces holding each of `directions`.
fn faces_of(directions: &[Vec3]) -> u8 {
    directions
        .iter()
        .fold(0, |faces, d| faces | holding(d.normalize().as_dvec3()))
}

/// Each face's grid of directions across its square, unprojected through
/// the face's own view.
fn face_grids() -> Vec<Vec<DVec3>> {
    face_clips()
        .into_iter()
        .map(|clip| {
            let light_from_clip = clip.inverse();
            let ndc = |i: usize| -1. + 2. * i as f64 / (GRID - 1) as f64;
            (0..GRID * GRID)
                .map(|i| {
                    let p =
                        light_from_clip * DVec3::new(ndc(i % GRID), ndc(i / GRID), 0.5).extend(1.);
                    (p.truncate() / p.w).normalize()
                })
                .collect()
        })
        .collect()
}

// Plausible defects: a face drawn though the cone cannot reach it, as the
// opposite face of a spot wider than 45° was, or a face the cone reaches
// left undrawn. The oracle is the angular distance from the spot's axis to
// each face's pyramid, derived by hand: 0 for a face holding the axis; for
// an axis spot, 45° to each side face's nearest edge (the axis's projection
// onto that face's side plane, at 45° within it) and 125.3° to the opposite
// face's nearest corner (acos(-1/√3)), beyond any spot; for a spot along
// (1, 1, 1), 70.5° to each negative face's nearest corner, such as (-1, 1,
// 1), at acos(1/3); for a spot along (1, 1, 0), 35.3° to the ±Z faces'
// nearest corners (1, 1, ±1), at acos(2/√6), and 90° to -X and -Y, whose
// pyramids hold no direction in front of the plane through the light
// perpendicular to the axis. Angles fall either side of each threshold.
#[wasm_bindgen_test(unsupported = test)]
fn a_spot_draws_the_cube_faces_its_cone_reaches() {
    let degrees = |angle: f32| angle.to_radians();
    let axes = [
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
        Vec3::NEG_Z,
    ];
    for axis in axes {
        let sides: Vec<Vec3> = axes
            .into_iter()
            .filter(|side| side.dot(axis) == 0.)
            .collect();
        let facing = faces_of(&[axis]);
        let reached = faces_of(&sides) | facing;
        assert_eq!(cone_faces(axis, degrees(44.)), facing, "{axis} at 44°");
        for angle in [46., 50., 80., 89.] {
            assert_eq!(
                cone_faces(axis, degrees(angle)),
                reached,
                "{axis} at {angle}°"
            );
        }
    }
    let diagonal = Vec3::ONE.normalize();
    let positive = faces_of(&[Vec3::X, Vec3::Y, Vec3::Z]);
    for angle in [46., 60., 70.] {
        assert_eq!(cone_faces(diagonal, degrees(angle)), positive, "{angle}°");
    }
    for angle in [71., 80., 89.] {
        assert_eq!(cone_faces(diagonal, degrees(angle)), 0b11_1111, "{angle}°");
    }
    let edge = Vec3::new(1., 1., 0.).normalize();
    let near = faces_of(&[Vec3::X, Vec3::Y]);
    let beside = faces_of(&[Vec3::Z, Vec3::NEG_Z]);
    assert_eq!(cone_faces(edge, degrees(34.)), near);
    for angle in [36., 60., 89.] {
        assert_eq!(cone_faces(edge, degrees(angle)), near | beside, "{angle}°");
    }
}

// Plausible defects: a face dropped though some direction within the cone
// falls in it, at a pyramid's edge or corner or near a right angle, or a
// face kept that the cone cannot reach. The oracles are the faces' own
// views: every direction sampled within the cone falls in faces that are
// kept; and each face's grid of directions, which comes within
// `GRID_MARGIN` of every direction in its pyramid, has one within the cone
// if it is not kept, and one within the cone widened by that margin if it is.
#[wasm_bindgen_test(unsupported = test)]
fn a_spot_keeps_every_face_its_cone_reaches_and_no_other() {
    let grids = face_grids();
    // The axes, a cube corner and an edge, and a Fibonacci sphere.
    let mut directions = vec![
        DVec3::X,
        DVec3::NEG_X,
        DVec3::Y,
        DVec3::NEG_Y,
        DVec3::Z,
        DVec3::NEG_Z,
        DVec3::ONE.normalize(),
        DVec3::new(-1., 1., 0.).normalize(),
    ];
    let golden = std::f64::consts::PI * (3. - 5f64.sqrt());
    let count = 48;
    directions.extend((0..count).map(|i| {
        let y = 1. - 2. * (i as f64 + 0.5) / count as f64;
        let ring = (1. - y * y).sqrt();
        let turn = golden * i as f64;
        DVec3::new(ring * turn.cos(), y, ring * turn.sin())
    }));
    for direction in directions {
        let (across, along) = direction.any_orthonormal_pair();
        for degrees in (2..90).step_by(3) {
            let outer = f64::from(degrees).to_radians();
            let kept = cone_faces(direction.as_vec3(), outer as f32);
            // Directions within the cone, a hair inside its edge, spread
            // evenly over its cap.
            let samples = 300;
            let cos_edge = (outer * (1. - 1e-4)).cos();
            for i in 0..samples {
                let cos = 1. - (1. - cos_edge) * (i as f64 + 0.5) / samples as f64;
                let sin = (1. - cos * cos).sqrt();
                let turn = golden * i as f64;
                let sample = direction * cos + (across * turn.cos() + along * turn.sin()) * sin;
                let faces = holding(sample);
                assert_eq!(
                    faces & !kept,
                    0,
                    "{direction} at {degrees}°: {sample} falls in faces {faces:06b}, kept {kept:06b}"
                );
            }
            for (face, grid) in grids.iter().enumerate() {
                let nearest = grid
                    .iter()
                    .map(|v| direction.dot(*v))
                    .fold(-1., f64::max)
                    .clamp(-1., 1.)
                    .acos();
                if kept & (1 << face) == 0 {
                    assert!(
                        nearest > outer,
                        "{direction} at {degrees}°: face {face} dropped, {:.3}° away",
                        nearest.to_degrees()
                    );
                } else {
                    assert!(
                        nearest <= outer + GRID_MARGIN,
                        "{direction} at {degrees}°: face {face} kept, {:.3}° away",
                        nearest.to_degrees()
                    );
                }
            }
        }
    }
}
