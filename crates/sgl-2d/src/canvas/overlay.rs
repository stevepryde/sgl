//! Flat-quad overlay primitives: lines, rectangle outlines, filled rects and
//! circles, for debug draws, editor gizmos and screen fades.
//!
//! SGL has no line pipeline — every primitive here is one or more quads on the
//! shared 1×1 white texture ([`crate::assets::white_texture`], uploaded by
//! `Renderer::white_texture`), stretched and rotated into shape. That is what
//! the source games' `debug_draw` and `editor/overlays` each rebuilt.
//!
//! An [`Overlay`] carries the settings a run of primitives shares (the white
//! handle, color, `z` and clip rect) and each method appends instances to a
//! plain `Vec<SpriteInstance>` — so the caller picks the channel by passing
//! `&mut list.world` or `&mut list.screen`. Overlays usually want the screen
//! channel: it draws after the lighting composite, so the gizmos stay unlit.
//!
//! A full-canvas fade is just a fill over the logical view:
//! `Overlay { color: [0.0, 0.0, 0.0, a], z, ..Overlay::new(white) }
//! .fill_rect(&mut list.screen, Rect::new(0.0, 0.0, width, height))`.

use crate::assets::{Handle, Texture};
use crate::canvas::draw::{Rect, SpriteInstance};
use sgl_core::math::Vec2;

/// The settings a run of overlay primitives shares.
///
/// Adjust with struct-update syntax, like [`SpriteInstance`]:
/// `Overlay { color: RED, z: 900.0, ..Overlay::new(white) }`.
#[derive(Debug, Clone, Copy)]
pub struct Overlay {
    /// The shared 1×1 white texture every primitive scales up.
    pub white: Handle<Texture>,
    /// RGBA modulate (straight alpha, sRGB) applied to every emitted quad.
    pub color: [f32; 4],
    /// Draw depth of every emitted quad.
    pub z: f32,
    /// Clip rect in logical view pixels, or `None` for unclipped.
    pub clip: Option<Rect>,
}

impl Overlay {
    /// Opaque white, `z = 0`, unclipped.
    pub fn new(white: Handle<Texture>) -> Self {
        Self {
            white,
            color: [1.0, 1.0, 1.0, 1.0],
            z: 0.0,
            clip: None,
        }
    }

    /// Fill an axis-aligned rectangle. Empty or negative rects emit nothing.
    pub fn fill_rect(&self, out: &mut Vec<SpriteInstance>, rect: Rect) {
        let size = rect.size();
        // A negated conjunction so NaN extents are rejected as well.
        if !(size.x > 0.0 && size.y > 0.0) {
            return;
        }
        out.push(SpriteInstance {
            scale: size,
            color: self.color,
            z: self.z,
            clip: self.clip,
            ..SpriteInstance::new(self.white, rect.min + size * 0.5)
        });
    }

    /// A `width`-pixel segment from `a` to `b`.
    ///
    /// Axis-aligned segments become **unrotated** quads whose leading edge is
    /// snapped to the pixel grid (`(a.y - width / 2).round()`): a rotated 1 px
    /// quad lands its edges on pixel centers, where the rasterizer's edge
    /// rules can drop the whole row. Other segments are quads of length
    /// `|b - a|` rotated to match. `width` floors at 1 px; a zero-length or
    /// non-finite segment emits nothing.
    pub fn line(&self, out: &mut Vec<SpriteInstance>, a: Vec2, b: Vec2, width: f32) {
        let d = b - a;
        let len = d.length();
        if len <= 0.0 || !len.is_finite() {
            return;
        }
        let width = width.max(1.0);
        if d.y == 0.0 {
            let y = (a.y - width * 0.5).round();
            self.fill_rect(out, Rect::new(a.x.min(b.x), y, len, width));
            return;
        }
        if d.x == 0.0 {
            let x = (a.x - width * 0.5).round();
            self.fill_rect(out, Rect::new(x, a.y.min(b.y), width, len));
            return;
        }
        out.push(SpriteInstance {
            scale: Vec2::new(len, width),
            rot: d.to_angle(),
            color: self.color,
            z: self.z,
            clip: self.clip,
            ..SpriteInstance::new(self.white, (a + b) * 0.5)
        });
    }

