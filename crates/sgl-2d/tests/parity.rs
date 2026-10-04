//! Frozen camera fixtures executed natively and in a wasm runtime: the pixel
//! seam a browser client and a native client must apply identically
//! (client.md 3). Floats are compared by bit pattern.

use sgl_2d::canvas::{Camera, Letterbox, WorldUnits};
use sgl_core::math::Vec2;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test(unsupported = test)]
fn world_unit_camera_seam_is_target_independent() {
    let mut camera = Camera::new(960, 540).with_units(WorldUnits {
        pixels_per_unit: 16.0,
        y_up: true,
    });
    camera.center = Vec2::new(12.5, -3.75);
    camera.zoom = 1.5;
    let screen = camera.world_to_screen(Vec2::new(3.5, -2.25));
    assert_eq!(
        [screen.x.to_bits(), screen.y.to_bits()],
        [1_132_724_224, 1_131_020_288]
    );
    let letterbox = Letterbox {
        x: 40.0,
        y: 22.5,
        width: 1200.0,
        height: 675.0,
    };
    let world = camera.screen_to_world(Vec2::new(333.0, 444.0), &letterbox);
    assert_eq!(
        [world.x.to_bits(), world.y.to_bits()],
        [1_074_860_304, 3_234_961_818]
    );
}
