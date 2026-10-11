//! Layout widgets: splitters that resize caller-owned extents, and clipped
//! scroll areas.

use super::{UiFrame, widget_id};
use crate::canvas::draw::Rect;

/// Scroll-wheel lines → content pixels (one "line" of a list).
const SCROLL_LINE_PX: f32 = 40.0;
/// Scrollbar track width in logical pixels.
const SCROLLBAR_W: f32 = 8.0;

/// Axis along which a splitter changes its caller-owned extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitterAxis {
    /// Left/right movement changes width.
    Horizontal,
    /// Up/down movement changes height.
    Vertical,
}

/// Cursor intent; the game maps this to its platform cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCursor {
    /// Left/right resize.
    ResizeHorizontal,
    /// Up/down resize.
    ResizeVertical,
}

/// Caller-owned splitter configuration. When `max < min` (a viewport too small
/// for both panes), `max` wins; a NaN bound is ignored.
#[derive(Debug, Clone, Copy)]
pub struct Splitter {
    pub axis: SplitterAxis,
    pub min: f32,
    pub max: f32,
}

/// Splitter result for this frame. Cursor intent persists outside the handle
/// during capture; apply it before dispatching pointer input to the world.
#[derive(Debug, Clone, Copy)]
pub struct SplitterResponse {
    pub changed: bool,
    pub dragging: bool,
    pub cursor: Option<UiCursor>,
}

/// An open scroll area, from its `scroll_area_begin` to its `scroll_area_end`.
#[derive(Debug, Clone, Copy)]
pub(super) struct ScrollScope {
    /// The clip its end restores.
    outer_clip: Option<Rect>,
    max_off: f32,
    /// Hovered with content to scroll: may take this frame's wheel at its end.
    wheel: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SplitterDrag {
    id: u64,
    start: f32,
    extent: f32,
}

impl UiFrame<'_> {
    /// Capture a press on `handle` and resize `extent` by pointer travel.
    /// Composition and drawing remain caller-owned. Submit once each frame;
    /// removal ends capture at frame end. Dragging continues across panes and
    /// outside the handle/clip until release or `Ui::cancel_interactions`.
    pub fn splitter(
        &mut self,
        name: &str,
        handle: Rect,
        extent: &mut f32,
        options: Splitter,
    ) -> SplitterResponse {
        let id = widget_id(name);
        let hover = self.hit(&handle);
        let position = match options.axis {
            SplitterAxis::Horizontal => self.input.mouse_pos.x,
            SplitterAxis::Vertical => self.input.mouse_pos.y,
        };
        if hover && self.input.mouse_pressed {
            self.ui.pointer_consumed = true;
            self.ui.splitter_drag = Some(SplitterDrag {
                id,
                start: position,
                extent: *extent,
            });
            self.ui.active = Some(id);
        }
        let mut changed = false;
        let captured = self.ui.splitter_drag.filter(|drag| drag.id == id);
        if let Some(drag) = captured {
            self.splitter_seen = true;
            if self.input.mouse_down || self.input.mouse_released {
                // Not `f32::clamp`: it panics on `min > max` or a NaN bound.
                let next = (drag.extent + position - drag.start)
                    .max(options.min)
                    .min(options.max);
                changed = *extent != next;
                *extent = next;
            }
        }
        let dragging = captured.is_some() && !self.input.mouse_released;
        SplitterResponse {
            changed,
            dragging,
            cursor: (hover || dragging).then_some(match options.axis {
                SplitterAxis::Horizontal => UiCursor::ResizeHorizontal,
                SplitterAxis::Vertical => UiCursor::ResizeVertical,
            }),
        }
    }