    /// Outline a rectangle with four `width`-pixel strips drawn **inside** it,
    /// so the outline never spills past `rect` (it can be clipped to the same
    /// rect) and its thickness does not depend on the rect's own size.
    ///
    /// The strips do not overlap — the top and bottom run the full width, the
    /// left and right fill only the band between them — so a corner pixel is
    /// covered exactly once and a translucent outline stays even. `width`
    /// floors at 1 px and clamps to half the rect on each axis. Strip rects
    /// are exact (no pixel snapping); pass an integer-aligned `rect` for a
    /// crisp edge.
    pub fn rect_outline(&self, out: &mut Vec<SpriteInstance>, rect: Rect, width: f32) {
        let size = rect.size();
        if !(size.x > 0.0 && size.y > 0.0) {
            return;
        }
        let width = width.max(1.0);
        // `2 * wx <= size.x` holds exactly, so the middle band never inverts.
        let wx = width.min(size.x * 0.5);
        let wy = width.min(size.y * 0.5);
        let band = size.y - 2.0 * wy;
        self.fill_rect(out, Rect::new(rect.min.x, rect.min.y, size.x, wy));
        self.fill_rect(out, Rect::new(rect.min.x, rect.max.y - wy, size.x, wy));
        self.fill_rect(out, Rect::new(rect.min.x, rect.min.y + wy, wx, band));
        self.fill_rect(out, Rect::new(rect.max.x - wx, rect.min.y + wy, wx, band));
    }

