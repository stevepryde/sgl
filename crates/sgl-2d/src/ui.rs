//! Immediate-mode UI over the [`DrawList`] screen channel (AR-6): the app
//! rebuilds every widget each frame; the only retained state is interaction
//! (which widget the mouse went down on, which line-edit has focus, a blink
//! clock).
//!
//! Frame shape:
//! ```text
//!   let mut frame = ui.begin(&mut text, &mut list, input); // UiInput from the app
//!   frame.panel(..); frame.button(..); frame.line_edit(..);
//!   frame.end();
//!   let uploads = text.end_frame(&mut assets.textures, &mut list);
//!   // upload `uploads` + render `list`
//! ```
//!
//! The app owns input translation: it fills [`UiInput`] (mouse in **logical**
//! pixels, printable chars, navigation/edit commands) from whatever platform
//! events it receives — `ui` never touches `platform`. Translate platform
//! select-all/copy/cut shortcuts to [`UiKey`], supply clipboard paste text in
//! [`UiInput::paste`], and drain [`Ui::take_clipboard_text`] to the clipboard.
//! After ending the frame, consult [`Ui::keyboard_captured`] before dispatching
//! world shortcuts so Tab, activation, editing and Escape have one owner.
//! Widgets use stable names for focus and edit selection. Tab traverses visible
//! widgets in the preceding frame's submission order (Shift reverses), wrapping
//! and staying inside the open popup/modal. Missing or fully clipped widgets
//! lose focus at frame end. `has_focus` retains its text-input meaning.
//!
//! [`UiTheme::default`] preserves the original translucent game palette;
//! [`UiTheme::tools_dark`] and [`UiTheme::tools_light`] provide opaque tool
//! chrome. Copy [`Ui::theme`] for caller-composed labels and surfaces. Buttons
//! **act on press-down**, not release.
//!
//! # Composing tools (including agent-authored interfaces)
//!
//! Derive layout and input from the current logical viewport in points, not a
//! fixed resolution. Map physical pointer coordinates through the platform's
//! DPI/letterbox transform once; configure text raster scale for that mapping.
//! Bound pane sizes against the viewport; [`UiFrame::splitter`] only changes a
//! caller-owned extent. Measure labels with [`TextRenderer::measure`], reserve
//! trailing row actions first, then clip or wrap the remaining label/help area.
//! Give each overflowing region one [`UiFrame::scroll_area_begin`] and its own offset;
//! restore enclosing clips and keep fixed toolbars outside scrolling content.
//!
//! Use text for unfamiliar/consequential commands and compact icons for repeated
//! familiar actions. Pair icons with [`UiFrame::tooltip_for`] using the same
//! stable name: keyboard focus also reveals help, including disabled reasons.
//! Keep hit targets larger than glyphs and retain visible focus/selection marks;
//! state must not depend only on color. Submit in useful keyboard traversal order.
//! Route keyboard/pointer capture before game shortcuts or world gestures, and
//! cancel interactions on window focus loss. The game owns layout, persistence,
//! undo and confirmation; use explicit labels such as “Delete layer”/“Cancel”.
//! See `specs/client.md` tool composition and the runnable `tool_ui` example.

// Immediate input snapshots intentionally expose independent edge/state flags;
// bounded widget indices and frame counters are converted to logical pixels.
#![allow(
    clippy::cast_precision_loss,
    clippy::float_cmp,
    clippy::items_after_statements,
    clippy::struct_excessive_bools
)]

use crate::assets::{Assets, Handle, Texture};
use crate::canvas::draw::{DrawList, Rect, SpriteInstance};
use crate::canvas::text::{HAlign, TextChannel, TextRenderer, TextStyle, VAlign};
use sgl_core::math::Vec2;

mod button;
mod input;
mod layout;
mod number_field;
mod popup;
#[cfg(test)]
mod test_ui;
mod text_edit;
mod theme;

pub use crate::fps::FpsCounter;
pub use button::IconButton;
pub use input::{UiInput, UiKey};
pub use layout::{Splitter, SplitterAxis, SplitterResponse, UiCursor};
pub use number_field::NumberField;
pub use theme::UiTheme;