    /// Begin a vertically scrollable, clipped region: `content_h` is the
    /// total content height laid out from `rect.min.y - *offset`. A draggable
    /// scrollbar appears when content overflows; `*offset` is clamped to the
    /// valid range. Draws after this clip to `rect` within the enclosing clip
    /// (including text) until [`scroll_area_end`](Self::scroll_area_end);
    /// scroll areas nest. The wheel scrolls the innermost hovered area with
    /// content to scroll, applied at its end, so the content shows it from the
    /// next frame.
    pub fn scroll_area_begin(&mut self, name: &str, rect: Rect, content_h: f32, offset: &mut f32) {
        let id = widget_id(name);
        let view_h = rect.size().y;
        let max_off = (content_h - view_h).max(0.0);
        *offset = offset.clamp(0.0, max_off);
        let wheel = max_off > 0.0 && self.hit(&rect);

        if max_off > 0.0 {
            let track = Rect::new(rect.max.x - SCROLLBAR_W, rect.min.y, SCROLLBAR_W, view_h);
            let thumb_h = (view_h * view_h / content_h).clamp(20.0_f32.min(view_h), view_h);
            let travel = view_h - thumb_h;
            let thumb_at = |off: f32| {
                Rect::new(
                    track.min.x,
                    rect.min.y + (off / max_off) * travel,
                    SCROLLBAR_W,
                    thumb_h,
                )
            };

            // Thumb drag: grab on press, track the pointer while held.
            if self.input.mouse_pressed && self.hit(&thumb_at(*offset)) {
                self.ui.active = Some(id);
                self.ui.thumb_drag = Some((id, self.input.mouse_pos.y - thumb_at(*offset).min.y));
            }
            if let Some((drag_id, grab)) = self.ui.thumb_drag
                && drag_id == id
            {
                if self.input.mouse_down && travel > 0.0 {
                    let t = ((self.input.mouse_pos.y - grab) - rect.min.y) / travel;
                    *offset = t.clamp(0.0, 1.0) * max_off;
                } else {
                    self.ui.thumb_drag = None;
                }
            }

            let thumb = thumb_at(*offset);
            let dragging = matches!(self.ui.thumb_drag, Some((d, _)) if d == id);
            let color = if dragging || self.hit(&thumb) {
                self.ui.theme.scroll_thumb_hovered
            } else {
                self.ui.theme.scroll_thumb
            };
            self.rect(track, self.ui.theme.scroll_track);
            self.rect(thumb, color);
        }

        self.scroll_scopes.push(ScrollScope {
            outer_clip: self.clip,
            max_off,
            wheel,
        });
        self.set_clip(Some(self.clip_within(rect)));
    }

