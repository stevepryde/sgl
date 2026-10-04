//! Press-activated controls: buttons, icon and toggle buttons, radios,
//! checkboxes and collapsing headers.

use super::{UiFrame, UiKey, widget_id};
use crate::canvas::draw::Rect;
use crate::canvas::text::{HAlign, TextStyle, VAlign};

/// Compact text-glyph button states. The hit rectangle is independent of
/// glyph size. Disabled controls remain focusable so their tooltip explains why.
#[derive(Debug, Clone, Copy, Default)]
pub struct IconButton {
    pub selected: bool,
    pub disabled: bool,
}

impl UiFrame<'_> {
    /// A push button. Returns `true` on **press-down** over the button
    /// (PR-9 semantics — the action fires the frame the mouse goes down).
    pub fn button(&mut self, name: &str, rect: Rect, label: &str, px: f32) -> bool {
        let (fired, held, hover) = self.button_behavior(name, &rect);
        let bg = if held {
            self.ui.theme.pressed
        } else if hover {
            self.ui.theme.hovered
        } else {
            self.ui.theme.control
        };
        self.rect(rect, bg);
        let fg = if held {
            self.ui.theme.text_pressed
        } else if hover {
            self.ui.theme.text_hovered
        } else {
            self.ui.theme.text
        };
        let style = TextStyle::new(px, fg).with_outline(self.ui.theme.text_outline);
        self.label(rect, label, &style, HAlign::Center, VAlign::Center);
        fired
    }

    /// Draw a compact glyph button using the shared button palette. `name`
    /// is its stable focus identity; attach an explanatory [`Self::tooltip_for`]
    /// with the same name. Selected controls have an inset geometric marker.
    /// Disabled controls never act, including Enter/Space, but retain focus.
    pub fn icon_button(
        &mut self,
        name: &str,
        rect: Rect,
        glyph: &str,
        px: f32,
        options: IconButton,
    ) -> bool {
        let (fired, held, hover) = self.button_behavior(name, &rect);
        let bg = if options.disabled {
            self.ui.theme.disabled
        } else if held || options.selected {
            self.ui.theme.pressed
        } else if hover {
            self.ui.theme.hovered
        } else {
            self.ui.theme.control
        };
        self.rect(rect, bg);
        let fg = if options.disabled {
            self.ui.theme.disabled_text
        } else {
            self.ui.theme.text
        };
        self.label(
            rect,
            glyph,
            &TextStyle::new(px, fg),
            HAlign::Center,
            VAlign::Center,
        );
        if options.selected {
            self.border(
                Rect::new(
                    rect.min.x + 3.0,
                    rect.min.y + 3.0,
                    (rect.size().x - 6.0).max(0.0),
                    (rect.size().y - 6.0).max(0.0),
                ),
                1.0,
                fg,
            );
        }
        if self.ui.focus == Some(widget_id(name)) {
            self.border(rect, 1.0, self.ui.theme.accent);
        }
        fired && !options.disabled
    }

    /// A toggle button (Godot `Button` with `toggle_mode`): draws the
    /// pressed style + accent border while `selected`. Returns `true` on
    /// press-down (the caller flips/sets its state).
    pub fn toggle_button(
        &mut self,
        name: &str,
        rect: Rect,
        label: &str,
        px: f32,
        selected: bool,
    ) -> bool {
        let (fired, held, hover) = self.button_behavior(name, &rect);
        if selected {
            self.border(rect, 2.0, self.ui.theme.accent);
        }
        let bg = if selected || held {
            self.ui.theme.pressed
        } else if hover {
            self.ui.theme.hovered
        } else {
            self.ui.theme.control
        };
        self.rect(rect, bg);
        let fg = if selected || held {
            self.ui.theme.text_pressed
        } else {
            self.ui.theme.text
        };
        let style = TextStyle::new(px, fg).with_outline(self.ui.theme.text_outline);
        self.label(rect, label, &style, HAlign::Center, VAlign::Center);
        fired
    }

    /// A radio member (PR-9 fire/water selection): a toggle button that
    /// stores `value` into `current` when pressed. Returns `true` when it
    /// fired this frame.
    pub fn radio<T: Copy + PartialEq>(
        &mut self,
        name: &str,
        rect: Rect,
        label: &str,
        px: f32,
        value: T,
        current: &mut T,
    ) -> bool {
        let fired = self.toggle_button(name, rect, label, px, *current == value);
        if fired {
            *current = value;
        }
        fired
    }

    /// Shared button interaction: press-on-down fire + held/hover visuals.
    pub(super) fn button_behavior(&mut self, name: &str, rect: &Rect) -> (bool, bool, bool) {
        let id = widget_id(name);
        let hover = self.hit(rect);
        let eligible = self.register_focus(id, rect);
        let keyboard = eligible
            && self.ui.focus == Some(id)
            && !self.input.mouse_pressed
            && !self.key_used
            && self
                .input
                .keys
                .iter()
                .any(|key| matches!(key, UiKey::Enter | UiKey::Space));
        let fired = (hover && self.input.mouse_pressed) || keyboard;
        if keyboard {
            self.key_used = true;
            self.ui.keyboard_captured = true;
        }
        if fired {
            self.ui.focus = Some(id);
            self.edit_clicked = true;
        }
        if eligible && self.ui.focus == Some(id) {
            self.border(*rect, 1.0, self.ui.theme.accent);
        }
        if fired {
            self.ui.active = Some(id);
        }
        let held = self.ui.active == Some(id) && self.input.mouse_down;
        (fired, held, hover)
    }

    /// A checkbox (Godot `CheckBox`): a square at the left edge of `rect`
    /// (its side is the rect's height) with `label` beside it. The whole
    /// `rect` is the hit area. Flips `checked` on **press-down** and returns
    /// `true` that frame.
    pub fn checkbox(
        &mut self,
        name: &str,
        rect: Rect,
        label: &str,
        px: f32,
        checked: &mut bool,
    ) -> bool {
        let (fired, held, hover) = self.button_behavior(name, &rect);
        if fired {
            *checked = !*checked;
        }
        let side = rect.size().y;
        let square = Rect::new(rect.min.x, rect.min.y, side, side);
        let bg = if held {
            self.ui.theme.pressed
        } else if hover {
            self.ui.theme.hovered
        } else {
            self.ui.theme.control
        };
        self.rect(square, bg);
        self.border(
            square,
            1.0,
            if *checked {
                self.ui.theme.accent
            } else {
                self.ui.theme.text
            },
        );
        if *checked {
            // The mark: an inset square in the accent color.
            let inset = (side * 0.25).max(2.0);
            self.rect(
                Rect::new(
                    square.min.x + inset,
                    square.min.y + inset,
                    (side - 2.0 * inset).max(1.0),
                    (side - 2.0 * inset).max(1.0),
                ),
                self.ui.theme.accent,
            );
        }
        let fg = if hover {
            self.ui.theme.text_hovered
        } else {
            self.ui.theme.text
        };
        let style = TextStyle::new(px, fg).with_outline(self.ui.theme.text_outline);
        let pad = 6.0;
        let text_rect = Rect::new(
            square.max.x + pad,
            rect.min.y,
            (rect.size().x - side - pad).max(0.0),
            side,
        );
        self.label(text_rect, label, &style, HAlign::Left, VAlign::Center);
        fired
    }

    /// A collapsing section header: a full-width button showing a `v` /
    /// `>` marker and `label`. Flips `open` on **press-down** and returns
    /// `true` that frame; the caller lays out the section body itself
    /// while `open` is `true` (this widget draws only the header row).
    pub fn collapsing_header(
        &mut self,
        name: &str,
        rect: Rect,
        label: &str,
        px: f32,
        open: &mut bool,
    ) -> bool {
        let (fired, held, hover) = self.button_behavior(name, &rect);
        if fired {
            *open = !*open;
        }
        let bg = if held {
            self.ui.theme.pressed
        } else if hover {
            self.ui.theme.hovered
        } else {
            self.ui.theme.control
        };
        self.rect(rect, bg);
        let fg = if held {
            self.ui.theme.text_pressed
        } else if hover {
            self.ui.theme.text_hovered
        } else {
            self.ui.theme.text
        };
        let style = TextStyle::new(px, fg).with_outline(self.ui.theme.text_outline);
        let pad = 8.0;
        let marker_w = rect.size().y;
        let marker = Rect::new(rect.min.x + pad, rect.min.y, marker_w, rect.size().y);
        self.label(
            marker,
            if *open { "v" } else { ">" },
            &style,
            HAlign::Left,
            VAlign::Center,
        );
        let text_rect = Rect::new(
            marker.max.x,
            rect.min.y,
            (rect.size().x - marker_w - 2.0 * pad).max(0.0),
            rect.size().y,
        );
        self.label(text_rect, label, &style, HAlign::Left, VAlign::Center);
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::draw::DrawList;
    use crate::ui::UiInput;
    use crate::ui::test_ui::{BTN, ROW, fixture, hover_at, press_at, release_at};
    use sgl_core::math::Vec2;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test(unsupported = test)]
    fn disabled_icon_keeps_focus_explanation_without_activation() {
        let (mut ui, mut text, _assets) = fixture();
        let rect = Rect::new(10.0, 10.0, 24.0, 24.0);
        for input in [
            UiInput::default(),
            UiInput {
                keys: vec![UiKey::Tab],
                ..UiInput::default()
            },
            UiInput {
                keys: vec![UiKey::Enter, UiKey::Space],
                ..UiInput::default()
            },
            UiInput {
                mouse_pos: Vec2::new(12.0, 12.0),
                mouse_pressed: true,
                mouse_down: true,
                ..UiInput::default()
            },
        ] {
            let mut list = DrawList::new();
            let mut f = ui.begin(&mut text, &mut list, input);
            assert!(!f.icon_button(
                "locked",
                rect,
                "X",
                12.0,
                IconButton {
                    disabled: true,
                    selected: true
                }
            ));
            let focused = f.is_focused("locked");
            let before = f.list.screen.len();
            f.tooltip_for(
                "locked",
                rect,
                Rect::new(0.0, 0.0, 100.0, 80.0),
                "Unavailable",
                12.0,
            );
            if focused {
                assert!(f.list.screen.len() > before);
            }
            f.end();
        }
        let mut list = DrawList::new();
        let mut f = ui.begin(&mut text, &mut list, UiInput::default());
        f.set_clip(Some(Rect::new(80.0, 0.0, 10.0, 10.0)));
        f.tooltip_for(
            "locked",
            rect,
            Rect::new(0.0, 0.0, 100.0, 80.0),
            "Unavailable",
            12.0,
        );
        assert!(
            f.list.screen.is_empty(),
            "clipped focus cannot show a tooltip"
        );
        f.set_clip(None);
        f.tooltip_for(
            "locked",
            rect,
            Rect::new(80.0, 0.0, 10.0, 10.0),
            "Unavailable",
            12.0,
        );
        assert!(
            f.list.screen.is_empty(),
            "offscreen focus cannot show a tooltip"
        );
        f.end();
    }

    /// PR-9: buttons fire on the press-down frame — not on release, not
    /// while held, not when pressed elsewhere.
    #[wasm_bindgen_test(unsupported = test)]
    fn button_fires_on_press_down_only() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();

        // Frame 1: press inside → fires immediately.
        let mut f = ui.begin(&mut text, &mut list, press_at(50.0, 30.0));
        assert!(f.button("b", BTN, "OK", 24.0));
        f.end();

        // Frame 2: still held → no re-fire.
        let mut f = ui.begin(&mut text, &mut list, hover_at(50.0, 30.0, true));
        assert!(!f.button("b", BTN, "OK", 24.0));
        f.end();

        // Frame 3: release → no fire.
        let input = UiInput {
            mouse_pos: Vec2::new(50.0, 30.0),
            mouse_released: true,
            ..UiInput::default()
        };
        let mut f = ui.begin(&mut text, &mut list, input);
        assert!(!f.button("b", BTN, "OK", 24.0));
        f.end();
        assert!(ui.active.is_none(), "release clears the active widget");

        // Frame 4: press outside → no fire.
        let mut f = ui.begin(&mut text, &mut list, press_at(500.0, 300.0));
        assert!(!f.button("b", BTN, "OK", 24.0));
        f.end();
    }

    /// Holding a press keeps the pressed visual state on that widget only.
    #[wasm_bindgen_test(unsupported = test)]
    fn active_widget_tracks_the_pressed_button() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();

        let mut f = ui.begin(&mut text, &mut list, press_at(50.0, 30.0));
        f.button("b", BTN, "OK", 24.0);
        f.end();
        assert_eq!(ui.active, Some(widget_id("b")));

        // Held next frame — stays active until release.
        let mut f = ui.begin(&mut text, &mut list, hover_at(50.0, 30.0, true));
        f.button("b", BTN, "OK", 24.0);
        f.end();
        assert_eq!(ui.active, Some(widget_id("b")));
    }

    /// Radio group: pressing a member selects it and deselects the rest
    /// (fire/water, PR-9).
    #[wasm_bindgen_test(unsupported = test)]
    fn radio_selects_on_press() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let water = Rect::new(120.0, 10.0, 80.0, 80.0);

        #[derive(Clone, Copy, PartialEq, Debug)]
        enum Element {
            Fire,
            Water,
        }
        let mut sel = Element::Fire;

        let mut f = ui.begin(&mut text, &mut list, press_at(150.0, 40.0));
        f.radio(
            "fire",
            Rect::new(10.0, 10.0, 80.0, 80.0),
            "F",
            24.0,
            Element::Fire,
            &mut sel,
        );
        f.radio("water", water, "W", 24.0, Element::Water, &mut sel);
        f.end();
        assert_eq!(sel, Element::Water);
    }

    /// A checkbox flips on the press-down frame anywhere in its rect — the
    /// label counts — and not while held or on release.
    #[wasm_bindgen_test(unsupported = test)]
    fn checkbox_flips_on_press_anywhere_in_its_rect() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let mut checked = false;
        // Press on the label, far from the square.
        let mut f = ui.begin(&mut text, &mut list, press_at(200.0, 116.0));
        assert!(f.checkbox("c", ROW, "Show grid", 20.0, &mut checked));
        f.end();
        assert!(checked);
        let mut f = ui.begin(&mut text, &mut list, hover_at(200.0, 116.0, true));
        assert!(!f.checkbox("c", ROW, "Show grid", 20.0, &mut checked));
        f.end();
        assert!(checked, "held → unchanged");
        let mut f = ui.begin(&mut text, &mut list, release_at(200.0, 116.0));
        assert!(!f.checkbox("c", ROW, "Show grid", 20.0, &mut checked));
        f.end();
        assert!(checked, "release → unchanged");
        let mut f = ui.begin(&mut text, &mut list, press_at(20.0, 116.0));
        assert!(f.checkbox("c", ROW, "Show grid", 20.0, &mut checked));
        f.end();
        assert!(!checked, "second press flips back");
    }

    /// A collapsing header flips `open` on press-down only.
    #[wasm_bindgen_test(unsupported = test)]
    fn collapsing_header_toggles_open_on_press() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let mut open = true;
        let mut f = ui.begin(&mut text, &mut list, press_at(100.0, 116.0));
        assert!(f.collapsing_header("h", ROW, "Transform", 20.0, &mut open));
        f.end();
        assert!(!open);
        let mut f = ui.begin(&mut text, &mut list, press_at(100.0, 200.0));
        assert!(!f.collapsing_header("h", ROW, "Transform", 20.0, &mut open));
        f.end();
        assert!(!open, "a press elsewhere leaves it");
    }
}
