//! Single-line text fields: line edits, their clear button and the masked
//! secret field, with caret, selection and clipboard editing.

use super::{CARET_BLINK, UiFrame, UiInput, UiKey, widget_id};
use crate::canvas::draw::Rect;
use crate::canvas::text::{HAlign, TextStyle, VAlign};
use zeroize::Zeroize as _;

#[derive(Default)]
pub(super) struct EditState {
    pub(super) id: Option<u64>,
    pub(super) caret: usize,
    pub(super) anchor: usize,
    pub(super) scroll: f32,
}

impl UiFrame<'_> {
    /// Single-line text field: click to focus at the end; Left/Right and
    /// Home/End move the caret, Shift extends selection, and `SelectAll` selects
    /// the buffer. Typing/paste replaces selection; Backspace/Delete remove
    /// adjacent characters or selection. Length is capped in Unicode scalar
    /// values, preserving UTF-8. Stable names retain caret/selection across
    /// redraws; the caller owns the draft. Returns whether `buf` changed.
    pub fn line_edit(
        &mut self,
        name: &str,
        rect: Rect,
        buf: &mut String,
        max_len: usize,
        px: f32,
    ) -> bool {
        self.line_edit_impl(name, rect, buf, max_len, px, false)
    }

    /// A secret field with ASCII append/backspace editing and paste. Navigation,
    /// selection, copy and cut commands are ignored; clearing zeroizes the
    /// buffer. Draws and metrics only receive mask glyphs. Unlike normal line
    /// edits this preserves the preallocated secret buffer without temporary
    /// copies during editing.
    pub fn password_edit_clear(
        &mut self,
        name: &str,
        rect: Rect,
        buf: &mut String,
        max_len: usize,
        px: f32,
    ) -> bool {
        let side = rect.size().y;
        let btn = Rect::new(rect.max.x - side, rect.min.y, side, side);
        let mut cleared = false;
        if !buf.is_empty() && self.input.mouse_pressed && self.hit(&btn) {
            zeroize_string(buf);
            cleared = true;
        }
        let edited = self.line_edit_impl(name, rect, buf, max_len, px, true);
        if !buf.is_empty() {
            let hover = self.hit(&btn);
            let fg = if hover {
                self.ui.theme.text_hovered
            } else {
                self.ui.theme.text
            };
            let style = TextStyle::new(px, fg).with_outline(self.ui.theme.text_outline);
            self.label(btn, "x", &style, HAlign::Center, VAlign::Center);
        }
        cleared || edited
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn line_edit_impl(
        &mut self,
        name: &str,
        rect: Rect,
        buf: &mut String,
        max_len: usize,
        px: f32,
        masked: bool,
    ) -> bool {
        let id = widget_id(name);
        let eligible = self.register_focus(id, &rect);
        let hover = self.hit(&rect);
        let clicked = self.input.mouse_pressed && hover;
        if clicked {
            self.ui.focus = Some(id);
            self.edit_clicked = true;
        }
        let focused = self.ui.focus == Some(id);
        if focused && (clicked || self.ui.edit.id != Some(id)) {
            self.ui.edit = EditState {
                id: Some(id),
                caret: buf.chars().count(),
                anchor: buf.chars().count(),
                scroll: 0.0,
            };
        }
        // This frame's characters go to exactly one field, whatever the
        // widget order: the one the frame's press landed on, or the focused
        // one when there was no press. A field focused before this frame
        // must not also consume them when the press moves focus elsewhere
        // (#245); a press on nothing drops them along with the focus.
        let receives = clicked || (eligible && focused && !self.input.mouse_pressed);
        let changed = if receives {
            self.ui.keyboard_captured = true;
            if masked {
                let changed =
                    edit_apply_secret(buf, &self.input.chars, self.input.backspace, max_len);
                let pasted = self.input.paste.as_deref().is_some_and(|paste| {
                    // Feed one character at a time without copying the secret.
                    let mut changed = false;
                    for c in paste.chars() {
                        changed |= edit_apply_secret(buf, &[c], false, max_len);
                    }
                    changed
                });
                self.ui.edit.caret = buf.chars().count();
                self.ui.edit.anchor = self.ui.edit.caret;
                changed || pasted
            } else {
                apply_edit(
                    buf,
                    &mut self.ui.edit,
                    &self.input,
                    max_len,
                    &mut self.ui.clipboard_text,
                )
            }
        } else {
            false
        };

        // Focus border + body.
        if focused {
            self.border(rect, 1.0, self.ui.theme.accent);
        }
        self.rect(rect, self.ui.theme.control);

        // Text, left-aligned with padding, vertically centered.
        let pad = 5.0;
        let trailing = if masked { rect.size().y } else { 0.0 };
        let inner = Rect::new(
            rect.min.x + pad,
            rect.min.y,
            (rect.size().x - 2.0 * pad - trailing).max(0.0),
            rect.size().y,
        );
        let style = TextStyle::new(px, self.ui.theme.text).with_outline(self.ui.theme.text_outline);
        let display = masked_display(masked, buf);
        let display = display.as_deref().unwrap_or(buf);
        // Draw selection and caret using character indices, never byte offsets.
        let caret_index = self.ui.edit.caret.min(buf.chars().count());
        let anchor = self.ui.edit.anchor.min(buf.chars().count());
        let prefix_width = |index: usize| {
            self.text
                .measure(&display.chars().take(index).collect::<String>(), px)
                .x
        };
        let caret_x = prefix_width(caret_index);
        let left = prefix_width(anchor.min(caret_index));
        let right = prefix_width(anchor.max(caret_index));
        if focused {
            self.ui.edit.scroll = self.ui.edit.scroll.min(caret_x);
            self.ui.edit.scroll = self
                .ui
                .edit
                .scroll
                .max(caret_x - (inner.size().x - 3.0).max(0.0));
        }
        let scroll = if focused { self.ui.edit.scroll } else { 0.0 };
        let clip_was = self.clip;
        self.set_clip(Some(self.clip_within(inner)));
        let text_rect = Rect::new(
            inner.min.x - scroll,
            inner.min.y,
            inner.size().x + scroll,
            inner.size().y,
        );
        self.label(text_rect, display, &style, HAlign::Left, VAlign::Center);
        if focused && anchor != caret_index {
            self.rect(
                Rect::new(
                    inner.min.x + left - scroll,
                    inner.min.y,
                    right - left,
                    inner.size().y,
                ),
                self.ui.theme.selection,
            );
        }
        // Caret at the insertion point, blinking.
        if focused && self.ui.clock < CARET_BLINK * 0.5 {
            let text_w = caret_x;
            let caret_h = self.text.line_height(px);
            let caret = Rect::new(
                inner.min.x + text_w + 1.0 - scroll,
                rect.min.y + (rect.size().y - caret_h) * 0.5,
                2.0,
                caret_h,
            );
            self.rect(caret, self.ui.theme.caret);
        }
        self.set_clip(clip_was);
        changed
    }

    /// A [`line_edit`](Self::line_edit) with a Godot-style clear button
    /// (PR-9 name field): while the buffer is non-empty an `x` occupies the
    /// square at the right edge; pressing it empties the buffer (the click
    /// still lands inside the field, so the edit keeps/gains focus, like
    /// Godot). Returns whether `buf` changed.
    pub fn line_edit_clear(
        &mut self,
        name: &str,
        rect: Rect,
        buf: &mut String,
        max_len: usize,
        px: f32,
    ) -> bool {
        let side = rect.size().y;
        let btn = Rect::new(rect.max.x - side, rect.min.y, side, side);
        let mut cleared = false;
        if !buf.is_empty() && self.input.mouse_pressed && self.hit(&btn) {
            buf.clear();
            cleared = true;
        }
        let edited = self.line_edit(name, rect, buf, max_len, px);
        if !buf.is_empty() {
            let hover = self.hit(&btn);
            let fg = if hover {
                self.ui.theme.text_hovered
            } else {
                self.ui.theme.text
            };
            let style = TextStyle::new(px, fg).with_outline(self.ui.theme.text_outline);
            self.label(btn, "x", &style, HAlign::Center, VAlign::Center);
        }
        cleared || edited
    }
}