    /// A circle outline as a regular `segments`-gon of `width`-pixel lines,
    /// starting at `center + (radius, 0)`. `segments` floors at 3; a
    /// non-positive `radius` collapses every side to zero length and emits
    /// nothing.
    pub fn circle(
        &self,
        out: &mut Vec<SpriteInstance>,
        center: Vec2,
        radius: f32,
        segments: u32,
        width: f32,
    ) {
        let n = segments.max(3);
        let step = std::f32::consts::TAU / n as f32;
        let mut prev = center + Vec2::new(radius, 0.0);
        for i in 1..=n {
            let angle = i as f32 * step;
            let next = center + Vec2::new(angle.cos(), angle.sin()) * radius;
            self.line(out, prev, next, width);
            prev = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{Assets, white_texture};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #255: every primitive carries the overlay's `z` and `clip`, and the
    /// zero-size guards are strict.
    #[wasm_bindgen_test(unsupported = test)]
    fn primitives_carry_z_and_clip_and_guards_are_strict() {
        let clip = Rect::new(1.0, 2.0, 30.0, 40.0);
        let o = Overlay {
            clip: Some(clip),
            ..overlay()
        };
        let mut out = Vec::new();
        o.fill_rect(&mut out, Rect::new(0.0, 0.0, 3.0, 3.0));
        o.line(&mut out, Vec2::ZERO, Vec2::new(5.0, 5.0), 1.0);
        o.line(&mut out, Vec2::ZERO, Vec2::new(5.0, 0.0), 1.0);
        o.rect_outline(&mut out, Rect::new(0.0, 0.0, 8.0, 8.0), 1.0);
        o.circle(&mut out, Vec2::ZERO, 4.0, 3, 1.0);
        assert!(
            out.iter().all(|q| q.z == 900.0 && q.clip == Some(clip)),
            "{out:?}"
        );

        let mut out = Vec::new();
        o.rect_outline(&mut out, Rect::new(0.0, 0.0, 0.0, 8.0), 1.0);
        o.rect_outline(&mut out, Rect::new(0.0, 0.0, 8.0, 0.0), 1.0);
        o.fill_rect(&mut out, Rect::new(0.0, 0.0, 8.0, 0.0));
        o.line(&mut out, Vec2::ZERO, Vec2::new(f32::NAN, 1.0), 1.0);
        o.line(&mut out, Vec2::ONE, Vec2::ONE, 1.0);
        assert!(out.is_empty(), "{out:?}");
    }

    fn overlay() -> Overlay {
        let mut assets: Assets<Texture> = Assets::new();
        Overlay {
            color: [1.0, 0.0, 0.0, 0.5],
            z: 900.0,
            ..Overlay::new(white_texture(&mut assets))
        }
    }

    /// A fill is one quad centered in the rect, scaled to its pixel size
    /// (the white texture's natural size is 1×1), carrying the run's style.
    #[wasm_bindgen_test(unsupported = test)]
    fn fill_rect_is_one_styled_quad() {
        let o = overlay();
        let mut out = Vec::new();
        o.fill_rect(&mut out, Rect::new(4.0, 10.0, 20.0, 6.0));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pos, Vec2::new(14.0, 13.0));
        assert_eq!(out[0].scale, Vec2::new(20.0, 6.0));
        assert_eq!(out[0].rot, 0.0);
        assert!(out[0].src.is_none(), "the whole 1×1 white pixel");
        assert_eq!(out[0].color, [1.0, 0.0, 0.0, 0.5]);
        assert_eq!(out[0].z, 900.0);

        // Empty and negative rects emit nothing.
        o.fill_rect(&mut out, Rect::new(0.0, 0.0, 0.0, 6.0));
        o.fill_rect(&mut out, Rect::new(0.0, 0.0, 6.0, -1.0));
        assert_eq!(out.len(), 1);
    }

    /// An axis-aligned line snaps its band to the pixel grid: a 1 px
    /// horizontal line at y = 20.3 covers pixel row 20 exactly (center 20.5)
    /// instead of straddling two rows and rasterizing as none.
    #[wasm_bindgen_test(unsupported = test)]
    fn axis_aligned_lines_snap_to_whole_pixel_rows() {
        let o = overlay();
        let mut out = Vec::new();
        o.line(&mut out, Vec2::new(10.0, 20.3), Vec2::new(30.0, 20.3), 1.0);
        assert_eq!(out[0].pos, Vec2::new(20.0, 20.5));
        assert_eq!(out[0].scale, Vec2::new(20.0, 1.0));
        assert_eq!(out[0].rot, 0.0, "axis-aligned lines are never rotated");

        // Vertical, drawn right-to-left/bottom-to-top: x snaps to 5, the
        // band spans y 4..12.
        out.clear();
        o.line(&mut out, Vec2::new(5.7, 12.0), Vec2::new(5.7, 4.0), 1.0);
        assert_eq!(out[0].pos, Vec2::new(5.5, 8.0));
        assert_eq!(out[0].scale, Vec2::new(1.0, 8.0));

        // A 3 px band starts at round(10 - 1.5) = 9, so it covers rows 9..12.
        out.clear();
        o.line(&mut out, Vec2::new(0.0, 10.0), Vec2::new(8.0, 10.0), 3.0);
        assert_eq!(out[0].pos, Vec2::new(4.0, 10.5));
        assert_eq!(out[0].scale, Vec2::new(8.0, 3.0));
    }

    /// A diagonal is a rotated quad as long as the segment; sub-pixel widths
    /// floor at 1 px; a zero-length segment emits nothing.
    #[wasm_bindgen_test(unsupported = test)]
    fn diagonal_lines_rotate_and_widths_floor_at_one() {
        let o = overlay();
        let mut out = Vec::new();
        // 3-4-5 triangle: length 5, angle atan2(4, 3).
        o.line(&mut out, Vec2::ZERO, Vec2::new(3.0, 4.0), 0.2);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pos, Vec2::new(1.5, 2.0));
        assert_eq!(out[0].scale, Vec2::new(5.0, 1.0));
        assert!((out[0].rot - 4.0_f32.atan2(3.0)).abs() < 1e-6);

        o.line(&mut out, Vec2::new(7.0, 7.0), Vec2::new(7.0, 7.0), 1.0);
        assert_eq!(out.len(), 1, "a zero-length segment emits nothing");
    }

