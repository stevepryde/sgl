//! Orthographic 2D camera over the logical pixel space (AR-6, PR-1/PR-5).
//!
//! The default convention matches the Godot original: **y-down** world in
//! logical pixels (960×540 view at `zoom = 1`), positive rotation clockwise.
//! The camera is a world-space center + zoom + rotation; UI/HUD uses the fixed
//! [`screen_view_proj`](Camera::screen_view_proj) (an identity camera over
//! the logical space).
//!
//! A camera may instead carry a **world-unit convention**
//! ([`WorldUnits`], via [`Camera::with_units`]): `pixels_per_unit` pixels
//! per unit and optionally **y-up**. Then `center`, every world-channel
//! sprite/light/occluder position and [`screen_to_world`](Camera::screen_to_world)
//! are in world units, and the renderer applies the pixel seam once (the
//! game no longer converts). The logical view size and the screen channel
//! stay in y-down pixels, `zoom` still magnifies, and clip rects stay in
//! logical view pixels. The pixel convention is `WorldUnits::default()` and
//! that path is bit-identical to a camera without units.

use glam::{Mat4, Vec2};

use crate::canvas::letterbox::Letterbox;

/// The world-channel coordinate convention a [`Camera`] carries.
///
/// The default (`pixels_per_unit = 1`, y-down) is the logical-pixel
/// convention. A game simulating in y-up world units at, say, 32 px/unit
/// sets `{ pixels_per_unit: 32.0, y_up: true }` and pushes world-unit
/// positions and sizes straight into the `DrawList`/`LightFrame`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldUnits {
    /// Logical pixels per world unit at `zoom = 1`. Must be finite and > 0.
    pub pixels_per_unit: f32,
    /// Whether world +y points **up** the screen (the pixel convention is
    /// y-down).
    pub y_up: bool,
}

impl Default for WorldUnits {
    /// The logical-pixel convention: one pixel per unit, y-down.
    fn default() -> Self {
        Self {
            pixels_per_unit: 1.0,
            y_up: false,
        }
    }
}

impl WorldUnits {
    /// The sign relating world y to the screen's y-down axis: `1.0` under
    /// the pixel convention, `-1.0` when `y_up`. Multiplying by it is exact,
    /// so the default path stays bit-identical.
    pub(crate) fn screen_y_sign(self) -> f32 {
        if self.y_up { -1.0 } else { 1.0 }
    }
}

/// A world-space camera: `center` is the world point at the middle of the
/// view; `zoom > 1` magnifies (shows fewer world pixels/units); `rotation`
/// turns the view in radians.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// World point at the view center, in the camera's units (logical
    /// pixels by default).
    pub center: Vec2,
    /// Magnification; `1.0` maps one world pixel (`pixels_per_unit` pixels
    /// per world unit) to one logical pixel.
    pub zoom: f32,
    /// Rotation in radians, matching
    /// [`SpriteInstance::rot`](crate::canvas::draw::SpriteInstance::rot):
    /// positive is clockwise on screen under the default y-down pixel
    /// convention, and counter-clockwise in world space when
    /// [`WorldUnits::y_up`] is `true`.
    pub rotation: f32,
    /// The logical view size in pixels (PR-1: 960×540).
    logical: Vec2,
    /// The world-channel convention (pixels, y-down by default).
    units: WorldUnits,
}

impl Camera {
    /// A camera over a `logical_w × logical_h` view, centered on it
    /// (so world coords == screen coords until moved), `zoom = 1`.
    pub fn new(logical_w: u32, logical_h: u32) -> Self {
        Self::with_view(Vec2::new(logical_w as f32, logical_h as f32))
    }

    /// [`Camera::new`] over a fractional view size (the editor's native-res
    /// view is `physical / ui_scale` and need not be integral).
    pub fn with_view(logical: Vec2) -> Self {
        Self {
            center: logical * 0.5,
            zoom: 1.0,
            rotation: 0.0,
            logical,
            units: WorldUnits::default(),
        }
    }

