//! Frames-per-second counter for an FPS label.

// The frame count converts to f32 for the rate.
#![allow(clippy::cast_precision_loss)]

/// The PR-1 FPS label value: accumulates frames for one second, then
/// reformats as `%.2f`. Draw the [`text`](Self::text) with a plain label.
#[derive(Debug)]
pub struct FpsCounter {
    accum: f32,
    frames: u32,
    text: String,
}

impl FpsCounter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            accum: 0.0,
            frames: 0,
            text: "0.00".to_owned(),
        }
    }

    /// Count one frame of `dt` seconds; refreshes the label text once per
    /// second (PR-1). Returns `true` on the frames it refreshed.
    pub fn tick(&mut self, dt: f32) -> bool {
        self.accum += dt;
        self.frames += 1;
        if self.accum >= 1.0 {
            let fps = self.frames as f32 / self.accum;
            self.text = format!("{fps:.2}");
            self.accum = 0.0;
            self.frames = 0;
            return true;
        }
        false
    }

    /// The current label text (`%.2f`).
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Default for FpsCounter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// FPS label: refreshes once per second with `%.2f` formatting (PR-1).
    #[wasm_bindgen_test(unsupported = test)]
    fn fps_counter_formats_once_per_second() {
        let mut fps = FpsCounter::new();
        assert_eq!(fps.text(), "0.00");
        assert!(!fps.tick(0.5));
        assert_eq!(fps.text(), "0.00", "no refresh before 1 s");
        assert!(fps.tick(0.6));
        // 2 frames / 1.1 s = 1.8181… → "1.82".
        assert_eq!(fps.text(), "1.82");
        // The accumulator resets.
        assert!(!fps.tick(0.9));
        assert_eq!(fps.text(), "1.82");
    }
}
