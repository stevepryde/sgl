//! The numeric field: step buttons, drag-to-scrub and click-to-type.

use super::text_edit::EditState;
use super::{UiFrame, widget_id};
use crate::canvas::draw::Rect;
use crate::canvas::text::{HAlign, TextStyle, VAlign};

/// A numeric field's press-and-drag: which field, where the press landed,
/// the value it started from, and whether the pointer has travelled far
/// enough to count as a scrub rather than a click.
#[derive(Debug, Clone, Copy)]
pub(super) struct NumberDrag {
    id: u64,
    start_x: f32,
    start_value: f32,
    moved: bool,
}

/// Pointer travel (px) that turns a numeric field's press into a value scrub;
/// a release inside it is a click, which opens the field for typing.
const NUMBER_DRAG_THRESHOLD: f32 = 3.0;

/// Longest text a numeric field accepts while typed into.
const NUMBER_EDIT_MAX_LEN: usize = 24;

/// Options for a [`UiFrame::number_field`]: how fast a drag scrubs, what the
/// `-`/`+` buttons add, the value's bounds, and how it is displayed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumberField {
    /// Value change per pixel of horizontal drag.
    pub speed: f32,
    /// Increment of the `-`/`+` buttons.
    pub step: f32,
    /// Lowest value the field will store.
    pub min: f32,
    /// Highest value the field will store.
    pub max: f32,
    /// Decimal places shown while not being typed into.
    pub decimals: usize,
}

impl Default for NumberField {
    /// Unbounded, one unit per pixel and per button press, two decimals.
    fn default() -> Self {
        Self {
            speed: 1.0,
            step: 1.0,
            min: f32::NEG_INFINITY,
            max: f32::INFINITY,
            decimals: 2,
        }
    }
}

impl NumberField {
    /// Value change per pixel of horizontal drag.
    #[must_use]
    pub fn speed(mut self, speed: f32) -> Self {
        self.speed = speed;
        self
    }

    /// Increment of the `-`/`+` buttons.
    #[must_use]
    pub fn step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    /// Inclusive bounds every stored value is clamped into.
    #[must_use]
    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.min = min;
        self.max = max;
        self
    }

    /// Decimal places shown while not being typed into.
    #[must_use]
    pub fn decimals(mut self, decimals: usize) -> Self {
        self.decimals = decimals;
        self
    }

    /// `value` clamped into the field's bounds. A NaN bound is ignored on
    /// that side rather than poisoning the value.
    fn clamp(&self, value: f32) -> f32 {
        let value = if self.min.is_nan() {
            value
        } else {
            value.max(self.min)
        };
        if self.max.is_nan() {
            value
        } else {
            value.min(self.max)
        }
    }

    /// The text shown for `value` while the field is not being typed into.
    fn display(&self, value: f32) -> String {
        format!("{value:.*}", self.decimals)
    }
}