    /// End the innermost scroll area, restoring the clip in effect at its
    /// [`scroll_area_begin`](Self::scroll_area_begin). Pass the same `offset`:
    /// inner areas end first, so the innermost hovered area with content to
    /// scroll takes this frame's wheel and enclosing areas leave it alone.
    pub fn scroll_area_end(&mut self, offset: &mut f32) {
        let Some(scope) = self.scroll_scopes.pop() else {
            self.set_clip(None);
            return;
        };
        if scope.wheel && !self.wheel_consumed {
            self.wheel_consumed = true;
            *offset = (*offset - self.input.scroll.y * SCROLL_LINE_PX).clamp(0.0, scope.max_off);
        }
        self.set_clip(scope.outer_clip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::draw::DrawList;
    use crate::ui::test_ui::{fixture, hover_at, press_at};
    use crate::ui::{UiInput, UiKey};
    use sgl_core::math::Vec2;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test(unsupported = test)]
    fn splitter_capture_crosses_panes_and_cancels() {
        let (mut ui, mut text, _assets) = fixture();
        let mut extent = 100.0;
        let options = Splitter {
            axis: SplitterAxis::Horizontal,
            min: 60.0,
            max: 180.0,
        };
        for (x, press, release, expected) in [
            (102.0, true, false, 100.0),
            (142.0, false, false, 140.0),
            (300.0, false, false, 180.0),
            (-50.0, false, true, 60.0),
        ] {
            let mut list = DrawList::new();
            let mut f = ui.begin(
                &mut text,
                &mut list,
                UiInput {
                    mouse_pos: Vec2::new(x, 15.0),
                    mouse_pressed: press,
                    mouse_released: release,
                    mouse_down: !release,
                    ..UiInput::default()
                },
            );
            let result = f.splitter(
                "split",
                Rect::new(extent, 0.0, 5.0, 100.0),
                &mut extent,
                options,
            );
            assert!(!f.button("pane", Rect::new(-100.0, 0.0, 500.0, 100.0), "Pane", 12.0));
            assert_eq!(extent, expected);
            assert_eq!(result.dragging, !release);
            f.end();
            assert!(
                ui.pointer_captured(),
                "release remains consumed for world dispatch"
            );
        }
        for remove in [false, true] {
            let mut list = DrawList::new();
            let mut f = ui.begin(
                &mut text,
                &mut list,
                UiInput {
                    mouse_pos: Vec2::new(62.0, 15.0),
                    mouse_pressed: true,
                    mouse_down: true,
                    ..UiInput::default()
                },
            );
            f.splitter(
                "split",
                Rect::new(60.0, 0.0, 5.0, 100.0),
                &mut extent,
                options,
            );
            f.end();
            assert!(ui.pointer_captured());
            if remove {
                ui.begin(&mut text, &mut list, UiInput::default()).end();
            } else {
                ui.cancel_interactions();
            }
            assert_eq!(ui.pointer_captured(), remove);
            let mut f = ui.begin(
                &mut text,
                &mut list,
                UiInput {
                    mouse_pos: Vec2::new(170.0, 15.0),
                    mouse_down: true,
                    ..UiInput::default()
                },
            );
            assert!(
                !f.splitter(
                    "split",
                    Rect::new(60.0, 0.0, 5.0, 100.0),
                    &mut extent,
                    options
                )
                .changed
            );
            f.end();
            assert_eq!(extent, 60.0);
        }
    }

    /// #318: a viewport-derived `max` below `min`, or a NaN bound, must not
    /// panic mid-drag; `max` wins and a NaN bound is ignored.
    #[wasm_bindgen_test(unsupported = test)]
    fn splitter_drag_tolerates_inverted_and_nan_bounds() {
        for (min, max, drag_to, expected) in [
            (220.0, 180.0, 400.0, 180.0),
            (220.0, 180.0, 0.0, 180.0),
            (f32::NAN, 180.0, 0.0, 0.0),
            (f32::NAN, 180.0, 400.0, 180.0),
            (60.0, f32::NAN, 400.0, 400.0),
            (60.0, f32::NAN, 0.0, 60.0),
        ] {
            let (mut ui, mut text, _assets) = fixture();
            let mut extent = 100.0;
            let options = Splitter {
                axis: SplitterAxis::Horizontal,
                min,
                max,
            };
            for input in [press_at(102.0, 15.0), hover_at(drag_to + 2.0, 15.0, true)] {
                let mut list = DrawList::new();
                let mut f = ui.begin(&mut text, &mut list, input);
                f.splitter(
                    "split",
                    Rect::new(100.0, 0.0, 5.0, 100.0),
                    &mut extent,
                    options,
                );
                f.end();
            }
            assert_eq!(extent, expected, "min {min}, max {max}, drag to {drag_to}");
        }
    }

    /// #255: the drawn geometry the games rely on, against hand-computed
    /// rects — the scrollbar track and thumb for a known offset, the thumb
    /// drag mapping, and the tooltip's placement and clamping.
    #[wasm_bindgen_test(unsupported = test)]
    fn scrollbar_thumb_and_tooltip_geometry_are_exact() {
        let (mut ui, mut text, _assets) = fixture();
        let area = Rect::new(100.0, 100.0, 200.0, 100.0);
        let content_h = 400.0; // max offset 300, thumb 25 px, travel 75 px
        let mut offset = 150.0;
        let mut list = DrawList::new();
        let mut f = ui.begin(&mut text, &mut list, hover_at(0.0, 0.0, false));
        f.scroll_area_begin("s", area, content_h, &mut offset);
        f.scroll_area_end(&mut offset);
        f.end();
        let rects: Vec<(Vec2, Vec2)> = list.screen.iter().map(|q| (q.pos, q.scale)).collect();
        let track = (Vec2::new(296.0, 150.0), Vec2::new(8.0, 100.0));
        let thumb = (Vec2::new(296.0, 100.0 + 37.5 + 12.5), Vec2::new(8.0, 25.0));
        assert!(rects.contains(&track), "track missing in {rects:?}");
        assert!(rects.contains(&thumb), "thumb missing in {rects:?}");

        // Grab the thumb at its center and drag to the bottom of the track:
        // the offset follows the thumb's top through the travel.
        let mut list = DrawList::new();
        let mut f = ui.begin(&mut text, &mut list, press_at(296.0, 150.0));
        f.scroll_area_begin("s", area, content_h, &mut offset);
        f.scroll_area_end(&mut offset);
        f.end();
        let mut list = DrawList::new();
        let mut f = ui.begin(&mut text, &mut list, hover_at(296.0, 300.0, true));
        f.scroll_area_begin("s", area, content_h, &mut offset);
        f.scroll_area_end(&mut offset);
        f.end();
        assert_eq!(
            offset, 300.0,
            "dragged past the end clamps to the max offset"
        );

        // Tooltip: below the anchor with 6 px padding, clamped inside bounds.
        let bounds = Rect::new(0.0, 0.0, 400.0, 300.0);
        let anchor = Rect::new(350.0, 250.0, 40.0, 20.0);
        let mut list = DrawList::new();
        let mut f = ui.begin(&mut text, &mut list, hover_at(360.0, 260.0, false));
        f.tooltip(anchor, bounds, "tip", 16.0);
        let size = f.measure_text("tip", 16.0);
        f.end();
        let tip_w = size.x + 12.0;
        let tip_h = size.y + 6.0;
        let tip_x = (350.0f32).min(400.0 - tip_w - 2.0);
        let tip_y = 250.0 - tip_h - 4.0; // no room below: flipped above
        let backdrop = (
            Vec2::new(tip_x + tip_w * 0.5, tip_y + tip_h * 0.5),
            Vec2::new(tip_w, tip_h),
        );
        let close = |a: Vec2, b: Vec2| (a - b).abs().max_element() < 1e-3;
        let found = list
            .screen
            .iter()
            .any(|q| close(q.pos, backdrop.0) && close(q.scale, backdrop.1));
        let rects: Vec<(Vec2, Vec2)> = list.screen.iter().map(|q| (q.pos, q.scale)).collect();
        assert!(found, "tooltip backdrop {backdrop:?} missing in {rects:?}");
    }

    /// #322: a scroll area inside a clipped panel (and one nested inside it)
    /// clips to the intersection, ignores presses outside it, and restores
    /// each enclosing clip at its end.
    #[wasm_bindgen_test(unsupported = test)]
    fn nested_scroll_areas_intersect_and_restore_the_enclosing_clip() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let panel = Rect::new(0.0, 0.0, 300.0, 200.0);
        let outer = Rect::new(100.0, 100.0, 300.0, 300.0);
        let inner = Rect::new(150.0, 50.0, 100.0, 100.0);
        let (mut outer_offset, mut inner_offset) = (0.0, 0.0);
        let mut f = ui.begin(&mut text, &mut list, press_at(350.0, 150.0));
        f.set_clip(Some(panel));
        f.scroll_area_begin("outer", outer, 300.0, &mut outer_offset);
        assert!(
            !f.button("hidden", Rect::new(320.0, 120.0, 60.0, 60.0), "", 12.0),
            "a press outside the panel reached scrolled content"
        );
        f.scroll_area_begin("inner", inner, 100.0, &mut inner_offset);
        f.rect(Rect::new(150.0, 50.0, 10.0, 10.0), [1.0; 4]);
        f.scroll_area_end(&mut inner_offset);
        f.rect(Rect::new(100.0, 100.0, 10.0, 10.0), [1.0; 4]);
        f.scroll_area_end(&mut outer_offset);
        f.rect(Rect::new(0.0, 0.0, 10.0, 10.0), [1.0; 4]);
        f.end();
        let clips: Vec<_> = list.screen.iter().rev().take(3).map(|q| q.clip).collect();
        assert_eq!(
            clips,
            vec![
                Some(panel),
                Some(Rect::new(100.0, 100.0, 200.0, 100.0)),
                Some(Rect::new(150.0, 100.0, 100.0, 50.0)),
            ]
        );
    }