use input::FocusTarget;
use layout::SplitterDrag;
use number_field::NumberDrag;
use text_edit::EditState;

// --- Godot-4-default-theme-ish palette (sRGB, straight alpha). ---

/// Panel background (Godot default `Panel` style box).
pub const PANEL_COLOR: [f32; 4] = [0.1, 0.1, 0.1, 0.6];
const BUTTON_NORMAL: [f32; 4] = [0.1, 0.1, 0.1, 0.6];
const BUTTON_HOVER: [f32; 4] = [0.225, 0.225, 0.225, 0.6];
const BUTTON_PRESSED: [f32; 4] = [0.0, 0.0, 0.0, 0.6];
const FONT_NORMAL: [f32; 4] = [0.875, 0.875, 0.875, 1.0];
const FONT_HOVER: [f32; 4] = [0.95, 0.95, 0.95, 1.0];
const FONT_PRESSED: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// Godot default accent blue (focus/selection borders).
const ACCENT: [f32; 4] = [0.44, 0.73, 0.98, 1.0];
const CARET: [f32; 4] = [0.95, 0.95, 0.95, 1.0];
/// Caret blink period in seconds (on for the first half).
const CARET_BLINK: f32 = 1.0;
/// Non-title text carries a 1 px black outline (PR-9: outline 10 for the
/// title, 1 elsewhere).
const WIDGET_OUTLINE: f32 = 1.0;