    /// Carry a world-unit convention: `center` and every world-channel
    /// position are then in `units` (see [`WorldUnits`]). `center` is left
    /// as is — set it in the new units. **Panics** when
    /// `pixels_per_unit` is not finite and positive (an authoring error).
    #[must_use]
    pub fn with_units(mut self, units: WorldUnits) -> Self {
        assert!(
            units.pixels_per_unit.is_finite() && units.pixels_per_unit > 0.0,
            "WorldUnits::pixels_per_unit must be finite and > 0, got {}",
            units.pixels_per_unit
        );
        self.units = units;
        self
    }

    /// The world-channel convention this camera carries.
    pub fn units(&self) -> WorldUnits {
        self.units
    }

    /// Change the view size this camera maps onto (the editor scene sizes
    /// its camera to the window; game scenes keep the fixed logical view).
    /// Center/zoom are left untouched.
    pub fn set_view_size(&mut self, logical: Vec2) {
        self.logical = logical;
    }

    /// The view size in logical pixels.
    pub fn view_size(&self) -> Vec2 {
        self.logical
    }

    /// View-projection matrix mapping world coordinates → wgpu clip space.
    /// World `center` lands at NDC origin; the view spans
    /// `logical / zoom / pixels_per_unit` world units and is rotated by
    /// `rotation`. Built by hand: x maps left→-1, right→+1; y **flips** under
    /// the pixel convention (screen top = smaller local y → clip +1) and
    /// maps straight when `y_up`; z is the constant 0.5 mid-depth (no
    /// depth buffer — painter's order comes from the z-sorted `DrawList`).
    /// When `rotation == 0.0`, the unrotated arithmetic is used verbatim so
    /// the default path stays bit-identical.
    pub fn view_proj(&self) -> Mat4 {
        let half = self.logical * 0.5 / self.zoom / self.units.pixels_per_unit;
        let sx = 1.0 / half.x;
        // y-down world → y-up clip (`-1 / half`); y-up world → `+1 / half`.
        let sy = -self.units.screen_y_sign() / half.y;
        if self.rotation == 0.0 {
            return Mat4::from_cols(
                glam::Vec4::new(sx, 0.0, 0.0, 0.0),
                glam::Vec4::new(0.0, sy, 0.0, 0.0),
                glam::Vec4::new(0.0, 0.0, 1.0, 0.0),
                glam::Vec4::new(-self.center.x * sx, -self.center.y * sy, 0.5, 1.0),
            );
        }
        let (s, c) = self.rotation.sin_cos();
        let cx = self.center.x;
        let cy = self.center.y;
        Mat4::from_cols(
            glam::Vec4::new(sx * c, -sy * s, 0.0, 0.0),
            glam::Vec4::new(sx * s, sy * c, 0.0, 0.0),
            glam::Vec4::new(0.0, 0.0, 1.0, 0.0),
            glam::Vec4::new(-sx * (c * cx + s * cy), -sy * (-s * cx + c * cy), 0.5, 1.0),
        )
    }

    /// The fixed screen-space projection over a logical view: identity
    /// camera (top-left `(0,0)`, bottom-right `(w,h)`) for UI/HUD sprites.
    pub fn screen_view_proj(logical_w: u32, logical_h: u32) -> Mat4 {
        Camera::new(logical_w, logical_h).view_proj()
    }

    /// [`Camera::screen_view_proj`] over a fractional view size (the
    /// editor's UI-point space).
    pub fn screen_view_proj_size(logical: Vec2) -> Mat4 {
        Camera::with_view(logical).view_proj()
    }