    /// #431: one wheel tick scrolls only the innermost hovered area that has
    /// content to scroll; an inner area with nothing to scroll passes it out.
    #[wasm_bindgen_test(unsupported = test)]
    fn wheel_scrolls_only_the_innermost_hovered_scroll_area() {
        let outer = Rect::new(0.0, 0.0, 200.0, 200.0);
        let inner = Rect::new(10.0, 10.0, 100.0, 100.0);
        for (pointer, inner_content, expected) in [
            (Vec2::new(50.0, 50.0), 1000.0, (0.0, 40.0)),
            (Vec2::new(150.0, 150.0), 1000.0, (40.0, 0.0)),
            (Vec2::new(50.0, 50.0), 50.0, (40.0, 0.0)),
        ] {
            let (mut ui, mut text, _assets) = fixture();
            let mut list = DrawList::new();
            let (mut outer_offset, mut inner_offset) = (0.0, 0.0);
            let mut f = ui.begin(
                &mut text,
                &mut list,
                UiInput {
                    mouse_pos: pointer,
                    scroll: Vec2::new(0.0, -1.0),
                    ..UiInput::default()
                },
            );
            f.scroll_area_begin("outer", outer, 1000.0, &mut outer_offset);
            f.scroll_area_begin("inner", inner, inner_content, &mut inner_offset);
            f.scroll_area_end(&mut inner_offset);
            f.scroll_area_end(&mut outer_offset);
            f.end();
            assert_eq!(
                (outer_offset, inner_offset),
                expected,
                "pointer {pointer:?}, inner content {inner_content}"
            );
        }
    }