/// Stable widget identity, hashed from the caller's name string (FNV-1a).
fn widget_id(name: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn contains(rect: &Rect, p: Vec2) -> bool {
    p.x >= rect.min.x && p.x < rect.max.x && p.y >= rect.min.y && p.y < rect.max.y
}

const SCROLLBAR_TRACK: [f32; 4] = [0.0, 0.0, 0.0, 0.35];
const SCROLLBAR_THUMB: [f32; 4] = [0.5, 0.5, 0.5, 0.8];
const SCROLLBAR_THUMB_HOVER: [f32; 4] = [0.65, 0.65, 0.65, 0.9];
/// The z band overlay content (popovers, modals) draws in — above every
/// ordinary widget of the frame.
const OVERLAY_Z: f32 = 5_000.0;
/// Popover/modal backdrop colors.
const POPOVER_BG: [f32; 4] = [0.08, 0.08, 0.08, 0.98];
const MODAL_DIM: [f32; 4] = [0.0, 0.0, 0.0, 0.55];

/// Persistent UI interaction state + the shared 1×1 white texture used for
/// every flat rect. Create once; `begin` each frame.
pub struct Ui {
    white: Handle<Texture>,
    theme: UiTheme,
    /// Widget the primary button went down on (pressed visual until release).
    active: Option<u64>,
    /// Focused widget.
    focus: Option<u64>,
    focus_order: Vec<FocusTarget>,
    edit: EditState,
    keyboard_captured: bool,
    clipboard_text: Option<String>,
    /// Caret-blink clock, advanced by `UiInput::dt`.
    clock: f32,
    /// The open dropdown/popover, if any (retained across frames).
    open_popup: Option<u64>,
    /// Overlay regions drawn **last** frame (popovers + modal backdrops):
    /// ordinary widgets ignore the mouse inside them this frame, so a click
    /// on an open popover never falls through to the widget underneath.
    blocked: Vec<Rect>,
    /// A modal was drawn last frame: ordinary widgets ignore ALL input.
    modal_blocking: bool,
    modal_scope: Option<u64>,
    /// Scrollbar-thumb drag: `(widget id, grab offset within the thumb)`.
    thumb_drag: Option<(u64, f32)>,
    /// A numeric field's value scrub in progress (see [`UiFrame::number_field`]).
    number_drag: Option<NumberDrag>,
    /// The text of the numeric field that has keyboard focus (the field
    /// re-parses it whenever it changes; dropped with the focus).
    number_edit: String,
    splitter_drag: Option<SplitterDrag>,
    pointer_consumed: bool,
}

impl Ui {
    /// Register the shared white pixel texture in `assets` and build the UI
    /// state. The app must upload [`white_texture`](Self::white_texture) to
    /// the renderer once, like any other texture.
    pub fn new(assets: &mut Assets<Texture>) -> Self {
        let white = crate::assets::white_texture(assets);
        Self {
            white,
            theme: UiTheme::default(),
            active: None,
            focus: None,
            focus_order: Vec::new(),
            edit: EditState::default(),
            keyboard_captured: false,
            clipboard_text: None,
            clock: 0.0,
            open_popup: None,
            blocked: Vec::new(),
            modal_blocking: false,
            modal_scope: None,
            thumb_drag: None,
            number_drag: None,
            number_edit: String::new(),
            splitter_drag: None,
            pointer_consumed: false,
        }
    }

    /// Select semantic colors for subsequent frames. Interaction state survives
    /// theme changes; the game owns choosing and persisting the preference.
    pub fn set_theme(&mut self, theme: UiTheme) {
        self.theme = theme;
    }

    /// Current semantic colors, also available to caller-composed labels/rects.
    #[must_use]
    pub fn theme(&self) -> &UiTheme {
        &self.theme
    }

    /// The shared 1×1 white texture handle (upload once at startup). This is
    /// the same handle [`crate::assets::white_texture`] hands every other
    /// consumer, so the UI and an overlay share one upload.
    #[must_use]
    pub fn white_texture(&self) -> Handle<Texture> {
        self.white
    }

    /// Whether a splitter owns the pointer, including outside its handle.
    /// Suppress world drags while true, including the release/removal frame.
    /// Release, removal and
    /// [`cancel_interactions`](Self::cancel_interactions) end capture.
    #[must_use]
    pub fn pointer_captured(&self) -> bool {
        self.pointer_consumed || self.splitter_drag.is_some()
    }

    /// Whether a line-edit currently has keyboard focus (the app can use
    /// this to suppress game hotkeys while typing).
    #[must_use]
    pub fn has_focus(&self) -> bool {
        self.focus.is_some() && self.edit.id == self.focus
    }

    /// Whether this frame belongs to the UI keyboard interaction. Query after
    /// `UiFrame::end` before dispatching world actions; includes Tab and Escape
    /// even when those commands remove focus or dismiss a popup.
    #[must_use]
    pub fn keyboard_captured(&self) -> bool {
        self.keyboard_captured || self.focus.is_some() || self.any_popup_open()
    }

    /// Drain a copy/cut request and write it to the platform clipboard. Secret
    /// fields never export their contents. Paste arrives through `UiInput::paste`.
    pub fn take_clipboard_text(&mut self) -> Option<String> {
        self.clipboard_text.take()
    }

    #[cfg(test)]
    fn is_focused_name(&self, name: &str) -> bool {
        self.focus == Some(widget_id(name))
    }

    /// Whether any dropdown popover is open (apps close it on Esc via
    /// [`close_popups`](Self::close_popups)).
    #[must_use]
    pub fn any_popup_open(&self) -> bool {
        self.open_popup.is_some()
    }

    /// Close any open dropdown popover.
    pub fn close_popups(&mut self) {
        self.open_popup = None;
    }

    /// Drop transient pointer interaction (active widget, thumb drags) and
    /// close any open popover. Call on focus loss so a half-finished press
    /// cannot complete later and a dropdown does not linger open while the
    /// window is unfocused.
    pub fn cancel_interactions(&mut self) {
        self.active = None;
        self.thumb_drag = None;
        self.number_drag = None;
        self.splitter_drag = None;
        self.pointer_consumed = false;
        self.open_popup = None;
    }

    /// Drop keyboard focus from any line-edit (R-17/#25: closing a panel
    /// that hosts text fields must release focus, or app hotkeys sharing
    /// letters with typing — e.g. F for fullscreen — stay suppressed).
    pub fn clear_focus(&mut self) {
        self.focus = None;
    }

    /// Whether the mouse at `pos` is over overlay content from last frame
    /// (ordinary widgets there must not react — the popover owns the spot).
    fn blocked_at(&self, pos: Vec2) -> bool {
        self.modal_blocking || self.blocked.iter().any(|r| contains(r, pos))
    }

    /// Start a frame: widgets are drawn through the returned [`UiFrame`]
    /// into `list`'s screen channel (rects immediately, text via `text`'s
    /// queue). Call [`UiFrame::end`] when done, then `text.end_frame(..)`.
    pub fn begin<'a>(
        &'a mut self,
        text: &'a mut TextRenderer,
        list: &'a mut DrawList,
        input: UiInput,
    ) -> UiFrame<'a> {
        self.clock = (self.clock + input.dt) % CARET_BLINK;
        self.pointer_consumed = self.splitter_drag.is_some();
        self.keyboard_captured =
            self.focus.is_some() || self.open_popup.is_some() || self.modal_blocking;
        if input.keys.contains(&UiKey::Tab) {
            let candidates: Vec<_> = self
                .focus_order
                .iter()
                .filter(|target| {
                    if let Some(popup) = self.open_popup {
                        target.popup == Some(popup)
                    } else {
                        target.popup.is_none()
                            && (!self.modal_blocking || target.modal == self.modal_scope)
                    }
                })
                .collect();
            if !candidates.is_empty() {
                let at = candidates
                    .iter()
                    .position(|target| Some(target.id) == self.focus);
                let next = match (at, input.shift) {
                    (Some(i), true) => (i + candidates.len() - 1) % candidates.len(),
                    (Some(i), false) => (i + 1) % candidates.len(),
                    (None, true) => candidates.len() - 1,
                    (None, false) => 0,
                };
                self.focus = Some(candidates[next].id);
                self.keyboard_captured = true;
            }
        }
        let popup_dismissed = input.keys.contains(&UiKey::Escape) && self.open_popup.is_some();
        if popup_dismissed {
            self.focus = self.open_popup.take();
        }
        UiFrame {
            ui: self,
            text,
            list,
            input,
            z: 0.0,
            z_overlay: OVERLAY_Z,
            overlay: false,
            clip: None,
            new_blocked: Vec::new(),
            new_modal: None,
            modal_scope: None,
            edit_clicked: false,
            focus_order: Vec::new(),
            popup_scope: None,
            popup_dismissed,
            key_used: false,
            focus_requested: false,
            splitter_seen: false,
        }
    }
}