fn edit_apply_secret(buf: &mut String, chars: &[char], backspace: bool, max_len: usize) -> bool {
    let mut changed = false;
    if backspace && !buf.is_empty() {
        let new_len = buf.char_indices().next_back().map_or(0, |(index, _)| index);
        let mut bytes = std::mem::take(buf).into_bytes();
        bytes[new_len..].zeroize();
        bytes.truncate(new_len);
        *buf = String::from_utf8(bytes).expect("truncating at a char boundary preserves UTF-8");
        changed = true;
    }
    for &character in chars {
        // Secret fields currently carry the invite capability, whose wire
        // format is ASCII. Keeping edits single-byte also guarantees a
        // preallocated max-length buffer never reallocates secret prefixes.
        if !character.is_ascii() || character.is_control() {
            continue;
        }
        if buf.chars().count() >= max_len {
            break;
        }
        buf.push(character);
        changed = true;
    }
    changed
}

fn apply_edit(
    buf: &mut String,
    state: &mut EditState,
    input: &UiInput,
    max_len: usize,
    clipboard: &mut Option<String>,
) -> bool {
    let mut chars: Vec<char> = buf.chars().collect();
    state.caret = state.caret.min(chars.len());
    state.anchor = state.anchor.min(chars.len());
    for key in &input.keys {
        let (lo, hi) = (state.caret.min(state.anchor), state.caret.max(state.anchor));
        let moved = match key {
            UiKey::Left => Some(if !input.shift && lo != hi {
                lo
            } else {
                state.caret.saturating_sub(1)
            }),
            UiKey::Right => Some(if !input.shift && lo != hi {
                hi
            } else {
                (state.caret + 1).min(chars.len())
            }),
            UiKey::Home => Some(0),
            UiKey::End => Some(chars.len()),
            _ => None,
        };
        if let Some(at) = moved {
            state.caret = at;
            if !input.shift {
                state.anchor = at;
            }
        }
        match key {
            UiKey::SelectAll => {
                state.anchor = 0;
                state.caret = chars.len();
            }
            UiKey::Copy | UiKey::Cut if lo != hi => {
                *clipboard = Some(chars[lo..hi].iter().collect());
                if *key == UiKey::Cut {
                    chars.drain(lo..hi);
                    state.caret = lo;
                    state.anchor = lo;
                }
            }
            UiKey::Delete => {
                let end = if lo == hi {
                    (hi + 1).min(chars.len())
                } else {
                    hi
                };
                chars.drain(lo..end);
                state.caret = lo;
                state.anchor = lo;
            }
            _ => {}
        }
    }
    if input.backspace {
        let lo = state.caret.min(state.anchor);
        let hi = state.caret.max(state.anchor);
        let start = if lo == hi { lo.saturating_sub(1) } else { lo };
        chars.drain(start..hi);
        state.caret = start;
        state.anchor = start;
    }
    let incoming = input
        .chars
        .iter()
        .copied()
        .chain(input.paste.as_deref().unwrap_or("").chars());
    for c in incoming.filter(|c| !c.is_control()) {
        let lo = state.caret.min(state.anchor);
        let hi = state.caret.max(state.anchor);
        if chars.len() - (hi - lo) >= max_len {
            break;
        }
        chars.splice(lo..hi, [c]);
        state.caret = lo + 1;
        state.anchor = state.caret;
    }
    let changed = !chars.iter().copied().eq(buf.chars());
    if changed {
        buf.clear();
        buf.extend(chars.iter());
    }
    chars.zeroize();
    changed
}

