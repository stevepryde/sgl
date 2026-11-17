//! The scenario harness's analytic maths against values computed by hand:
//! coverage, projection and motion. Pure, so they run on native and wasm32.
mod scenario;

use scenario::geometry::{Motion, Pose, View, coverage};
use sgl_3d::glam::{Mat4, Vec2, Vec3};
use wasm_bindgen_test::wasm_bindgen_test;

/// A 90° view, 200 by 100: tan(45°) = 1, so one metre ahead spans 2 m
/// vertically over 100 pixels and 4 m horizontally over 200.
fn view() -> View {
    View {
        size: [200, 100],
        fov_y: std::f32::consts::FRAC_PI_2,
        near: 0.1,
    }
}

fn close(a: f32, b: f32, tolerance: f32) -> bool {
    (a - b).abs() <= tolerance
}

#[wasm_bindgen_test(unsupported = test)]
fn coverage_is_the_clipped_area() {
    // The left half of pixel [3, 2], from a strip x in 3..3.5.
    let strip = [3., 3.5].map(|x| [Vec2::new(x, 0.), Vec2::new(x, 10.)]);
    let strip = [strip[0][0], strip[1][0], strip[1][1], strip[0][1]];
    assert!(close(coverage(&strip, [3, 2], 0.), 0.5, 1e-6));
    assert_eq!(coverage(&strip, [5, 2], 0.), 0.);
    // The triangle below pixel [2, 2]'s diagonal is half of it.
    let triangle = [Vec2::new(2., 2.), Vec2::new(3., 2.), Vec2::new(2., 3.)];
    assert!(close(coverage(&triangle, [2, 2], 0.), 0.5, 1e-6));
    // Pixel [2, 2] itself fills a quarter of its square grown by half a
    // pixel, which is 2 by 2.
    let square = [(2., 2.), (3., 2.), (3., 3.), (2., 3.)].map(|(x, y)| Vec2::new(x, y));
    assert!(close(coverage(&square, [2, 2], 0.5), 0.25, 1e-6));
    // A polygon enclosing the pixel covers it wholly, either winding.
    let mut large = [(0., 0.), (9., 0.), (9., 9.), (0., 9.)].map(|(x, y)| Vec2::new(x, y));
    assert_eq!(coverage(&large, [4, 4], 0.), 1.);
    large.reverse();
    assert_eq!(coverage(&large, [4, 4], 0.), 1.);
}

#[wasm_bindgen_test(unsupported = test)]
fn projection_places_points_by_hand() {
    let view = view();
    let camera = view.camera(Pose::at(Vec3::ZERO));
    let (centre, depth) = view.project(&camera, Vec3::new(0., 0., -1.)).unwrap();
    assert!(close(centre.x, 100., 1e-4) && close(centre.y, 50., 1e-4) && close(depth, 1., 1e-6));
    // x = 1 at 2 m: NDC 1 / (2 × aspect 2) = 0.25, pixel 0.625 × 200.
    let (right, _) = view.project(&camera, Vec3::new(1., 0., -2.)).unwrap();
    assert!(close(right.x, 125., 1e-4));
    // y = 0.5 at 1 m: NDC 0.5, pixel 0.25 × 100 (y down).
    let (up, _) = view.project(&camera, Vec3::new(0., 0.5, -1.)).unwrap();
    assert!(close(up.y, 25., 1e-4));
    assert!(view.project(&camera, Vec3::new(0., 0., 1.)).is_none());
    // 2 m ahead, 100 pixels span 4 m.
    assert!(close(view.pixel_metres(2.), 0.04, 1e-7));
    // The ray through a projected point leads back to it.
    let ray = view.ray(&camera, right);
    assert!(ray.cross(Vec3::new(1., 0., -2.).normalize()).length() < 1e-6);
}

#[wasm_bindgen_test(unsupported = test)]
fn motion_follows_the_script_by_hand() {
    let view = view();
    let start = Pose::at(Vec3::new(1., 2., 3.));
    let motion = Motion::translate(Vec3::new(0.5, 0., 0.)).jumping_at(6, Vec3::Z);
    assert_eq!(motion.pose(start, 2).position, Vec3::new(2., 2., 3.));
    assert_eq!(motion.pose(start, 5).position, Vec3::new(3.5, 2., 3.));
    assert_eq!(motion.pose(start, 6).position, Vec3::new(4., 2., 4.));
    // A card facing the camera 2 m ahead; corners in its own frame.
    let card = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)].map(|(x, y)| Vec3::new(x, y, 0.));
    let placed = Mat4::from_translation(Vec3::new(0., 0., -2.));
    // A camera that panned one pixel at 2 m (0.04 m) to the right: the
    // card's point at the centre was one pixel right of it before.
    let now = view.camera(Pose::at(Vec3::new(0.04, 0., 0.)));
    let before = view.camera(Pose::at(Vec3::ZERO));
    let centre = Vec2::new(100., 50.);
    let pan = view
        .surface_motion(&now, &before, card, placed, placed, centre)
        .unwrap();
    assert!(close(pan.x, -1. / 200., 1e-6) && close(pan.y, 0., 1e-6) && close(pan.z, 0., 1e-6));
    // A camera that dollied 0.5 m forward: the point at x = 1 (pixel
    // 125 now) lay 2.5 m ahead, at NDC 1 / 5, pixel 120; its depth fell
    // by 0.5 m.
    let now = view.camera(Pose::at(Vec3::ZERO));
    let before = view.camera(Pose::at(Vec3::new(0., 0., 0.5)));
    let dolly = view
        .surface_motion(&now, &before, card, placed, placed, Vec2::new(125., 50.))
        .unwrap();
    assert!(close(dolly.x, 5. / 200., 1e-5) && close(dolly.z, 0.5, 1e-5));
    // The same card carried 0.5 m toward a still camera moves alike.
    let farther = Mat4::from_translation(Vec3::new(0., 0., -2.5));
    let approach = view
        .surface_motion(&now, &now, card, placed, farther, Vec2::new(125., 50.))
        .unwrap();
    assert!((approach - dolly).length() < 1e-5);
    // A camera that turned by atan(0.04) to its left: the direction now
    // ahead lay at NDC -0.04 / aspect 2 before, pixel 98, so the sky
    // moved 2 pixels right.
    let turned = view.camera(Pose {
        position: Vec3::ZERO,
        yaw: 0.04f32.atan(),
    });
    let sky = view.sky_motion(&turned, &now, centre);
    assert!(close(sky.x, 2. / 200., 1e-6) && close(sky.y, 0., 1e-6));
}
