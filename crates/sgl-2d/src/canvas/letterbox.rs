//! Aspect-preserving letterbox fit (PR-1): the largest centered placement of
//! the logical image inside the window. Fractional scaling allowed (matches
//! Godot `canvas_items` stretch + keep aspect); bars fill the rest.

/// A pixel rectangle inside the window, for `RenderPass::set_viewport`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Letterbox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Fit a `logical_w × logical_h` image inside a `window_w × window_h` window:
/// `scale = min(win_w/log_w, win_h/log_h)`, fractional (not integer-snapped),
/// centered. Zero-area windows are treated as 1×1 (never divides by zero).
pub fn fit_fractional(window_w: u32, window_h: u32, logical_w: u32, logical_h: u32) -> Letterbox {
    let win_w = window_w.max(1) as f32;
    let win_h = window_h.max(1) as f32;
    let log_w = logical_w.max(1) as f32;
    let log_h = logical_h.max(1) as f32;
    let scale = (win_w / log_w).min(win_h / log_h);
    let width = log_w * scale;
    let height = log_h * scale;
    Letterbox {
        x: (win_w - width) * 0.5,
        y: (win_h - height) * 0.5,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const W: u32 = 960;
    const H: u32 = 540;

    fn assert_close(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "expected {b}, got {a}");
    }

    /// Exact-size window: the image fills it, no bars.
    #[wasm_bindgen_test(unsupported = test)]
    fn exact_fit_has_no_bars() {
        let lb = fit_fractional(W, H, W, H);
        assert_close(lb.x, 0.0);
        assert_close(lb.y, 0.0);
        assert_close(lb.width, 960.0);
        assert_close(lb.height, 540.0);
    }

    /// Integer upscale: 1920×1080 is an exact 2× fill.
    #[wasm_bindgen_test(unsupported = test)]
    fn double_size_fills_exactly() {
        let lb = fit_fractional(1920, 1080, W, H);
        assert_close(lb.x, 0.0);
        assert_close(lb.y, 0.0);
        assert_close(lb.width, 1920.0);
        assert_close(lb.height, 1080.0);
    }

    /// A wider-than-16:9 window pillarboxes: full height, centered x bars.
    #[wasm_bindgen_test(unsupported = test)]
    fn wide_window_pillarboxes() {
        let lb = fit_fractional(2000, 540, W, H);
        assert_close(lb.height, 540.0);
        assert_close(lb.width, 960.0);
        assert_close(lb.x, (2000.0 - 960.0) * 0.5);
        assert_close(lb.y, 0.0);
    }

    /// A taller-than-16:9 window letterboxes: full width, centered y bars.
    #[wasm_bindgen_test(unsupported = test)]
    fn tall_window_letterboxes() {
        let lb = fit_fractional(960, 1000, W, H);
        assert_close(lb.width, 960.0);
        assert_close(lb.height, 540.0);
        assert_close(lb.x, 0.0);
        assert_close(lb.y, (1000.0 - 540.0) * 0.5);
    }

    /// Fractional scaling is allowed (PR-1): a non-integer ratio still fills
    /// the constraining axis exactly.
    #[wasm_bindgen_test(unsupported = test)]
    fn fractional_scale_fills_constraining_axis() {
        let lb = fit_fractional(1000, 1000, W, H);
        assert_close(lb.width, 1000.0);
        assert_close(lb.height, 1000.0 * 540.0 / 960.0);
        assert_close(lb.x, 0.0);
    }

    /// A zero-area (minimized) window must not divide by zero.
    #[wasm_bindgen_test(unsupported = test)]
    fn zero_size_window_is_safe() {
        let lb = fit_fractional(0, 0, W, H);
        assert!(lb.width > 0.0 && lb.height > 0.0);
    }
}