fn zeroize_string(value: &mut String) {
    std::mem::take(value).into_bytes().zeroize();
}

fn masked_display(masked: bool, value: &str) -> Option<String> {
    masked.then(|| "•".repeat(value.chars().count()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::draw::DrawList;
    use crate::canvas::text::TextRenderer;
    use crate::ui::Ui;
    use crate::ui::test_ui::{BTN, ROW, fixture, press_at};
    use sgl_core::math::Vec2;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test(unsupported = test)]
    fn clicking_selected_edit_collapses_selection_to_end() {
        let (mut ui, mut text, _assets) = fixture();
        let mut draft = "abc".to_owned();
        for (index, input) in [
            UiInput {
                keys: vec![UiKey::SelectAll],
                ..UiInput::default()
            },
            UiInput {
                mouse_pos: BTN.min + Vec2::splat(5.0),
                mouse_pressed: true,
                ..UiInput::default()
            },
            UiInput {
                chars: vec!['X'],
                ..UiInput::default()
            },
        ]
        .into_iter()
        .enumerate()
        {
            let mut list = DrawList::new();
            let mut f = ui.begin(&mut text, &mut list, input);
            if index == 0 {
                f.focus("draft");
            }
            f.line_edit("draft", BTN, &mut draft, 50, 20.0);
            f.end();
        }
        assert_eq!(draft, "abcX");
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn keyboard_selection_clipboard_and_draft_survive_redraw() {
        let (mut ui, mut text, _assets) = fixture();
        let mut draft = String::from("aé猫z");
        let sequence = [
            (
                UiInput {
                    keys: vec![UiKey::Home],
                    ..UiInput::default()
                },
                "aé猫z",
            ),
            (
                UiInput {
                    keys: vec![UiKey::Right, UiKey::Right],
                    shift: true,
                    ..UiInput::default()
                },
                "aé猫z",
            ),
            (
                UiInput {
                    keys: vec![UiKey::Copy],
                    ..UiInput::default()
                },
                "aé猫z",
            ),
            (
                UiInput {
                    chars: vec!['Q'],
                    ..UiInput::default()
                },
                "Q猫z",
            ),
            (UiInput::default(), "Q猫z"),
            (
                UiInput {
                    keys: vec![UiKey::Delete],
                    ..UiInput::default()
                },
                "Qz",
            ),
            (
                UiInput {
                    paste: Some("é猫\n".into()),
                    ..UiInput::default()
                },
                "Qé猫z",
            ),
            (
                UiInput {
                    keys: vec![UiKey::SelectAll, UiKey::Cut],
                    ..UiInput::default()
                },
                "",
            ),
            (
                UiInput {
                    paste: Some("123456".into()),
                    ..UiInput::default()
                },
                "12345",
            ),
            (
                UiInput {
                    keys: vec![UiKey::End],
                    backspace: true,
                    ..UiInput::default()
                },
                "1234",
            ),
        ];
        for (i, (input, expected)) in sequence.into_iter().enumerate() {
            let mut list = DrawList::new();
            let mut f = ui.begin(&mut text, &mut list, input);
            if i == 0 {
                f.focus("draft");
            }
            // Unrelated widgets can change without replacing the field state.
            if i % 2 == 0 {
                f.button("unrelated", ROW, "Other", 20.0);
            }
            f.line_edit("draft", BTN, &mut draft, 5, 20.0);
            f.end();
            assert_eq!(draft, expected, "input frame {i}");
            assert!(ui.has_focus());
            if i == 2 {
                assert_eq!(ui.take_clipboard_text().as_deref(), Some("aé"));
            }
            if i == 7 {
                assert_eq!(ui.take_clipboard_text().as_deref(), Some("Qé猫z"));
            }
        }
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn long_draft_caret_stays_visible_and_modal_escape_captures() {
        let (mut ui, mut text, _assets) = fixture();
        let field = Rect::new(20.0, 20.0, 80.0, 30.0);
        let mut draft = "a long filename for an editor room".to_owned();
        let mut list = DrawList::new();
        let mut f = ui.begin(&mut text, &mut list, UiInput::default());
        f.focus("draft");
        f.line_edit("draft", field, &mut draft, 80, 20.0);
        f.end();
        // The last quad is the 2 px caret. Its right edge must stay inside
        // the field's 5 px inset, even when the draft is wider than the field.
        let caret = list.screen.last().unwrap();
        assert!(caret.pos.x + caret.scale.x * 0.5 <= 95.0);
        assert!(caret.pos.x - caret.scale.x * 0.5 >= 25.0);
        assert_eq!(caret.clip, Some(Rect::new(25.0, 20.0, 70.0, 30.0)));
        let mut list = DrawList::new();
        let mut f = ui.begin(
            &mut text,
            &mut list,
            UiInput {
                keys: vec![UiKey::Home],
                ..UiInput::default()
            },
        );
        f.line_edit("draft", field, &mut draft, 80, 20.0);
        f.end();
        assert_eq!(list.screen.last().unwrap().pos.x, 27.0);

        ui.clear_focus();
        let mut f = ui.begin(
            &mut text,
            &mut list,
            UiInput {
                keys: vec![UiKey::Escape],
                ..UiInput::default()
            },
        );
        assert_eq!(
            f.confirm_modal(
                "confirm",
                Rect::new(0.0, 0.0, 960.0, 540.0),
                "Title",
                "Body",
                "OK",
                "Cancel",
                20.0
            ),
            Some(false)
        );
        f.end();
        assert!(
            ui.keyboard_captured(),
            "Escape never reaches a world action"
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn clipped_edit_ignores_input_and_password_never_copies() {
        let (mut ui, mut text, _assets) = fixture();
        let mut secret = "secret".to_owned();
        let mut list = DrawList::new();
        let mut f = ui.begin(
            &mut text,
            &mut list,
            UiInput {
                keys: vec![UiKey::SelectAll, UiKey::Copy, UiKey::Cut],
                ..UiInput::default()
            },
        );
        f.focus("secret");
        f.password_edit_clear("secret", BTN, &mut secret, 20, 20.0);
        f.end();
        assert!(ui.take_clipboard_text().is_none());
        assert_eq!(secret, "secret");
        let mut f = ui.begin(
            &mut text,
            &mut list,
            UiInput {
                chars: vec!['X'],
                ..UiInput::default()
            },
        );
        f.set_clip(Some(Rect::new(0.0, 0.0, 1.0, 1.0)));
        assert!(!f.password_edit_clear("secret", BTN, &mut secret, 20, 20.0));
        f.end();
        assert_eq!(secret, "secret");
        assert!(!ui.has_focus());
    }

    /// Line edit: click focuses; typing inserts up to `max_len`; backspace
    /// deletes; clicking elsewhere unfocuses and typing then does nothing.
    #[wasm_bindgen_test(unsupported = test)]
    fn line_edit_focus_and_editing() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let field = Rect::new(10.0, 10.0, 300.0, 40.0);
        let mut buf = String::new();

        // Unfocused typing is ignored.
        let input = UiInput {
            chars: vec!['x'],
            ..UiInput::default()
        };
        let mut f = ui.begin(&mut text, &mut list, input);
        assert!(!f.line_edit("name", field, &mut buf, 50, 24.0));
        f.end();
        assert_eq!(buf, "");

        // Click inside → focus, then type.
        let mut f = ui.begin(&mut text, &mut list, press_at(50.0, 30.0));
        f.line_edit("name", field, &mut buf, 50, 24.0);
        f.end();
        assert!(ui.has_focus());

        let input = UiInput {
            chars: vec!['H', 'i'],
            ..UiInput::default()
        };
        let mut f = ui.begin(&mut text, &mut list, input);
        assert!(f.line_edit("name", field, &mut buf, 50, 24.0));
        f.end();
        assert_eq!(buf, "Hi");

        // Backspace deletes the last char.
        let input = UiInput {
            backspace: true,
            ..UiInput::default()
        };
        let mut f = ui.begin(&mut text, &mut list, input);
        assert!(f.line_edit("name", field, &mut buf, 50, 24.0));
        f.end();
        assert_eq!(buf, "H");

        // Click away → unfocused; typing is ignored again.
        let mut f = ui.begin(&mut text, &mut list, press_at(800.0, 500.0));
        f.line_edit("name", field, &mut buf, 50, 24.0);
        f.end();
        assert!(!ui.has_focus());
    }

    /// #245: on the frame a press moves focus from one field to another, the
    /// typed characters reach only the newly focused field — in either widget
    /// order — and a press on nothing drops them with the focus.
    #[wasm_bindgen_test(unsupported = test)]
    fn characters_on_a_focus_moving_frame_reach_only_the_new_field() {
        let first = Rect::new(10.0, 10.0, 300.0, 40.0);
        let second = Rect::new(10.0, 60.0, 300.0, 40.0);
        for first_runs_first in [true, false] {
            let (mut ui, mut text, _assets) = fixture();
            let mut list = DrawList::new();
            let (mut a, mut b) = (String::new(), String::new());
            let mut run = |ui: &mut Ui,
                           text: &mut TextRenderer,
                           input: UiInput,
                           a: &mut String,
                           b: &mut String| {
                let mut f = ui.begin(text, &mut list, input);
                if first_runs_first {
                    f.line_edit("a", first, a, 50, 24.0);
                    f.line_edit("b", second, b, 50, 24.0);
                } else {
                    f.line_edit("b", second, b, 50, 24.0);
                    f.line_edit("a", first, a, 50, 24.0);
                }
                f.end();
            };
            run(&mut ui, &mut text, press_at(50.0, 30.0), &mut a, &mut b);
            assert!(ui.is_focused_name("a"));

            // Press on the second field while typing "x".
            let input = UiInput {
                chars: vec!['x'],
                ..press_at(50.0, 80.0)
            };
            run(&mut ui, &mut text, input, &mut a, &mut b);
            assert_eq!(
                (a.as_str(), b.as_str()),
                ("", "x"),
                "order first={first_runs_first}"
            );

            // Press on nothing while typing "y": focus and characters both go.
            let input = UiInput {
                chars: vec!['y'],
                ..press_at(800.0, 500.0)
            };
            run(&mut ui, &mut text, input, &mut a, &mut b);
            assert_eq!((a.as_str(), b.as_str()), ("", "x"));
            assert!(!ui.has_focus());
        }
    }

    /// The clear button (PR-9): pressing the right-edge square empties the
    /// buffer and the field keeps focus; the button is inert while empty.
    #[wasm_bindgen_test(unsupported = test)]
    fn line_edit_clear_button() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let field = Rect::new(10.0, 10.0, 300.0, 40.0);
        let mut buf = String::from("Bob");

        // Press inside the clear square (right edge, 40×40).
        let mut f = ui.begin(&mut text, &mut list, press_at(295.0, 30.0));
        assert!(f.line_edit_clear("name", field, &mut buf, 50, 24.0));
        f.end();
        assert_eq!(buf, "");
        assert!(ui.has_focus(), "clear click still focuses the field");

        // Empty buffer: the same press is a plain focus click, no change.
        let mut f = ui.begin(&mut text, &mut list, press_at(295.0, 30.0));
        assert!(!f.line_edit_clear("name", field, &mut buf, 50, 24.0));
        f.end();
        assert_eq!(buf, "");
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn password_edit_never_exposes_the_secret_as_display_text() {
        let secret = "12abcdef";
        let display = masked_display(true, secret).expect("masked display");
        assert_eq!(display, "••••••••");
        assert!(!display.contains(secret));

        let mut value = String::with_capacity(64);
        value.push_str(secret);
        assert!(edit_apply_secret(&mut value, &[], true, 64));
        assert_eq!(value, "12abcde");
        zeroize_string(&mut value);
        assert!(value.is_empty());
    }
}