/// One frame's widget builder — all draws go to the screen channel with
/// monotonically increasing z, so later widgets cover earlier ones.
/// Overlay content (dropdown popovers, modals) draws in a separate high-z
/// band and blocks the widgets underneath (see [`UiFrame::dropdown`] and
/// [`UiFrame::confirm_modal`]).
pub struct UiFrame<'a> {
    ui: &'a mut Ui,
    text: &'a mut TextRenderer,
    list: &'a mut DrawList,
    input: UiInput,
    z: f32,
    /// z cursor for the overlay band (popovers/modals above everything).
    z_overlay: f32,
    /// Currently emitting overlay content (input bypasses blocking).
    overlay: bool,
    /// Current clip rect applied to widget draws + hit tests.
    clip: Option<Rect>,
    /// Overlay rects drawn this frame → `Ui::blocked` next frame.
    new_blocked: Vec<Rect>,
    /// A modal was drawn this frame → `Ui::modal_blocking` next frame.
    new_modal: Option<u64>,
    modal_scope: Option<u64>,
    /// A line-edit consumed this frame's press (suppresses unfocus).
    edit_clicked: bool,
    focus_order: Vec<FocusTarget>,
    popup_scope: Option<u64>,
    popup_dismissed: bool,
    key_used: bool,
    focus_requested: bool,
    splitter_seen: bool,
}