    /// Map a cursor in **physical window pixels** (winit: origin top-left,
    /// +Y down) to a world point in the camera's units, undoing the
    /// letterbox placement (`lb`, from
    /// [`fit_fractional`](crate::canvas::letterbox::fit_fractional)) then
    /// the camera. A cursor on a letterbox bar simply yields a world point
    /// outside the view (linear, unclamped).
    pub fn screen_to_world(&self, cursor_px: Vec2, lb: &Letterbox) -> Vec2 {
        // Window px → logical px (undo bar offset + fit scale).
        let logical = Vec2::new(
            (cursor_px.x - lb.x) / (lb.width / self.logical.x),
            (cursor_px.y - lb.y) / (lb.height / self.logical.y),
        );
        // Logical px → camera-local world: recenter on the view middle,
        // un-zoom, apply the units (y flips only when `y_up`).
        let offset = (logical - self.logical * 0.5) / self.zoom;
        let local =
            Vec2::new(offset.x, offset.y * self.units.screen_y_sign()) / self.units.pixels_per_unit;
        if self.rotation == 0.0 {
            return self.center + local;
        }
        let (s, c) = self.rotation.sin_cos();
        self.center + Vec2::new(c * local.x - s * local.y, s * local.x + c * local.y)
    }

    /// Map a world point (the camera's units) to **logical view pixels**
    /// (y-down, origin top-left — the screen channel's space), the forward
    /// map overlays use to draw over the scene. The inverse of
    /// [`screen_to_world`](Self::screen_to_world) up to the letterbox.
    pub fn world_to_screen(&self, world: Vec2) -> Vec2 {
        let local = if self.rotation == 0.0 {
            world - self.center
        } else {
            let d = world - self.center;
            let (s, c) = self.rotation.sin_cos();
            Vec2::new(c * d.x + s * d.y, -s * d.x + c * d.y)
        };
        let offset = local * (self.zoom * self.units.pixels_per_unit);
        Vec2::new(offset.x, offset.y * self.units.screen_y_sign()) + self.logical * 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec4;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #255: the forward map alone, at zoom and pixels-per-unit other than
    /// one, against hand-computed pixels (a roundtrip cannot tell a
    /// multiply from a divide when both maps flip together).
    #[wasm_bindgen_test(unsupported = test)]
    fn world_to_screen_and_screen_to_world_use_hand_computed_scales() {
        let mut camera = Camera::new(200, 100).with_units(WorldUnits {
            pixels_per_unit: 8.0,
            y_up: true,
        });
        camera.center = Vec2::new(10.0, 5.0);
        camera.zoom = 2.0;
        // (12, 6) is 2 units right and 1 unit up of the center: 2 × 8 × 2 =
        // 32 px right, 16 px up (screen y-down) of the view center (100, 50).
        assert_eq!(
            camera.world_to_screen(Vec2::new(12.0, 6.0)),
            Vec2::new(132.0, 34.0)
        );
        assert_eq!(camera.units().pixels_per_unit, 8.0);
        assert!(camera.units().y_up);
        let lb = Letterbox {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 100.0,
        };
        assert_eq!(
            camera.screen_to_world(Vec2::new(132.0, 34.0), &lb),
            Vec2::new(12.0, 6.0)
        );
        camera.set_view_size(Vec2::new(400.0, 200.0));
        assert_eq!(camera.view_size(), Vec2::new(400.0, 200.0));
        assert_eq!(
            camera.world_to_screen(Vec2::new(12.0, 6.0)),
            Vec2::new(232.0, 84.0)
        );
    }

    use crate::canvas::letterbox::fit_fractional;

    const W: u32 = 960;
    const H: u32 = 540;

    fn assert_close(a: Vec2, b: Vec2) {
        assert!((a - b).length() < 1e-2, "expected {b:?}, got {a:?}");
    }

    /// Forward map world → window px through `view_proj` + the letterbox
    /// (the inverse of `screen_to_world`) — used to round-trip-test it.
    fn world_to_window(world: Vec2, cam: &Camera, lb: &Letterbox) -> Vec2 {
        let clip = cam.view_proj() * Vec4::new(world.x, world.y, 0.0, 1.0);
        // NDC x ∈ [-1,1] left→right, y ∈ [-1,1] bottom→top of the screen.
        let lx = (clip.x * 0.5 + 0.5) * W as f32;
        let ly = (0.5 - clip.y * 0.5) * H as f32;
        Vec2::new(
            lb.x + lx * (lb.width / W as f32),
            lb.y + ly * (lb.height / H as f32),
        )
    }

    /// The default camera is the identity mapping: world == logical pixels,
    /// top-left (0,0) → clip (-1, +1).
    #[wasm_bindgen_test(unsupported = test)]
    fn default_camera_maps_logical_pixels_identically() {
        let cam = Camera::new(W, H);
        let top_left = cam.view_proj() * Vec4::new(0.0, 0.0, 0.0, 1.0);
        assert!((top_left.x + 1.0).abs() < 1e-6);
        assert!((top_left.y - 1.0).abs() < 1e-6);
        let bottom_right = cam.view_proj() * Vec4::new(960.0, 540.0, 0.0, 1.0);
        assert!((bottom_right.x - 1.0).abs() < 1e-6);
        assert!((bottom_right.y + 1.0).abs() < 1e-6);
    }

    /// `screen_view_proj` equals the untouched default camera's matrix.
    #[wasm_bindgen_test(unsupported = test)]
    fn screen_projection_is_the_identity_camera() {
        assert_eq!(
            Camera::screen_view_proj(W, H),
            Camera::new(W, H).view_proj()
        );
    }

    /// The window center maps to the camera center, letterbox or not.
    #[wasm_bindgen_test(unsupported = test)]
    fn screen_to_world_center_is_camera_center() {
        let mut cam = Camera::new(W, H);
        cam.center = Vec2::new(123.0, -45.0);
        for (ww, wh) in [(960, 540), (1920, 1080), (2000, 540), (960, 1000)] {
            let lb = fit_fractional(ww, wh, W, H);
            let center_px = Vec2::new(ww as f32 * 0.5, wh as f32 * 0.5);
            assert_close(cam.screen_to_world(center_px, &lb), cam.center);
        }
    }

    /// Round trip through a letterboxed window, moved camera, and zoom.
    #[wasm_bindgen_test(unsupported = test)]
    fn screen_to_world_roundtrips_with_letterbox_and_zoom() {
        let mut cam = Camera::new(W, H);
        cam.center = Vec2::new(300.0, 800.0);
        cam.zoom = 2.0;
        let lb = fit_fractional(1600, 1600, W, H);
        assert!(lb.y > 0.0, "expected horizontal bars");
        for world in [
            Vec2::new(300.0, 800.0),
            Vec2::new(120.0, 700.0),
            Vec2::new(500.0, 900.0),
        ] {
            let px = world_to_window(world, &cam, &lb);
            assert_close(cam.screen_to_world(px, &lb), world);
        }
    }

    /// Zoom 2 shows half the world span: the window's right edge is only a
    /// quarter-view to the right of center.
    #[wasm_bindgen_test(unsupported = test)]
    fn zoom_narrows_the_visible_span() {
        let mut cam = Camera::new(W, H);
        cam.zoom = 2.0;
        let lb = fit_fractional(W, H, W, H);
        let right_edge = cam.screen_to_world(Vec2::new(960.0, 270.0), &lb);
        assert_close(right_edge, Vec2::new(480.0 + 240.0, 270.0));
    }

    /// Under the pixel convention `world_to_screen` is the identity on the
    /// untouched camera and follows center/zoom: with the camera at
    /// (300, 800) at zoom 2, world (310, 790) is 20 px right of and 20 px
    /// above the view center (480, 270).
    #[wasm_bindgen_test(unsupported = test)]
    fn world_to_screen_follows_center_and_zoom_in_pixels() {
        let cam = Camera::new(W, H);
        assert_close(
            cam.world_to_screen(Vec2::new(100.0, 40.0)),
            Vec2::new(100.0, 40.0),
        );
        let mut cam = Camera::new(W, H);
        cam.center = Vec2::new(300.0, 800.0);
        cam.zoom = 2.0;
        assert_close(
            cam.world_to_screen(Vec2::new(310.0, 790.0)),
            Vec2::new(500.0, 250.0),
        );
    }

    /// The units convention used by the y-up, 32 px/unit game (shadow-sp).
    fn units32() -> WorldUnits {
        WorldUnits {
            pixels_per_unit: 32.0,
            y_up: true,
        }
    }

    /// At 32 px/unit, y-up, from a camera at the origin, world (1, 1) is
    /// 32 px right of and 32 px ABOVE the view center: clip x = 32/480,
    /// clip y = +32/270 (clip +y is the screen top).
    #[wasm_bindgen_test(unsupported = test)]
    fn units_view_proj_maps_one_unit_to_ppu_pixels_y_up() {
        let mut cam = Camera::new(W, H).with_units(units32());
        cam.center = Vec2::ZERO;
        let clip = cam.view_proj() * Vec4::new(1.0, 1.0, 0.0, 1.0);
        assert!((clip.x - 32.0 / 480.0).abs() < 1e-6, "{clip:?}");
        assert!((clip.y - 32.0 / 270.0).abs() < 1e-6, "{clip:?}");
    }

    /// The bare-window corners at 32 px/unit, y-up, camera at the origin:
    /// the view is 30 × 16.875 units, so the top-left pixel is world
    /// (-15, +8.4375) and the bottom-right is (15, -8.4375).
    #[wasm_bindgen_test(unsupported = test)]
    fn units_screen_to_world_corners_y_up() {
        let mut cam = Camera::new(W, H).with_units(units32());
        cam.center = Vec2::ZERO;
        let lb = fit_fractional(W, H, W, H);
        assert_close(
            cam.screen_to_world(Vec2::ZERO, &lb),
            Vec2::new(-15.0, 8.4375),
        );
        assert_close(
            cam.screen_to_world(Vec2::new(960.0, 540.0), &lb),
            Vec2::new(15.0, -8.4375),
        );
    }

    /// In units the forward map lands where the convention says (camera
    /// (3, -2), zoom 2: one unit right is 64 px right, one unit up is 64 px
    /// up the screen), and it round-trips with `screen_to_world` through a
    /// letterboxed window.
    #[wasm_bindgen_test(unsupported = test)]
    fn units_world_to_screen_and_back_roundtrip_with_letterbox() {
        let mut cam = Camera::new(W, H).with_units(units32());
        cam.center = Vec2::new(3.0, -2.0);
        cam.zoom = 2.0;
        assert_close(
            cam.world_to_screen(Vec2::new(3.0, -2.0)),
            Vec2::new(480.0, 270.0),
        );
        assert_close(
            cam.world_to_screen(Vec2::new(4.0, -2.0)),
            Vec2::new(544.0, 270.0),
        );
        assert_close(
            cam.world_to_screen(Vec2::new(3.0, -1.0)),
            Vec2::new(480.0, 206.0),
        );

        let lb = fit_fractional(1600, 1600, W, H);
        assert!(lb.y > 0.0, "expected horizontal bars");
        for world in [
            Vec2::new(3.0, -2.0),
            Vec2::new(-4.5, 1.25),
            Vec2::new(7.0, -6.0),
        ] {
            let logical = cam.world_to_screen(world);
            let px = Vec2::new(
                lb.x + logical.x * (lb.width / W as f32),
                lb.y + logical.y * (lb.height / H as f32),
            );
            assert_close(cam.screen_to_world(px, &lb), world);
            // And the GPU matrix agrees with the CPU forward map.
            assert_close(world_to_window(world, &cam, &lb), px);
        }
    }

    #[wasm_bindgen_test(unsupported = test)]
    #[should_panic(expected = "pixels_per_unit")]
    fn with_units_rejects_zero_ppu() {
        let _ = Camera::new(W, H).with_units(WorldUnits {
            pixels_per_unit: 0.0,
            y_up: false,
        });
    }

    #[wasm_bindgen_test(unsupported = test)]
    #[should_panic(expected = "pixels_per_unit")]
    fn with_units_rejects_infinite_ppu() {
        let _ = Camera::new(W, H).with_units(WorldUnits {
            pixels_per_unit: f32::INFINITY,
            y_up: true,
        });
    }

    /// `rotation == 0.0` takes the exact unrotated path (bit-identical to the
    /// pre-rotation formulas, including `screen_view_proj`).
    #[wasm_bindgen_test(unsupported = test)]
    fn rotation_zero_is_bit_identical() {
        for units in [WorldUnits::default(), units32()] {
            let mut cam = Camera::new(W, H).with_units(units);
            cam.center = Vec2::new(123.25, -45.75);
            cam.zoom = 1.5;
            assert_eq!(cam.rotation, 0.0);

            let half = cam.view_size() * 0.5 / cam.zoom / units.pixels_per_unit;
            let sx = 1.0 / half.x;
            let sy = -units.screen_y_sign() / half.y;
            let expected = Mat4::from_cols(
                Vec4::new(sx, 0.0, 0.0, 0.0),
                Vec4::new(0.0, sy, 0.0, 0.0),
                Vec4::new(0.0, 0.0, 1.0, 0.0),
                Vec4::new(-cam.center.x * sx, -cam.center.y * sy, 0.5, 1.0),
            );
            assert_eq!(
                cam.view_proj().to_cols_array().map(f32::to_bits),
                expected.to_cols_array().map(f32::to_bits),
            );

            let world = Vec2::new(131.5, -39.25);
            let offset = (world - cam.center) * (cam.zoom * units.pixels_per_unit);
            let expected_screen =
                Vec2::new(offset.x, offset.y * units.screen_y_sign()) + cam.view_size() * 0.5;
            let got_screen = cam.world_to_screen(world);
            assert_eq!(got_screen.x.to_bits(), expected_screen.x.to_bits());
            assert_eq!(got_screen.y.to_bits(), expected_screen.y.to_bits());

            let lb = fit_fractional(1600, 1600, W, H);
            let cursor = Vec2::new(412.5, 687.25);
            let logical = Vec2::new(
                (cursor.x - lb.x) / (lb.width / cam.view_size().x),
                (cursor.y - lb.y) / (lb.height / cam.view_size().y),
            );
            let unzoomed = (logical - cam.view_size() * 0.5) / cam.zoom;
            let expected_world = cam.center
                + Vec2::new(unzoomed.x, unzoomed.y * units.screen_y_sign()) / units.pixels_per_unit;
            let got_world = cam.screen_to_world(cursor, &lb);
            assert_eq!(got_world.x.to_bits(), expected_world.x.to_bits());
            assert_eq!(got_world.y.to_bits(), expected_world.y.to_bits());
        }
    }

    /// Rotated `view_proj` matches hand-computed clip coordinates across
    /// angles and both y-down / y-up conventions, and a sprite rotated by
    /// `cam.rotation` appears upright on screen.
    #[wasm_bindgen_test(unsupported = test)]
    fn rotated_view_proj_matches_hand_computed_clip_coords() {
        use crate::canvas::sprite::sprite_affine;
        use std::f32::consts::{FRAC_PI_2, PI};

        for units in [WorldUnits::default(), units32()] {
            for angle in [-FRAC_PI_2, FRAC_PI_2, 0.6, PI] {
                let mut cam = Camera::new(W, H).with_units(units);
                cam.center = Vec2::new(10.0, -6.0);
                cam.zoom = 2.0;
                cam.rotation = angle;

                let vp = cam.view_proj();
                let half = cam.view_size() * 0.5 / cam.zoom / units.pixels_per_unit;
                let sx = 1.0 / half.x;
                let sy = -units.screen_y_sign() / half.y;
                let (s, c) = angle.sin_cos();

                // Center always lands at NDC origin.
                let center_clip = vp * Vec4::new(cam.center.x, cam.center.y, 0.0, 1.0);
                assert!(center_clip.x.abs() < 1e-5, "{center_clip:?}");
                assert!(center_clip.y.abs() < 1e-5, "{center_clip:?}");
                assert!((center_clip.z - 0.5).abs() < 1e-6);
                assert!((center_clip.w - 1.0).abs() < 1e-6);

                for delta in [
                    Vec2::new(0.0, 10.0),
                    Vec2::new(10.0, 0.0),
                    Vec2::new(-3.5, 4.25),
                ] {
                    let world = cam.center + delta;
                    let clip = vp * Vec4::new(world.x, world.y, 0.0, 1.0);
                    let expected_x = sx * (c * delta.x + s * delta.y);
                    let expected_y = sy * (-s * delta.x + c * delta.y);
                    assert!(
                        (clip.x - expected_x).abs() < 1e-4 && (clip.y - expected_y).abs() < 1e-4,
                        "units={units:?} angle={angle} delta={delta:?}: got {clip:?}, expected ({expected_x}, {expected_y})"
                    );
                }

                // Sign check at +π/2: center + (0, 10) appears to the RIGHT of
                // screen center in both y-down and y-up; center + (10, 0) is UP
                // under y-down and DOWN under y-up.
                if (angle - FRAC_PI_2).abs() < 1e-6 {
                    let p_y = vp * Vec4::new(cam.center.x, cam.center.y + 10.0, 0.0, 1.0);
                    assert!(p_y.x > 0.0 && p_y.y.abs() < 1e-5, "{p_y:?}");
                    let p_x = vp * Vec4::new(cam.center.x + 10.0, cam.center.y, 0.0, 1.0);
                    assert!(p_x.x.abs() < 1e-5, "{p_x:?}");
                    if units.y_up {
                        assert!(p_x.y < 0.0, "expected down on screen for y-up: {p_x:?}");
                    } else {
                        assert!(p_x.y > 0.0, "expected up on screen for y-down: {p_x:?}");
                    }
                }

                // A sprite rotated by `cam.rotation` appears upright on screen:
                // texture-right points along +clip.x, texture-down along -clip.y.
                let a = sprite_affine(cam.center, angle, Vec2::new(32.0, 16.0), units);
                let c0 = vp * Vec4::new(a.translation.x, a.translation.y, 0.0, 1.0);
                let cr = vp
                    * Vec4::new(
                        a.translation.x + a.matrix2.x_axis.x,
                        a.translation.y + a.matrix2.x_axis.y,
                        0.0,
                        1.0,
                    );
                let cd = vp
                    * Vec4::new(
                        a.translation.x + a.matrix2.y_axis.x,
                        a.translation.y + a.matrix2.y_axis.y,
                        0.0,
                        1.0,
                    );
                let dr = cr - c0;
                let dd = cd - c0;
                assert!(dr.x > 0.0 && dr.y.abs() < 1e-5, "right axis {dr:?}");
                assert!(dd.x.abs() < 1e-5 && dd.y < 0.0, "down axis {dd:?}");
            }
        }
    }

    /// `world_to_screen`, `screen_to_world`, and `view_proj` agree and round-trip
    /// through a letterboxed window across rotation angles and world-unit modes.
    #[wasm_bindgen_test(unsupported = test)]
    fn rotated_world_to_screen_and_screen_to_world_roundtrip() {
        use std::f32::consts::{FRAC_PI_2, PI};

        let lb = fit_fractional(1600, 1600, W, H);
        assert!(lb.y > 0.0, "expected horizontal bars");

        for units in [WorldUnits::default(), units32()] {
            for angle in [-FRAC_PI_2, FRAC_PI_2, 0.6, PI] {
                let mut cam = Camera::new(W, H).with_units(units);
                cam.center = Vec2::new(12.5, -7.25);
                cam.zoom = 1.75;
                cam.rotation = angle;

                for world in [
                    cam.center,
                    cam.center + Vec2::new(5.0, 0.0),
                    cam.center + Vec2::new(0.0, 5.0),
                    cam.center + Vec2::new(-4.5, 3.25),
                    cam.center + Vec2::new(7.0, -6.0),
                ] {
                    let logical = cam.world_to_screen(world);
                    let px = Vec2::new(
                        lb.x + logical.x * (lb.width / W as f32),
                        lb.y + logical.y * (lb.height / H as f32),
                    );
                    assert_close(world_to_window(world, &cam, &lb), px);
                    assert_close(cam.screen_to_world(px, &lb), world);
                }
            }
        }
    }
}
