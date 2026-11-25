use std::ops::{Add, Mul};
use std::time::Duration;

/// A presentation offset that hides a reconcile's jump and fades to zero.
///
/// When a reconcile moves the predicted body, the game
/// [`add`](Self::add)s the difference between where it was drawn and where
/// it now is, draws the body at its state plus [`offset`](Self::offset), and
/// [`advance`](Self::advance)s the correction by each frame's elapsed time.
/// The offset fades linearly to exactly zero over the `fade` time the game
/// chose, so it never overshoots; a new correction during a fade adds to
/// what is left and starts the fade again. A zero `fade` snaps.
///
/// It holds only the offset and never touches the simulated state. `V` is
/// whatever the game offsets: `f32`, a glam vector, or its own type that
/// scales by `f32` and whose `Default` is zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Correction<V> {
    /// The offset when the fade began.
    start: V,
    fade: Duration,
    remaining: Duration,
}

impl<V> Correction<V>
where
    V: Copy + Default + Add<Output = V> + Mul<f32, Output = V>,
{
    /// No offset; each correction fades over `fade`.
    #[must_use]
    pub fn new(fade: Duration) -> Self {
        Self {
            start: V::default(),
            fade,
            remaining: Duration::ZERO,
        }
    }

    /// The time a correction takes to fade.
    #[must_use]
    pub fn fade(&self) -> Duration {
        self.fade
    }

    /// Add `offset` to what is left of the current one and fade the sum
    /// from now.
    pub fn add(&mut self, offset: V) {
        self.start = self.offset() + offset;
        self.remaining = self.fade;
    }

    /// Let `elapsed` of the fade pass.
    pub fn advance(&mut self, elapsed: Duration) {
        self.remaining = self.remaining.saturating_sub(elapsed);
    }

    /// The offset to add to the drawn state now.
    #[must_use]
    pub fn offset(&self) -> V {
        if self.remaining.is_zero() {
            return V::default();
        }
        // `remaining` is at most a nonzero `fade`; the rounded ratio only
        // feeds presentation.
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let left = (self.remaining.as_nanos() as f64 / self.fade.as_nanos() as f64) as f32;
        self.start * left
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    /// Oracle, by hand: 8 fading linearly over 100 ms is 6 after 25 ms and
    /// 4 after 50; a further 2 then gives 6, fading afresh, so 3 after
    /// another 50 ms; it is exactly zero once the fade has passed and stays
    /// there, never crossing to the other side. Every value is exact in
    /// `f32`.
    #[allow(clippy::float_cmp)]
    #[wasm_bindgen_test(unsupported = test)]
    fn a_correction_fades_to_zero_without_overshooting() {
        let mut correction = Correction::<f32>::new(ms(100));
        assert_eq!(correction.offset().to_bits(), 0.0_f32.to_bits());
        correction.add(8.0);
        assert_eq!(correction.offset(), 8.0);
        correction.advance(ms(25));
        assert_eq!(correction.offset(), 6.0);
        correction.advance(ms(25));
        assert_eq!(correction.offset(), 4.0);
        correction.add(2.0);
        assert_eq!(correction.offset(), 6.0);
        correction.advance(ms(50));
        assert_eq!(correction.offset(), 3.0);
        correction.advance(ms(50));
        assert_eq!(correction.offset().to_bits(), 0.0_f32.to_bits());
        correction.advance(ms(1000));
        assert_eq!(correction.offset().to_bits(), 0.0_f32.to_bits());
    }

    /// A zero fade time draws no offset at all.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_zero_fade_snaps() {
        let mut correction = Correction::<f32>::new(Duration::ZERO);
        correction.add(8.0);
        assert_eq!(correction.offset().to_bits(), 0.0_f32.to_bits());
    }
}