    /// A 10×6 outline at width 1 is four exact inside strips that partition
    /// the border ring: full-width top and bottom, and left/right filling
    /// only the 4 px band between them, so corners are covered once.
    #[wasm_bindgen_test(unsupported = test)]
    fn rect_outline_strips_are_inside_and_do_not_overlap() {
        let o = overlay();
        let mut out = Vec::new();
        o.rect_outline(&mut out, Rect::new(0.0, 0.0, 10.0, 6.0), 1.0);
        assert_eq!(out.len(), 4);
        let strips: Vec<(Vec2, Vec2)> = out.iter().map(|q| (q.pos, q.scale)).collect();
        assert_eq!(
            strips,
            vec![
                (Vec2::new(5.0, 0.5), Vec2::new(10.0, 1.0)), // top
                (Vec2::new(5.0, 5.5), Vec2::new(10.0, 1.0)), // bottom
                (Vec2::new(0.5, 3.0), Vec2::new(1.0, 4.0)),  // left
                (Vec2::new(9.5, 3.0), Vec2::new(1.0, 4.0)),  // right
            ]
        );
        // Ring area = 10*6 - 8*4 = 28, matched exactly by the four strips.
        let covered: f32 = out.iter().map(|q| q.scale.x * q.scale.y).sum();
        assert_eq!(covered, 28.0);
    }

    /// A width past half the rect clamps instead of inverting the middle
    /// band: a 4×4 rect at width 10 fills solid with two strips.
    #[wasm_bindgen_test(unsupported = test)]
    fn rect_outline_clamps_a_width_past_half_the_rect() {
        let o = overlay();
        let mut out = Vec::new();
        o.rect_outline(&mut out, Rect::new(0.0, 0.0, 4.0, 4.0), 10.0);
        assert_eq!(out.len(), 2, "the middle band is empty, so no side strips");
        assert_eq!(out[0].pos, Vec2::new(2.0, 1.0));
        assert_eq!(out[0].scale, Vec2::new(4.0, 2.0));
        assert_eq!(out[1].pos, Vec2::new(2.0, 3.0));
        assert_eq!(out[1].scale, Vec2::new(4.0, 2.0));
    }

    /// A 4-gon of radius 10 is four diagonals between the axis points, each
    /// √200 long, centered on the quadrant midpoints.
    #[wasm_bindgen_test(unsupported = test)]
    fn circle_is_an_n_gon_of_rotated_sides() {
        let o = overlay();
        let mut out = Vec::new();
        o.circle(&mut out, Vec2::ZERO, 10.0, 4, 1.0);
        assert_eq!(out.len(), 4);
        let side = 200.0_f32.sqrt();
        let expected = [
            (Vec2::new(5.0, 5.0), 135.0_f32),
            (Vec2::new(-5.0, 5.0), -135.0),
            (Vec2::new(-5.0, -5.0), -45.0),
            (Vec2::new(5.0, -5.0), 45.0),
        ];
        for (q, (pos, deg)) in out.iter().zip(expected) {
            assert!((q.pos - pos).length() < 1e-3, "{:?} vs {pos:?}", q.pos);
            assert!((q.scale.x - side).abs() < 1e-3, "{:?}", q.scale);
            assert_eq!(q.scale.y, 1.0);
            assert!(
                (q.rot - deg.to_radians()).abs() < 1e-5,
                "{} vs {deg}",
                q.rot
            );
        }

        // `segments` floors at 3; a collapsed radius emits nothing.
        out.clear();
        o.circle(&mut out, Vec2::ZERO, 10.0, 0, 1.0);
        assert_eq!(out.len(), 3);
        out.clear();
        o.circle(&mut out, Vec2::new(2.0, 2.0), 0.0, 8, 1.0);
        assert!(out.is_empty());
    }

    /// Both channels share one implementation: the caller picks by passing
    /// the channel's vector.
    #[wasm_bindgen_test(unsupported = test)]
    fn either_draw_list_channel_takes_the_primitives() {
        let o = overlay();
        let mut list = crate::canvas::draw::DrawList::new();
        o.fill_rect(&mut list.screen, Rect::new(0.0, 0.0, 4.0, 4.0));
        o.rect_outline(&mut list.world, Rect::new(0.0, 0.0, 10.0, 6.0), 1.0);
        assert_eq!(list.screen.len(), 1);
        assert_eq!(list.world.len(), 4);
    }
}
