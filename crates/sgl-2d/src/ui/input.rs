//! Input snapshot, keyboard commands and focus traversal.

use super::{UiFrame, widget_id};
use crate::canvas::draw::Rect;
use sgl_core::math::Vec2;

/// One frame of translated input, filled by the app (all positions in
/// logical pixels). `mouse_pressed`/`mouse_released` are this-frame edges;
/// `mouse_down` is the level. `dt` advances the caret-blink clock.
#[derive(Debug, Clone, Default)]
pub struct UiInput {
    pub mouse_pos: Vec2,
    /// The primary button went down this frame (edge).
    pub mouse_pressed: bool,
    /// The primary button went up this frame (edge).
    pub mouse_released: bool,
    /// The primary button is currently held (level).
    pub mouse_down: bool,
    /// Printable characters typed this frame, in order.
    pub chars: Vec<char>,
    /// Backspace was pressed this frame.
    pub backspace: bool,
    /// Logical key presses/repeats. Navigation/dismissal runs first, then at most
    /// one activation; text-edit commands run in vector order before Backspace,
    /// characters and paste. Coalesce Tab/activation to one edge per frame.
    /// The app maps platform shortcuts (e.g. Command-A) to `SelectAll`.
    pub keys: Vec<UiKey>,
    /// Shift extends selections and reverses Tab traversal.
    pub shift: bool,
    /// Clipboard text supplied by the app for a paste command this frame.
    pub paste: Option<String>,
    /// Scroll wheel lines this frame (`y > 0` = up/away, the platform
    /// convention). Consumed by hovered scroll areas.
    pub scroll: Vec2,
    /// Real seconds since the previous frame (cosmetic timers only).
    pub dt: f32,
}

/// Platform-neutral keyboard commands. Defaults contain no commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiKey {
    Tab,
    Enter,
    Space,
    Escape,
    Left,
    Right,
    Home,
    End,
    Delete,
    SelectAll,
    Copy,
    Cut,
}

#[derive(Clone, Copy)]
pub(super) struct FocusTarget {
    pub(super) id: u64,
    pub(super) modal: Option<u64>,
    pub(super) popup: Option<u64>,
}

impl UiFrame<'_> {
    pub(super) fn register_focus(&mut self, id: u64, rect: &Rect) -> bool {
        let visible = rect.size().x > 0.0
            && rect.size().y > 0.0
            && self.clip.is_none_or(|clip| {
                rect.min.x < clip.max.x
                    && rect.max.x > clip.min.x
                    && rect.min.y < clip.max.y
                    && rect.max.y > clip.min.y
            });
        if visible && !self.focus_order.iter().any(|target| target.id == id) {
            self.focus_order.push(FocusTarget {
                id,
                modal: self.modal_scope,
                popup: self.popup_scope,
            });
        }
        visible
            && (!self.ui.modal_blocking || self.modal_scope == self.ui.modal_scope)
            && (self.ui.open_popup.is_none() || self.popup_scope == self.ui.open_popup)
    }

    /// Give keyboard focus to the named line-edit (PR-9: JOIN keeps focus
    /// on the name field when validation fails).
    pub fn focus(&mut self, name: &str) {
        self.focus_requested = true;
        self.ui.focus = Some(widget_id(name));
        // Don't let this frame's click-away handling drop it again.
        self.edit_clicked = true;
    }

    /// Whether the named line-edit currently has keyboard focus (D-14: the
    /// menu shows a placeholder hint only while a field is unfocused).
    #[must_use]
    pub fn is_focused(&self, name: &str) -> bool {
        self.ui.focus == Some(widget_id(name))
    }

    /// Drop keyboard focus from any line-edit (see [`Ui::clear_focus`](super::Ui::clear_focus)).
    pub fn clear_focus(&mut self) {
        self.ui.focus = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::draw::DrawList;
    use crate::ui::test_ui::{BTN, ROW, fixture};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Commands cross real frames, including clipping and a world shortcut
    /// routed only after the UI has had its chance to consume the input.
    #[wasm_bindgen_test(unsupported = test)]
    fn keyboard_traversal_activation_and_removed_focus() {
        let (mut ui, mut text, _assets) = fixture();
        let mut checked = false;
        let mut world_actions = 0;
        for (keys, shift, expected) in [
            (vec![], false, None),
            (vec![UiKey::Tab], false, Some("button")),
            (vec![UiKey::Tab], false, Some("toggle")),
            (vec![UiKey::Tab], true, Some("button")),
            (vec![UiKey::Tab], true, Some("check")),
            (vec![UiKey::Space], false, Some("check")),
        ] {
            let action = keys.contains(&UiKey::Space);
            let mut list = DrawList::new();
            let mut f = ui.begin(
                &mut text,
                &mut list,
                UiInput {
                    keys,
                    shift,
                    ..UiInput::default()
                },
            );
            f.button("button", BTN, "Run", 20.0);
            f.set_clip(Some(Rect::new(0.0, 0.0, 1.0, 1.0)));
            assert!(!f.button("hidden", ROW, "Hidden", 20.0));
            f.set_clip(None);
            f.toggle_button("toggle", ROW, "Toggle", 20.0, false);
            f.checkbox(
                "check",
                Rect::new(10.0, 160.0, 200.0, 30.0),
                "Check",
                20.0,
                &mut checked,
            );
            if let Some(name) = expected {
                assert!(f.is_focused(name));
            }
            f.end();
            if action && !ui.keyboard_captured() {
                world_actions += 1;
            }
        }
        assert!(checked);
        assert_eq!(world_actions, 0);
        let mut list = DrawList::new();
        let f = ui.begin(&mut text, &mut list, UiInput::default());
        f.end();
        assert!(ui.focus.is_none(), "removed control releases focus");
    }
}
