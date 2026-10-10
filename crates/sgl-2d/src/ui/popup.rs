//! Overlay-band content: tooltips, overlay panels, dropdown popovers and the
//! confirmation modal.

use super::{UiFrame, UiKey, contains, widget_id};
use crate::canvas::draw::Rect;
use crate::canvas::text::{HAlign, TextStyle, VAlign};
use sgl_core::math::Vec2;

impl UiFrame<'_> {
    /// A hover tooltip for `anchor`: while the pointer is over the (clip- and
    /// blocking-gated) anchor rect, draws `text` in the overlay band just
    /// below it, clamped into `bounds`. Purely cosmetic — never blocks input.
    pub fn tooltip(&mut self, anchor: Rect, bounds: Rect, text: &str, px: f32) {
        if text.is_empty() || !self.hit(&anchor) {
            return;
        }
        self.draw_tooltip(anchor, bounds, text, px);
    }

    /// Tooltip on hover or keyboard focus, including disabled icon buttons.
    /// The anchor must intersect both the viewport and current clip. Popup and
    /// modal focus scopes apply. Text wraps to the viewport; excess height clips.
    pub fn tooltip_for(&mut self, name: &str, anchor: Rect, bounds: Rect, text: &str, px: f32) {
        let Some(visible) = anchor.intersection(&bounds) else {
            return;
        };
        if self
            .clip
            .is_some_and(|clip| visible.intersection(&clip).is_none())
        {
            return;
        }
        let focused = self.ui.focus == Some(widget_id(name))
            && (!self.ui.modal_blocking || self.modal_scope == self.ui.modal_scope)
            && (self.ui.open_popup.is_none() || self.popup_scope == self.ui.open_popup);
        if !text.is_empty() && (focused || self.hit(&anchor)) {
            self.draw_tooltip(anchor, bounds, text, px);
        }
    }

    fn draw_tooltip(&mut self, anchor: Rect, bounds: Rect, text: &str, px: f32) {
        let pad = 6.0;
        if bounds.size().x <= 0.0 || bounds.size().y <= 0.0 {
            return;
        }
        let available = (bounds.size().x - 4.0).max(0.0);
        let text_width = (available - pad * 2.0).max(1.0);
        let mut lines = Vec::new();
        for paragraph in text.split('\n') {
            let mut line = String::new();
            for word in paragraph.split_whitespace() {
                let candidate = if line.is_empty() {
                    word.to_owned()
                } else {
                    format!("{line} {word}")
                };
                if !line.is_empty() && self.text.measure(&candidate, px).x > text_width {
                    lines.push(std::mem::take(&mut line));
                }
                if !line.is_empty() {
                    line.push(' ');
                }
                for ch in word.chars() {
                    let mut candidate = line.clone();
                    candidate.push(ch);
                    if !line.is_empty() && self.text.measure(&candidate, px).x > text_width {
                        lines.push(std::mem::take(&mut line));
                    }
                    line.push(ch);
                }
            }
            lines.push(line);
        }
        let width = lines
            .iter()
            .map(|line| self.text.measure(line, px).x)
            .fold(0.0, f32::max);
        let w = (width + pad * 2.0).min(available);
        let line_h = self.text.line_height(px);
        let h = (line_h * lines.len() as f32 + pad).min(bounds.size().y);
        let x = anchor
            .min
            .x
            .clamp(bounds.min.x, (bounds.max.x - w).max(bounds.min.x));
        let below = anchor.max.y + 4.0;
        let y = if below + h <= bounds.max.y {
            below
        } else {
            anchor.min.y - h - 4.0
        }
        .clamp(bounds.min.y, (bounds.max.y - h).max(bounds.min.y));
        let rect = Rect::new(x, y, w, h);
        let overlay_was = self.overlay;
        let clip_was = self.clip;
        self.overlay = true;
        self.set_clip(Some(bounds));
        self.rect(rect, self.ui.theme.popup);
        self.border(rect, 1.0, self.ui.theme.accent);
        self.set_clip(Some(rect));
        let style = TextStyle::new(px, self.ui.theme.text).with_outline(self.ui.theme.text_outline);
        for (index, line) in lines.iter().enumerate() {
            self.label(
                Rect::new(
                    x + pad,
                    y + pad * 0.5 + index as f32 * line_h,
                    width,
                    line_h,
                ),
                line,
                &style,
                HAlign::Left,
                VAlign::Top,
            );
        }
        self.set_clip(clip_was);
        self.overlay = overlay_was;
    }

    /// Begin a custom overlay panel (R-17/#25: the editor's File panel):
    /// widgets drawn until [`overlay_panel_end`](Self::overlay_panel_end)
    /// go to the overlay band (above ordinary widgets) and stay
    /// interactive, while `panel` becomes an input blocker for the
    /// ordinary widgets underneath on the following frames — the same
    /// semantics as an open dropdown popover, for panels the caller lays
    /// out itself. Draws the panel backdrop + accent border.
    pub fn overlay_panel_begin(&mut self, panel: Rect) {
        self.overlay = true;
        self.new_blocked.push(panel);
        self.rect(panel, self.ui.theme.popup);
        self.border(panel, 2.0, self.ui.theme.accent);
    }

    /// End the current overlay panel (see
    /// [`overlay_panel_begin`](Self::overlay_panel_begin)).
    pub fn overlay_panel_end(&mut self) {
        self.overlay = false;
    }

    /// A custom dropdown (never a native HTML `select`): a button showing
    /// `options[*selected]`; pressing it opens a popover listing every
    /// option in the overlay band (drawn above and input-blocking the
    /// widgets underneath). Selecting an option stores it and closes;
    /// pressing elsewhere closes. The popover opens downward and stays in
    /// the current clip: only its visible part shows, takes presses and
    /// blocks widgets underneath. Place dropdowns where
    /// `rect.max.y + options·row` stays on screen and inside the clip.
    /// Returns `true` the frame the selection changed.
    pub fn dropdown(
        &mut self,
        name: &str,
        rect: Rect,
        options: &[&str],
        selected: &mut usize,
        px: f32,
    ) -> bool {
        let id = widget_id(name);
        let was_open = self.ui.open_popup == Some(id);

        let (fired, held, hover) = self.button_behavior(name, &rect);
        let bg = if held || was_open {
            self.ui.theme.pressed
        } else if hover {
            self.ui.theme.hovered
        } else {
            self.ui.theme.control
        };
        self.rect(rect, bg);
        let fg = if hover {
            self.ui.theme.text_hovered
        } else {
            self.ui.theme.text
        };
        let style = TextStyle::new(px, fg).with_outline(self.ui.theme.text_outline);
        let pad = 8.0;
        let inner = Rect::new(
            rect.min.x + pad,
            rect.min.y,
            (rect.size().x - 2.0 * pad).max(0.0),
            rect.size().y,
        );
        let current = options.get(*selected).copied().unwrap_or("");
        self.label(inner, current, &style, HAlign::Left, VAlign::Center);
        self.label(
            inner,
            if was_open { "^" } else { "v" },
            &style,
            HAlign::Right,
            VAlign::Center,
        );

        if fired {
            self.ui.open_popup = if was_open { None } else { Some(id) };
        }
        if !was_open || fired {
            return false;
        }

        // Open popover (overlay band).
        let row_h = rect.size().y;
        let pop = Rect::new(
            rect.min.x,
            rect.max.y + 2.0,
            rect.size().x,
            row_h * options.len() as f32,
        );
        // The popover draws and hit-tests within the current clip, so only
        // its visible part blocks widgets underneath or counts as inside.
        let visible = self.clip.map_or(Some(pop), |clip| clip.intersection(&pop));
        let overlay_was = self.overlay;
        self.overlay = true;
        self.popup_scope = Some(id);
        self.new_blocked.extend(visible);
        self.rect(pop, self.ui.theme.popup);
        self.border(pop, 1.0, self.ui.theme.accent);
        let mut changed = false;
        for (i, option) in options.iter().enumerate() {
            let row = Rect::new(pop.min.x, pop.min.y + row_h * i as f32, pop.size().x, row_h);
            let row_name = format!("{name}.opt{i}");
            let (row_fired, row_held, row_hover) = self.button_behavior(&row_name, &row);
            if row_held || row_hover {
                self.rect(
                    row,
                    if row_held {
                        self.ui.theme.pressed
                    } else {
                        self.ui.theme.hovered
                    },
                );
            }
            let fg = if i == *selected {
                self.ui.theme.accent
            } else {
                self.ui.theme.text
            };
            let row_style = TextStyle::new(px, fg).with_outline(self.ui.theme.text_outline);
            let row_inner = Rect::new(row.min.x + pad, row.min.y, row.size().x - 2.0 * pad, row_h);
            self.label(row_inner, option, &row_style, HAlign::Left, VAlign::Center);
            if row_fired {
                *selected = i;
                changed = true;
                self.ui.open_popup = None;
                self.ui.focus = Some(id);
            }
        }
        self.popup_scope = None;
        self.overlay = overlay_was;

        // Click-away close (a press neither on the button nor the visible
        // popover).
        let p = self.input.mouse_pos;
        if self.input.mouse_pressed
            && !contains(&rect, p)
            && !visible.is_some_and(|visible| contains(&visible, p))
        {
            self.ui.open_popup = None;
        }
        changed
    }

    /// A modal confirmation dialog in the overlay band: dims `screen`,
    /// draws `panel` with a title, a body line, and CONFIRM/CANCEL buttons.
    /// While shown it blocks every ordinary widget (input never reaches
    /// them). Returns `Some(true)` on confirm, `Some(false)` on cancel,
    /// `None` otherwise — the **caller** owns the open/closed state.
    #[allow(clippy::too_many_arguments)]
    pub fn confirm_modal(
        &mut self,
        name: &str,
        screen: Rect,
        title: &str,
        body: &str,
        confirm_label: &str,
        cancel_label: &str,
        px: f32,
    ) -> Option<bool> {
        let overlay_was = self.overlay;
        self.overlay = true;
        self.ui.keyboard_captured = true;
        self.ui.open_popup = None;
        let modal_was = self.modal_scope;
        self.modal_scope = Some(widget_id(name));

        let panel_size = Vec2::new(420.0, 160.0);
        let center = (screen.min + screen.max) * 0.5;
        let panel = Rect::new(
            center.x - panel_size.x * 0.5,
            center.y - panel_size.y * 0.5,
            panel_size.x,
            panel_size.y,
        );
        self.rect(screen, self.ui.theme.modal_dim);
        self.rect(panel, self.ui.theme.popup);
        self.border(panel, 2.0, self.ui.theme.accent);

        let title_style = TextStyle::new(px * 1.2, self.ui.theme.text_pressed)
            .with_outline(self.ui.theme.text_outline);
        self.label(
            Rect::new(panel.min.x, panel.min.y + 12.0, panel_size.x, 32.0),
            title,
            &title_style,
            HAlign::Center,
            VAlign::Center,
        );
        let body_style =
            TextStyle::new(px * 0.8, self.ui.theme.text).with_outline(self.ui.theme.text_outline);
        self.label(
            Rect::new(
                panel.min.x + 16.0,
                panel.min.y + 52.0,
                panel_size.x - 32.0,
                32.0,
            ),
            body,
            &body_style,
            HAlign::Center,
            VAlign::Center,
        );

        let btn = Vec2::new(150.0, 44.0);
        let gap = 40.0;
        let y = panel.max.y - btn.y - 16.0;
        let confirm_rect = Rect::new(center.x - btn.x - gap * 0.5, y, btn.x, btn.y);
        let cancel_rect = Rect::new(center.x + gap * 0.5, y, btn.x, btn.y);
        let confirm = self.button(&format!("{name}.confirm"), confirm_rect, confirm_label, px);
        let cancel = self.button(&format!("{name}.cancel"), cancel_rect, cancel_label, px);

        self.overlay = overlay_was;
        self.modal_scope = modal_was;
        let result = if confirm {
            Some(true)
        } else if cancel || (self.input.keys.contains(&UiKey::Escape) && !self.popup_dismissed) {
            Some(false)
        } else {
            None
        };
        // Blocking arms for the next frame only while the modal stays
        // unresolved: on the frame CONFIRM/CANCEL fires the caller closes
        // the modal, and ordinary widgets must be live again immediately
        // (no one-frame lockout after resolution).
        if result.is_none() {
            self.new_modal = Some(widget_id(name));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::draw::DrawList;
    use crate::ui::UiInput;
    use crate::ui::test_ui::{BTN, ROW, fixture, hover_at, press_at};
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test(unsupported = test)]
    fn modal_keyboard_scope_excludes_custom_overlay() {
        let (mut ui, mut text, _assets) = fixture();
        for (index, keys) in [vec![], vec![UiKey::Tab], vec![UiKey::Enter]]
            .into_iter()
            .enumerate()
        {
            let mut list = DrawList::new();
            let mut f = ui.begin(
                &mut text,
                &mut list,
                UiInput {
                    keys,
                    shift: true,
                    ..UiInput::default()
                },
            );
            f.overlay_panel_begin(BTN);
            assert!(!f.button("panel.action", BTN, "Background", 20.0));
            f.overlay_panel_end();
            if index == 0 {
                f.focus("modal.confirm");
            }
            let result = f.confirm_modal(
                "modal",
                Rect::new(0.0, 0.0, 960.0, 540.0),
                "Delete room",
                "Discard changes?",
                "Delete room",
                "Keep room",
                20.0,
            );
            if index == 1 {
                assert!(f.is_focused("modal.cancel"));
            }
            assert_eq!(result, if index == 2 { Some(false) } else { None });
            f.end();
        }
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn popup_keyboard_is_exclusive_and_escape_is_consumed() {
        let (mut ui, mut text, _assets) = fixture();
        let mut selected = 0;
        let dd = Rect::new(10.0, 10.0, 160.0, 30.0);
        for (i, keys) in [
            vec![],
            vec![UiKey::Enter],
            vec![],
            vec![UiKey::Tab],
            vec![UiKey::Tab],
            vec![UiKey::Space],
            vec![UiKey::Enter],
            vec![UiKey::Escape],
        ]
        .into_iter()
        .enumerate()
        {
            let mut list = DrawList::new();
            let mut f = ui.begin(
                &mut text,
                &mut list,
                UiInput {
                    keys,
                    ..UiInput::default()
                },
            );
            if i == 0 {
                f.focus("menu");
            }
            assert!(!f.button("under", ROW, "Under", 20.0));
            f.dropdown("menu", dd, &["a", "b"], &mut selected, 20.0);
            f.end();
            assert!(ui.keyboard_captured());
            if i == 4 {
                assert!(ui.is_focused_name("menu.opt1"));
            }
        }
        assert_eq!(selected, 1);
        assert!(!ui.any_popup_open());
        assert!(ui.is_focused_name("menu"));
    }

    /// Dropdown lifecycle: closed by default; press opens; an option press
    /// selects + closes; while open, the popover region blocks ordinary
    /// widgets underneath on the following frame.
    #[wasm_bindgen_test(unsupported = test)]
    fn dropdown_opens_selects_and_blocks_underneath() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let dd = Rect::new(10.0, 10.0, 160.0, 30.0);
        let options = ["25%", "50%", "100%"];
        let mut selected = 2usize;

        // Frame 1: press the dropdown button → opens, no change.
        let mut f = ui.begin(&mut text, &mut list, press_at(50.0, 25.0));
        assert!(!f.dropdown("zoom", dd, &options, &mut selected, 16.0));
        f.end();
        assert!(ui.any_popup_open());

        // Frame 2: no input; the popover draws. Rows sit below the button:
        // row i occupies y ∈ [42 + 30i, 72 + 30i).
        let mut f = ui.begin(&mut text, &mut list, hover_at(0.0, 0.0, false));
        f.dropdown("zoom", dd, &options, &mut selected, 16.0);
        f.end();

        // Frame 3: an underlying button at the popover position must be
        // blocked while a click there selects option 1 (row y = 72..102).
        let mut f = ui.begin(&mut text, &mut list, press_at(50.0, 85.0));
        let under = f.button("under", Rect::new(10.0, 42.0, 160.0, 90.0), "X", 16.0);
        assert!(!under, "widget under an open popover must not fire");
        let changed = f.dropdown("zoom", dd, &options, &mut selected, 16.0);
        f.end();
        assert!(changed);
        assert_eq!(selected, 1);
        assert!(!ui.any_popup_open(), "selection closes the popover");

        // Reopen, then click far away → closes without selection change.
        let mut f = ui.begin(&mut text, &mut list, press_at(50.0, 25.0));
        f.dropdown("zoom", dd, &options, &mut selected, 16.0);
        f.end();
        assert!(ui.any_popup_open());
        let mut f = ui.begin(&mut text, &mut list, press_at(800.0, 500.0));
        assert!(!f.dropdown("zoom", dd, &options, &mut selected, 16.0));
        f.end();
        assert!(!ui.any_popup_open(), "click-away closes");
        assert_eq!(selected, 1);
    }

    /// #323: a dropdown opened near the bottom of a scroll area blocks only
    /// its visible options; a press on a widget under the clipped-away part
    /// reaches that widget and closes the popover.
    #[wasm_bindgen_test(unsupported = test)]
    fn clipped_dropdown_blocks_only_its_visible_options() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let area = Rect::new(0.0, 0.0, 200.0, 100.0);
        let dd = Rect::new(10.0, 40.0, 160.0, 30.0);
        let options = ["25%", "50%", "100%"];
        let mut selected = 0usize;
        let mut offset = 0.0;
        // Options occupy y ∈ [72, 162); the scroll area shows y < 100.
        let below = Rect::new(10.0, 120.0, 160.0, 30.0);
        let mut fired = Vec::new();
        for input in [
            press_at(50.0, 55.0),
            hover_at(0.0, 0.0, false),
            press_at(50.0, 135.0),
        ] {
            let mut f = ui.begin(&mut text, &mut list, input);
            fired.push(f.button("below", below, "B", 16.0));
            f.scroll_area_begin("list", area, 100.0, &mut offset);
            f.dropdown("zoom", dd, &options, &mut selected, 16.0);
            f.scroll_area_end();
            f.end();
        }
        assert_eq!(fired, [false, false, true]);
        assert_eq!(selected, 0);
        assert!(!ui.any_popup_open(), "the press outside the clip closes it");
    }

    /// A modal blocks every ordinary widget anywhere on screen the frame
    /// after it is shown, while its own buttons stay interactive.
    #[wasm_bindgen_test(unsupported = test)]
    fn modal_blocks_ordinary_widgets() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let screen = Rect::new(0.0, 0.0, 960.0, 540.0);

        // Frame 1: modal drawn → blocking arms for the next frame.
        let mut f = ui.begin(&mut text, &mut list, UiInput::default());
        assert_eq!(
            f.confirm_modal("exit", screen, "Exit?", "Sure?", "EXIT", "CANCEL", 24.0),
            None
        );
        f.end();

        // Frame 2: a press on an ordinary button far from the panel fires
        // nothing; the modal's own confirm button (center-left, y ≈ 310..354)
        // resolves Some(true).
        let mut f = ui.begin(&mut text, &mut list, press_at(50.0, 30.0));
        assert!(!f.button("b", BTN, "OK", 24.0), "modal must block widgets");
        f.end();
        let confirm_center = Vec2::new(480.0 - 75.0 - 20.0, 480.0 * 0.0 + 332.0);
        let mut f = ui.begin(
            &mut text,
            &mut list,
            press_at(confirm_center.x, confirm_center.y),
        );
        let result = f.confirm_modal("exit", screen, "Exit?", "Sure?", "EXIT", "CANCEL", 24.0);
        f.end();
        assert_eq!(result, Some(true));

        // The frame the modal resolved must NOT arm blocking: the very
        // next frame's press on an ordinary button fires (no one-frame
        // lockout after CONFIRM/CANCEL).
        let mut f = ui.begin(&mut text, &mut list, press_at(50.0, 30.0));
        assert!(
            f.button("b", BTN, "OK", 24.0),
            "widgets must be live on the first frame after resolution"
        );
        f.end();
    }

    /// An overlay panel blocks the ordinary widgets underneath on the
    /// following frame while its own widgets stay interactive, and focus
    /// can be dropped explicitly when the panel closes (R-17/#25).
    #[wasm_bindgen_test(unsupported = test)]
    fn overlay_panel_blocks_underneath_and_focus_clears() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let panel = Rect::new(100.0, 100.0, 300.0, 200.0);

        // Frame 1: the panel is drawn → blocking arms for the next frame.
        let mut f = ui.begin(&mut text, &mut list, UiInput::default());
        f.overlay_panel_begin(panel);
        f.button("panel.ok", Rect::new(120.0, 120.0, 100.0, 40.0), "OK", 16.0);
        f.overlay_panel_end();
        f.end();

        // Frame 2: a press inside the panel region fires the panel's own
        // button but never the ordinary widget underneath.
        let mut f = ui.begin(&mut text, &mut list, press_at(150.0, 140.0));
        let under = f.button("under", Rect::new(100.0, 100.0, 300.0, 200.0), "U", 16.0);
        assert!(!under, "ordinary widget under the panel must be blocked");
        f.overlay_panel_begin(panel);
        let ok = f.button("panel.ok", Rect::new(120.0, 120.0, 100.0, 40.0), "OK", 16.0);
        f.overlay_panel_end();
        f.end();
        assert!(ok, "the panel's own widget stays interactive");

        // A focused line-edit inside the panel releases focus explicitly
        // when the caller closes the panel.
        let field = Rect::new(120.0, 180.0, 200.0, 30.0);
        let mut buf = String::new();
        let mut f = ui.begin(&mut text, &mut list, press_at(150.0, 190.0));
        f.overlay_panel_begin(panel);
        f.line_edit("panel.name", field, &mut buf, 50, 16.0);
        f.overlay_panel_end();
        f.end();
        assert!(ui.has_focus());
        ui.clear_focus();
        assert!(!ui.has_focus());
    }

    /// Tooltips draw only while their anchor is hovered, land in the
    /// overlay z band, and never become input blockers.
    #[wasm_bindgen_test(unsupported = test)]
    fn tooltip_draws_on_hover_only_and_never_blocks() {
        let (mut ui, mut text, mut assets) = fixture();
        let screen = Rect::new(0.0, 0.0, 960.0, 540.0);
        let anchor = Rect::new(10.0, 10.0, 100.0, 30.0);

        // Not hovered: nothing is drawn.
        let mut list = DrawList::new();
        let mut f = ui.begin(&mut text, &mut list, hover_at(500.0, 300.0, false));
        f.tooltip(anchor, screen, "Collision overlay", 13.0);
        f.end();
        text.end_frame(&mut assets, &mut list);
        assert!(list.screen.is_empty(), "unhovered tooltip must not draw");

        // Hovered: backdrop + border + glyphs, all in the overlay band.
        let mut list = DrawList::new();
        let mut f = ui.begin(&mut text, &mut list, hover_at(50.0, 25.0, false));
        f.tooltip(anchor, screen, "Collision overlay", 13.0);
        f.end();
        text.end_frame(&mut assets, &mut list);
        assert!(!list.screen.is_empty(), "hovered tooltip draws");
        assert!(
            list.screen.iter().all(|i| i.z > 4_000.0),
            "tooltips live in the overlay z band"
        );

        // A tooltip never blocks: the frame after drawing one, a button
        // underneath its rect still fires.
        let mut list = DrawList::new();
        let mut f = ui.begin(&mut text, &mut list, press_at(50.0, 55.0));
        assert!(
            f.button("under", Rect::new(10.0, 40.0, 100.0, 30.0), "B", 13.0),
            "tooltips must not become input blockers"
        );
        f.end();
    }
}