impl UiFrame<'_> {
    fn next_z(&mut self) -> f32 {
        if self.overlay {
            self.z_overlay += 1.0;
            self.z_overlay
        } else {
            self.z += 1.0;
            self.z
        }
    }

    /// Whether the mouse can interact with an ordinary widget at `rect`:
    /// inside the rect and the current clip, and not underneath overlay
    /// content (unless this widget IS overlay content).
    fn hit(&self, rect: &Rect) -> bool {
        let p = self.input.mouse_pos;
        self.ui.splitter_drag.is_none()
            && contains(rect, p)
            && self.clip.is_none_or(|c| contains(&c, p))
            && (self.overlay || !self.ui.blocked_at(p))
    }

    /// Whether the mouse hovers `rect` (same gating as widget interaction —
    /// custom widget compositions use this for hover states).
    #[must_use]
    pub fn hovered(&self, rect: &Rect) -> bool {
        self.hit(rect)
    }

    /// The frame's pointer position (logical pixels) — custom widget
    /// compositions (tooltips, hover accents).
    #[must_use]
    pub fn mouse_pos(&self) -> Vec2 {
        self.input.mouse_pos
    }

    /// Single-line size of `text` at `px` through the frame's font (layout
    /// helpers: wrapping, right-alignment of composed rows).
    #[must_use]
    pub fn measure_text(&self, text: &str, px: f32) -> Vec2 {
        self.text.measure(text, px)
    }

    /// Set the clip rect applied to subsequent widget draws and hit tests
    /// (`None` = unclipped). Scroll areas manage this automatically.
    pub fn set_clip(&mut self, clip: Option<Rect>) {
        self.clip = clip;
        self.text.set_clip(clip);
    }

    /// A flat colored rectangle (the 1×1 white texture scaled).
    pub fn rect(&mut self, rect: Rect, color: [f32; 4]) {
        let z = self.next_z();
        let size = rect.size();
        self.list.push_screen(SpriteInstance {
            scale: size,
            color,
            z,
            clip: self.clip,
            ..SpriteInstance::new(self.ui.white, rect.min + size * 0.5)
        });
    }

    /// A flat panel in the current theme’s panel color.
    pub fn panel(&mut self, rect: Rect) {
        self.rect(rect, self.ui.theme.panel);
    }

    /// An arbitrary sprite in the UI (screen channel) at this frame's z —
    /// icons/previews layered over widgets (PR-9: the fire/water animated
    /// body previews inside the element toggle buttons). The instance's `z`
    /// is replaced by the frame's draw order; the frame's clip applies
    /// unless the instance carries its own.
    pub fn sprite(&mut self, instance: SpriteInstance) {
        let z = self.next_z();
        let clip = instance.clip.or(self.clip);
        self.list.push_screen(SpriteInstance {
            z,
            clip,
            ..instance
        });
    }

    /// A border ring of `width` px just outside `rect` (four opaque strips —
    /// widget bodies are translucent, so an underlay rect would tint them).
    pub fn border(&mut self, rect: Rect, width: f32, color: [f32; 4]) {
        let (w, size) = (width, rect.size());
        let full_w = size.x + 2.0 * w;
        // Top, bottom, left, right.
        self.rect(Rect::new(rect.min.x - w, rect.min.y - w, full_w, w), color);
        self.rect(Rect::new(rect.min.x - w, rect.max.y, full_w, w), color);
        self.rect(Rect::new(rect.min.x - w, rect.min.y, w, size.y), color);
        self.rect(Rect::new(rect.max.x, rect.min.y, w, size.y), color);
    }

    /// One line of text aligned inside `rect` (a Godot `Label`).
    pub fn label(&mut self, rect: Rect, text: &str, style: &TextStyle, h: HAlign, v: VAlign) {
        let z = self.next_z();
        self.text
            .draw_in_rect(text, rect, h, v, style, z, TextChannel::Screen);
    }

    /// Finish the frame: releases clear the pressed widget; a press that no
    /// line-edit consumed drops focus (click-away unfocus); this frame's
    /// overlay regions become next frame's input blockers.
    pub fn end(self) {
        self.text.set_clip(None);
        if self.input.mouse_released || !self.splitter_seen {
            self.ui.splitter_drag = None;
        }
        if self.input.mouse_released {
            self.ui.active = None;
            self.ui.thumb_drag = None;
            self.ui.number_drag = None;
        }
        if self.input.mouse_pressed && !self.edit_clicked {
            self.ui.focus = None;
        }
        if self.ui.focus.is_none() && self.input.keys.contains(&UiKey::Tab) {
            let target = if self.input.shift {
                self.focus_order.last()
            } else {
                self.focus_order.first()
            };
            if let Some(target) = target {
                self.ui.focus = Some(target.id);
                self.ui.keyboard_captured = true;
            }
        }
        if !self.focus_requested
            && self
                .ui
                .focus
                .is_some_and(|id| !self.focus_order.iter().any(|target| target.id == id))
        {
            self.ui.focus = None;
        }
        if self.ui.edit.id != self.ui.focus {
            self.ui.edit = EditState::default();
        }
        self.ui.focus_order = self.focus_order;
        self.ui.blocked = self.new_blocked;
        self.ui.modal_blocking = self.new_modal.is_some();
        self.ui.modal_scope = self.new_modal;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_ui::{BTN, fixture, press_at};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #255: a press exactly on a rect's far edge misses; one pixel inside
    /// hits.
    #[wasm_bindgen_test(unsupported = test)]
    fn hit_testing_is_half_open() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let mut f = ui.begin(&mut text, &mut list, press_at(110.0, 50.0));
        assert!(!f.button("b", BTN, "B", 20.0), "max edge is outside");
        f.end();
        let mut f = ui.begin(&mut text, &mut list, press_at(109.0, 49.0));
        assert!(f.button("b", BTN, "B", 20.0));
        f.end();
    }

    /// The UI's white pixel is the shared one: a renderer-only consumer
    /// asking `assets` for it afterwards gets the very same handle, so the
    /// UI and an overlay batch against one atlas entry instead of two.
    #[wasm_bindgen_test(unsupported = test)]
    fn ui_shares_the_canonical_white_texture() {
        let mut assets: Assets<Texture> = Assets::new();
        let ui = Ui::new(&mut assets);
        assert_eq!(
            ui.white_texture(),
            crate::assets::white_texture(&mut assets)
        );
    }

    /// `cancel_interactions` (focus loss) drops the active widget, any
    /// thumb drag, and an open popover so a stale press cannot complete
    /// later and no dropdown lingers open while the window is unfocused.
    #[wasm_bindgen_test(unsupported = test)]
    fn cancel_interactions_clears_pointer_state() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();

        let mut f = ui.begin(&mut text, &mut list, press_at(50.0, 30.0));
        f.button("b", BTN, "OK", 24.0);
        f.end();
        assert!(ui.active.is_some());
        ui.cancel_interactions();
        assert!(ui.active.is_none());
        assert!(ui.thumb_drag.is_none());

        // An open dropdown popover closes too (focus loss must not leave
        // a floating popover behind).
        let dd = Rect::new(10.0, 10.0, 160.0, 30.0);
        let mut selected = 0usize;
        let mut f = ui.begin(&mut text, &mut list, press_at(50.0, 25.0));
        f.dropdown("zoom", dd, &["a", "b"], &mut selected, 16.0);
        f.end();
        assert!(ui.any_popup_open());
        ui.cancel_interactions();
        assert!(!ui.any_popup_open(), "focus loss closes the popover");
    }

    /// Panels and buttons emit screen-channel quads with increasing z, so
    /// later widgets draw over earlier ones.
    #[wasm_bindgen_test(unsupported = test)]
    fn draws_go_to_screen_channel_with_increasing_z() {
        let (mut ui, mut text, mut assets) = fixture();
        let mut list = DrawList::new();

        let mut f = ui.begin(&mut text, &mut list, UiInput::default());
        f.panel(Rect::new(40.0, 340.0, 880.0, 160.0));
        f.button("start", Rect::new(81.0, 380.0, 300.0, 80.0), "START", 48.0);
        f.end();
        text.end_frame(&mut assets, &mut list);

        assert!(list.world.is_empty());
        assert!(list.screen.len() > 2, "panel + button + glyph quads");
        // Panel first, button body second, strictly increasing z.
        assert!(list.screen[0].z < list.screen[1].z);
        // Button text z is above the button body z.
        let body_z = list.screen[1].z;
        assert!(list.screen[2..].iter().all(|i| i.z > body_z));
    }
}