    /// A scroll area entirely outside its enclosing clip clips to a zero
    /// rect at the origin; a control straddling the origin there is hidden,
    /// so Tab never focuses it.
    #[wasm_bindgen_test(unsupported = test)]
    fn fully_clipped_scroll_area_hides_controls_from_focus() {
        let (mut ui, mut text, _assets) = fixture();
        for keys in [vec![], vec![UiKey::Tab]] {
            let mut list = DrawList::new();
            let mut offset = 0.0;
            let mut f = ui.begin(
                &mut text,
                &mut list,
                UiInput {
                    keys,
                    ..UiInput::default()
                },
            );
            f.set_clip(Some(Rect::new(0.0, 0.0, 100.0, 100.0)));
            f.scroll_area_begin("s", Rect::new(200.0, 200.0, 50.0, 50.0), 50.0, &mut offset);
            f.button("hidden", Rect::new(-5.0, -5.0, 10.0, 10.0), "X", 12.0);
            f.scroll_area_end(&mut offset);
            f.end();
        }
        assert!(!ui.is_focused_name("hidden"));
    }

    /// Scroll area: wheel scrolls only while hovered, the offset clamps to
    /// the content range, and content drawn inside carries the clip rect.
    #[wasm_bindgen_test(unsupported = test)]
    fn scroll_area_scrolls_clamps_and_clips() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let area = Rect::new(700.0, 60.0, 200.0, 300.0);
        let mut offset = 0.0_f32;