impl UiFrame<'_> {
    /// A numeric field (egui's `DragValue`): `-` and `+` squares at the
    /// ends of `rect` (side = the rect's height) add `opts.step`; the middle
    /// shows `value` with `opts.decimals` places and is **dragged**
    /// horizontally to scrub it by `opts.speed` per pixel, or **clicked**
    /// (a press released within a few pixels of where it landed) to type a
    /// value: the middle then behaves as a focused line edit whose text is
    /// re-parsed whenever it changes — a parse that succeeds stores the
    /// number, one that fails (`-`, empty, `1e`) leaves `value` alone (and
    /// so does a `-`/`+` press while typing, which ends the typing). Every stored
    /// value is clamped into `opts.min..=opts.max`. Clicking elsewhere ends
    /// typing (the ordinary click-away unfocus). Returns whether `value`
    /// changed this frame.
    pub fn number_field(
        &mut self,
        name: &str,
        rect: Rect,
        value: &mut f32,
        opts: NumberField,
        px: f32,
    ) -> bool {
        let id = widget_id(name);
        let side = rect.size().y;
        let dec = Rect::new(rect.min.x, rect.min.y, side, side);
        let inc = Rect::new(rect.max.x - side, rect.min.y, side, side);
        let middle = Rect::new(
            rect.min.x + side,
            rect.min.y,
            (rect.size().x - 2.0 * side).max(0.0),
            side,
        );
        let before = *value;

        if self.button(&format!("{name}.dec"), dec, "-", px) {
            *value = opts.clamp(*value - opts.step);
        }
        if self.button(&format!("{name}.inc"), inc, "+", px) {
            *value = opts.clamp(*value + opts.step);
        }

        self.register_focus(id, &middle);
        if self.ui.focus == Some(id) {
            if self.ui.edit.id != Some(id) {
                self.ui.number_edit = opts.display(*value);
            }
            // Typing: the middle is a line edit over the retained buffer.
            let mut buf = std::mem::take(&mut self.ui.number_edit);
            let edited =
                self.line_edit_impl(name, middle, &mut buf, NUMBER_EDIT_MAX_LEN, px, false);
            if edited
                && let Ok(typed) = buf.trim().parse::<f32>()
                && typed.is_finite()
            {
                *value = opts.clamp(typed);
            }
            self.ui.number_edit = buf;
            return *value != before;
        }

        // Scrubbing: a press on the middle starts a drag; the value follows
        // the pointer once it has travelled the threshold, and a release
        // before that is a click that opens the field for typing.
        let hover = self.hit(&middle);
        if self.input.mouse_pressed && hover {
            self.ui.active = Some(id);
            self.ui.number_drag = Some(NumberDrag {
                id,
                start_x: self.input.mouse_pos.x,
                start_value: *value,
                moved: false,
            });
        }
        let mut dragging = false;
        if let Some(mut drag) = self.ui.number_drag
            && drag.id == id
        {
            if self.input.mouse_down {
                let dx = self.input.mouse_pos.x - drag.start_x;
                if dx.abs() >= NUMBER_DRAG_THRESHOLD {
                    drag.moved = true;
                }
                if drag.moved {
                    *value = opts.clamp(drag.start_value + dx * opts.speed);
                }
                self.ui.number_drag = Some(drag);
                dragging = drag.moved;
            } else {
                self.ui.number_drag = None;
                if !drag.moved {
                    self.ui.focus = Some(id);
                    self.ui.number_edit = opts.display(*value);
                    self.ui.edit = EditState {
                        id: Some(id),
                        caret: self.ui.number_edit.chars().count(),
                        anchor: self.ui.number_edit.chars().count(),
                        scroll: 0.0,
                    };
                }
            }
        }

        let bg = if dragging {
            self.ui.theme.pressed
        } else if hover {
            self.ui.theme.hovered
        } else {
            self.ui.theme.control
        };
        self.rect(middle, bg);
        let fg = if hover {
            self.ui.theme.text_hovered
        } else {
            self.ui.theme.text
        };
        let style = TextStyle::new(px, fg).with_outline(self.ui.theme.text_outline);
        self.label(
            middle,
            &opts.display(*value),
            &style,
            HAlign::Center,
            VAlign::Center,
        );
        *value != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::draw::DrawList;
    use crate::ui::test_ui::{BTN, ROW, fixture, hover_at, press_at, release_at};
    use crate::ui::{UiInput, UiKey};
    use sgl_core::math::Vec2;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test(unsupported = test)]
    fn numeric_keyboard_reentry_uses_stepped_value() {
        let (mut ui, mut text, _assets) = fixture();
        let mut value = 10.0;
        for (index, input) in [
            UiInput::default(),
            UiInput {
                keys: vec![UiKey::Tab],
                ..UiInput::default()
            },
            UiInput {
                keys: vec![UiKey::Enter],
                ..UiInput::default()
            },
            UiInput {
                keys: vec![UiKey::Tab],
                shift: true,
                ..UiInput::default()
            },
            UiInput {
                chars: vec!['0'],
                ..UiInput::default()
            },
        ]
        .into_iter()
        .enumerate()
        {
            let mut list = DrawList::new();
            let mut f = ui.begin(&mut text, &mut list, input);
            if index == 0 {
                f.focus("n");
            }
            f.number_field(
                "n",
                BTN,
                &mut value,
                NumberField::default().decimals(0),
                20.0,
            );
            f.end();
            if index == 2 {
                assert_eq!(value, 9.0);
            }
        }
        assert_eq!(value, 90.0);
    }

    /// The numeric field: 32 px squares at each end. `-`/`+` add the step
    /// and clamp into the range.
    #[wasm_bindgen_test(unsupported = test)]
    fn number_field_buttons_step_and_clamp() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let opts = NumberField::default().step(5.0).range(0.0, 12.0);
        let mut value = 10.0;
        // `+` is the right-most square: x 218..250.
        let mut f = ui.begin(&mut text, &mut list, press_at(230.0, 116.0));
        assert!(f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        assert_eq!(value, 12.0, "10 + 5 clamps to the max");
        // `-` is the left-most square: x 10..42.
        let mut f = ui.begin(&mut text, &mut list, press_at(20.0, 116.0));
        assert!(f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        assert_eq!(value, 7.0);
        let mut f = ui.begin(&mut text, &mut list, hover_at(20.0, 116.0, true));
        assert!(
            !f.number_field("n", ROW, &mut value, opts, 20.0),
            "held: no repeat"
        );
        f.end();
        assert_eq!(value, 7.0);
    }

    /// Dragging the middle scrubs the value by `speed` per pixel once the
    /// pointer has moved past the threshold; the release ends the drag
    /// without opening the field for typing.
    #[wasm_bindgen_test(unsupported = test)]
    fn number_field_drag_scrubs_by_speed() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let opts = NumberField::default().speed(0.5).range(-100.0, 100.0);
        let mut value = 1.0;
        let mut f = ui.begin(&mut text, &mut list, press_at(100.0, 116.0));
        assert!(!f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        // 2 px: under the threshold, still a click.
        let mut f = ui.begin(&mut text, &mut list, hover_at(102.0, 116.0, true));
        assert!(!f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        assert_eq!(value, 1.0);
        // 20 px right: 1 + 20 × 0.5.
        let mut f = ui.begin(&mut text, &mut list, hover_at(120.0, 116.0, true));
        assert!(f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        assert_eq!(value, 11.0);
        // Back to 6 px right of the press: the value follows the total travel
        // from the press, not the last frame (no accumulation error).
        let mut f = ui.begin(&mut text, &mut list, hover_at(106.0, 116.0, true));
        assert!(f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        assert_eq!(value, 4.0);
        // Drag well past the range: clamped.
        let mut f = ui.begin(&mut text, &mut list, hover_at(900.0, 116.0, true));
        f.number_field("n", ROW, &mut value, opts, 20.0);
        f.end();
        assert_eq!(value, 100.0);
        let mut f = ui.begin(&mut text, &mut list, release_at(900.0, 116.0));
        assert!(!f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        assert!(!ui.has_focus(), "a drag never opens the field for typing");
        assert_eq!(value, 100.0);
    }

    /// A click on the middle (press + release without travel) opens the
    /// field for typing: typed text that parses replaces the value, text
    /// that does not leaves it, and a press elsewhere ends typing.
    #[wasm_bindgen_test(unsupported = test)]
    fn number_field_click_types_a_value() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let opts = NumberField::default().range(-10.0, 10.0);
        let mut value = 2.5;
        let mut f = ui.begin(&mut text, &mut list, press_at(100.0, 116.0));
        f.number_field("n", ROW, &mut value, opts, 20.0);
        f.end();
        let mut f = ui.begin(&mut text, &mut list, release_at(101.0, 116.0));
        assert!(!f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        assert!(ui.has_focus(), "a click opens the field for typing");
        assert_eq!(value, 2.5);

        // Backspace the "2.50" down to "2.5" (same value), "2." (Rust parses
        // a trailing dot: 2.0), "2", then "" (no parse → value kept).
        let type_in = |chars: &[char], backspace: bool| UiInput {
            mouse_pos: Vec2::new(500.0, 500.0),
            chars: chars.to_vec(),
            backspace,
            ..UiInput::default()
        };
        let mut f = ui.begin(&mut text, &mut list, type_in(&[], true));
        assert!(!f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        assert_eq!(value, 2.5, "\"2.5\" parses to the same value");
        let mut f = ui.begin(&mut text, &mut list, type_in(&[], true));
        assert!(f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        assert_eq!(value, 2.0, "\"2.\" parses as 2");
        for _ in 0..2 {
            let mut f = ui.begin(&mut text, &mut list, type_in(&[], true));
            assert!(!f.number_field("n", ROW, &mut value, opts, 20.0));
            f.end();
            assert_eq!(value, 2.0, "\"2\" then \"\": value kept");
        }
        // "-" alone does not parse; "-7" does; "-79" clamps to the min.
        let mut f = ui.begin(&mut text, &mut list, type_in(&['-'], false));
        assert!(!f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        assert_eq!(value, 2.0);
        let mut f = ui.begin(&mut text, &mut list, type_in(&['7'], false));
        assert!(f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        assert_eq!(value, -7.0);
        let mut f = ui.begin(&mut text, &mut list, type_in(&['9'], false));
        assert!(f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        assert_eq!(value, -10.0, "typed -79 clamps to the min");

        // A press elsewhere ends typing; the field shows the value again.
        let mut f = ui.begin(&mut text, &mut list, press_at(500.0, 500.0));
        assert!(!f.number_field("n", ROW, &mut value, opts, 20.0));
        f.end();
        assert!(!ui.has_focus());
        assert_eq!(value, -10.0);
    }
}