        // Wheel down (negative y) while hovered scrolls the content down.
        let input = UiInput {
            mouse_pos: Vec2::new(750.0, 200.0),
            scroll: Vec2::new(0.0, -2.0),
            ..UiInput::default()
        };
        let mut f = ui.begin(&mut text, &mut list, input);
        f.scroll_area_begin("palette", area, 900.0, &mut offset);
        f.rect(Rect::new(710.0, 70.0, 100.0, 40.0), [1.0; 4]);
        f.scroll_area_end(&mut offset);
        f.rect(Rect::new(0.0, 0.0, 10.0, 10.0), [1.0; 4]);
        f.end();
        assert!(offset > 0.0, "wheel-down scrolled: {offset}");
        let n = list.screen.len();
        assert_eq!(
            list.screen[n - 2].clip,
            Some(area),
            "content carries the clip"
        );
        assert_eq!(list.screen[n - 1].clip, None, "clip resets after end");

        // Wheel while NOT hovered does nothing.
        let before = offset;
        let input = UiInput {
            mouse_pos: Vec2::new(100.0, 100.0),
            scroll: Vec2::new(0.0, -2.0),
            ..UiInput::default()
        };
        let mut f = ui.begin(&mut text, &mut list, input);
        f.scroll_area_begin("palette", area, 900.0, &mut offset);
        f.scroll_area_end(&mut offset);
        f.end();
        assert_eq!(offset, before, "unhovered wheel must not scroll");

        // The offset clamps to [0, content_h - view_h].
        offset = 1e6;
        let mut f = ui.begin(&mut text, &mut list, UiInput::default());
        f.scroll_area_begin("palette", area, 900.0, &mut offset);
        f.scroll_area_end(&mut offset);
        f.end();
        assert_eq!(offset, 600.0, "clamped to content_h - view_h");
        offset = -50.0;
        let mut f = ui.begin(&mut text, &mut list, UiInput::default());
        f.scroll_area_begin("palette", area, 900.0, &mut offset);
        f.scroll_area_end(&mut offset);
        f.end();
        assert_eq!(offset, 0.0);

        // Content shorter than the view: no scrolling, no scrollbar thumb.
        offset = 10.0;
        let mut f = ui.begin(&mut text, &mut list, UiInput::default());
        f.scroll_area_begin("palette", area, 100.0, &mut offset);
        f.scroll_area_end(&mut offset);
        f.end();
        assert_eq!(offset, 0.0, "short content pins to the top");
    }

    /// Widgets clipped by a scroll area only respond where visible: a press
    /// inside the widget rect but outside the clip is ignored.
    #[wasm_bindgen_test(unsupported = test)]
    fn clipped_widgets_ignore_presses_outside_the_clip() {
        let (mut ui, mut text, _assets) = fixture();
        let mut list = DrawList::new();
        let area = Rect::new(0.0, 0.0, 200.0, 100.0);
        let mut offset = 0.0_f32;

        // The button extends below the clip (y 80..160); press at y=120.
        let mut f = ui.begin(&mut text, &mut list, press_at(50.0, 120.0));
        f.scroll_area_begin("list", area, 400.0, &mut offset);
        let fired = f.button("item", Rect::new(10.0, 80.0, 150.0, 80.0), "I", 16.0);
        f.scroll_area_end(&mut offset);
        f.end();
        assert!(!fired, "press outside the clip must not fire");

        // The same press inside the visible part fires.
        let mut f = ui.begin(&mut text, &mut list, press_at(50.0, 90.0));
        f.scroll_area_begin("list", area, 400.0, &mut offset);
        let fired = f.button("item", Rect::new(10.0, 80.0, 150.0, 80.0), "I", 16.0);
        f.scroll_area_end(&mut offset);
        f.end();
        assert!(fired, "visible part still fires");
    }
}
