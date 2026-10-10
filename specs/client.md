# 2D client building blocks

The CPU-side, GPU-free features of `sgl-2d` that games build on: assets,
the draw-list seam, camera, text, overlay primitives, immediate UI, and the
Aseprite loader. The GPU pipeline itself is in [rendering](rendering.md).

## Requirements

1. **Assets.** `AssetServer` loads the same logical path from a native
   directory (`with_root`) or from caller-supplied bytes (`from_bundle`) and
   returns the same decoded value. `Texture` is straight-alpha RGBA8 on the
   CPU; upload is the renderer's job. Texture, normal-map and light-cookie
   uploads refuse an empty texture, one larger than the device allows, one
   whose RGBA length mismatches its size, and a normal map sized unlike its
   diffuse with a `TextureError`, changing nothing. `Assets` never removes
   an asset, so a `Handle<T>` stays valid for its cache's lifetime, and
   re-inserting a path replaces the asset under the same handle. Handles from
   different caches can share an index, and a renderer keys its textures by
   handle, so each `Renderer` draws from one texture cache (for example
   `AssetServer::textures`, which `Ui::new`, `TextRenderer::end_frame` and
   `white_texture` then also take). Loading is synchronous.
   `white_texture` registers one shared 1×1 white pixel under `sgl://white`.
2. **Draw list.** `DrawList` is the only channel from game code to the
   renderer: a `world` channel (through the camera) and a `screen` channel
   (fixed logical space), both `SpriteInstance`s addressed by texture handle
   and pixel source rect. Each channel is stable-sorted by ascending `z`, so
   equal-`z` sprites keep push order; a NaN `z` draws last. `push_tiled`
   covers the target rect with repeated source tiles including a partial last
   tile; `push_nine_slice` keeps corners at native size and stretches edges
   and center. Both take a `WorldUnits` (the world camera's on the world
   channel; the screen variants use logical pixels): the target and tile
   sizes are in those units, the grid's y direction follows `y_up` (the
   source's top row lands at the larger world y), the 9-slice border stays in
   source pixels, and the default units are bit-identical to the pixel
   layout. Expansions are bounded by `MAX_TILED_QUADS`.
3. **Camera.** `Camera` is a world-space center plus zoom over a logical view;
   the default convention is y-down logical pixels and clockwise rotation.
   `with_units` switches the world channel to pixels-per-unit and optional
   y-up; the pixel convention is bit-identical to a camera without units.
   `screen_to_world` and `world_to_screen` are inverses through the letterbox.
4. **Letterbox.** `fit_fractional` returns the largest aspect-preserving,
   centered rect of the logical size inside the window.
5. **Text.** `TextRenderer` rasterizes TTF glyphs on demand into CPU atlas
   pages and emits sprite quads on either channel; `measure` agrees with the
   quads `draw` emits for the same text and size. Each page is one texture
   asset under a stable handle; `end_frame` republishes changed pages in
   place and returns their handles for the game to upload, which replaces
   their pixels in the existing renderer or sprite pass. Outline and
   shadow are offset copies of the glyph quads. `pixel_scale = 1` is
   bit-identical to unscaled rendering.
6. **Overlay.** Lines, rect outlines, fills, and circles are emitted as quads
   on the shared white texture into whichever channel the caller passes, with
   the overlay's color, `z`, and clip. Positions, sizes and widths are in the
   overlay's `WorldUnits` (logical pixels by default; the world camera's
   units for world gizmos); pixel snapping and the one-pixel minimum width
   apply in world pixels. The default is bit-identical to the pixel layout.
7. **UI.** Immediate mode: the game rebuilds widgets every frame and supplies
   `UiInput` (logical-pixel mouse, edges, chars, editing/navigation keys,
   app-supplied clipboard paste, scroll, `dt`); the
   only retained state is interaction (active widget, focus, selection/caret,
   horizontal edit scroll, numeric draft, popups, blink). Tab/Shift-Tab wrap
   through visible widgets in the preceding frame's submission order; fully
   clipped and removed controls cannot receive keyboard input. Open dropdowns
   and modals restrict keyboard interaction to their contents; an open
   dropdown not submitted in a frame closes at its end; a dropdown's
   popover stays in the current clip, and only its visible part blocks
   widgets beneath it. Buttons, toggles, checkboxes and dropdowns show an
   accent focus border and activate with Enter/Space. Escape dismisses the topmost dropdown or cancels the modal.
   The app translates platform shortcuts, supplies clipboard paste, drains
   `take_clipboard_text` for copy/cut, and checks `keyboard_captured` after the
   frame before dispatching world shortcuts (including the dismissal frame).
   Navigation/dismissal precedes activation, followed by ordered editing keys,
   backspace, characters and paste; the app coalesces traversal/activation to
   one edge per frame. `has_focus` retains its text-input-only meaning.
   Normal line edits support Left/Right, Home/End, Shift selection, select-all,
   Delete/Backspace, copy/cut, and replacement by typing/paste. Caret and draft
   survive unrelated redraws under stable widget names. Edits scroll to keep
   the caret visible and clip content to the field and enclosing clip. Secret
   fields retain ASCII append/backspace/paste editing and never copy or cut;
   they avoid temporary copies of the secret while editing, provided the
   game preallocates `buf` with at least `max_len` bytes.
   Buttons act on press-down. At most one widget is active at a time. An open
   modal blocks widgets behind it. Line edits respect `max_len` and keep the
   buffer valid UTF-8; `password_edit_clear` zeroizes the cleared text in
   place and keeps the buffer's allocation. Scroll areas clip their content
   and clamp their offset. A checkbox and a
   collapsing header flip their caller-owned flag on press-down over the whole
   rect. A numeric field's `-`/`+` ends add its step; a horizontal drag on the
   middle past a small threshold scrubs the value by its speed per pixel from
   the press point; a press released inside the threshold opens the middle for
   typing, and the typed text is re-parsed whenever it changes — a parse that succeeds
   stores the number and one that fails leaves the value. Every stored value
   is clamped into the field's range. Widgets emit only to the screen channel.
   A splitter captures pointer travel along its configured axis, changes a
   caller-owned extent within supplied bounds, and returns resize cursor intent.
   When `max` falls below `min` (a viewport too small for both panes), `max`
   wins; a NaN bound is ignored. Neither panics.
   Capture continues outside the handle and across panes, blocks other pointer
   controls, and ends on release, removal or `cancel_interactions` (window focus
   loss). The game checks `pointer_captured` before world pointer dispatch.
   Compact `icon_button` hit rectangles are independent of glyph size; selected
   state has an inset marker, alongside hover, pressed, focus and disabled
   visuals. Disabled buttons remain focusable for explanation but never act.
   `tooltip_for` uses a stable control name for hover or keyboard-focus help,
   respects popup/modal scope and anchor clipping/viewport visibility, wraps to
   viewport width and clips excess height. Games own tooltip text and placement
   bounds, layout, icons and platform cursor mapping.
8. **Aseprite.** `AseSheet::parse` accepts Hash and Array exports; Hash frame
   indices are recovered from trailing digits and must be exactly
   `0..len` (gap, duplicate, or missing digits is an error). Tags are
   inclusive ranges validated against the frame count; `direction` defaults
   to `forward` and expands `forward`, `reverse`, `pingpong`, and
   `pingpong_reverse` into frame orders. A frame rect outside the sheet is an
   error. Only `frame` and `duration` are read; trim data is ignored.

## Acceptance

- The same PNG and Aseprite JSON loaded via a root directory and via a bundle
  produce equal `Texture` and sheet values.
- Malformed loader input returns an error and never panics.
- A `UiInput` sequence drives a widget tree deterministically without a
  window, GPU, or clock.

## Tool composition

For games and coding agents building native editors, start with the runnable
[`tool_ui` example](../crates/sgl-2d/examples/tool_ui.rs):
`cargo run -p sgl-2d --example tool_ui`. SGL supplies widgets; the game owns
composition and editor state. `Ui::set_theme(UiTheme::tools_dark())` or
`tools_light()` gives opaque chrome over scene artwork. `Default` retains the
original translucent game appearance. Copy `*ui.theme()` for custom labels and
rectangles so their text, accent and surface roles follow the selected palette.

- **Coordinates and density.** Recompute bounds from the current logical
  viewport in points. Convert physical pointer coordinates through the same
  DPI/letterbox mapping once, and set text raster scale accordingly. Do not
  assume 960×540. Compact mouse/keyboard tools can use small glyphs inside larger
  hit rectangles; preserve readable type, focus borders and gaps between actions.
- **Bounded panes and rows.** Store pane extents in the game and constrain
  `splitter` bounds to leave usable space for both sides. Clamp again when the
  viewport shrinks (with `.max(min).min(max)`, not `f32::clamp`, which panics
  when the bounds cross). Measure content with `TextRenderer::measure`; reserve
  fixed trailing visibility/lock/delete actions before sizing a flexible row
  label.
  Reflow sections or wrap help when space runs out; do not solve overflow by
  shrinking all text or stacking every action into a full-width button.
- **Overflow ownership.** Give each overflowing pane one
  `scroll_area_begin`/`scroll_area_end` pair and its own game-owned offset.
  Compute content height from the laid-out rows; keep headers and toolbars
  outside its scrolling content. A scroll area clips within the enclosing
  clip and restores it at `scroll_area_end`; restore enclosing clips yourself
  after custom clipping. Widget borders extend outside their hit rectangles:
  inset content from viewport edges and reserve gaps for focus/selection strokes
  and the scrollbar. Clip long row names to their allocated space; provide
  their full meaning in focus/hover help instead of letting them cover actions.
- **Discoverability and state.** Use icons for familiar repeated row actions,
  text for unfamiliar or consequential commands. Pair `icon_button` and
  `tooltip_for` with the same stable name; explain disabled reasons and any
  game-implemented shortcut. Keyboard focus reveals tooltips too. Retain
  selection marks, checkbox geometry and focus outlines so color is not the
  only indication. A compact interface must remain keyboard navigable.
- **Input ownership.** Submit controls in useful Tab order with stable names.
  Translate platform editing/clipboard commands into `UiInput`, drain copy/cut
  output, and consult `keyboard_captured` after `end` before world shortcuts.
  Popup/modal focus stays inside its scope, including Escape dismissal.
  Consult `pointer_captured` before world drags, apply splitter cursor intent,
  and call `cancel_interactions` on window focus loss.
- **Game-owned decisions.** The game owns persistence of theme, pane extents,
  selection, documents and undo. It also decides whether an operation warrants
  confirmation. Use explicit consequential labels such as “Delete layer” and
  “Cancel”, and state what is affected; avoid an ambiguous “OK”.

The example exercises reusable SGL behavior and composition. Its dimensions
are a desktop layout example, not universal minimums; adapt them to the game's
viewport and input needs.

Verified adapter lessons for coding agents:

- Preserve pointer edge positions and native key ordering when multiple events
  arrive between redraws. A single overwritten mouse position can lose a fast
  splitter press before SGL sees it. Queue snapshots and consume them in order.
- Use platform modifier-change notifications as well as key events. Snapshot
  modifiers per event; native macOS Cmd shortcuts may not arrive with a separate
  physical modifier press. Deliver key releases even while a control captures
  input so camera movement cannot remain held.
- Distinguish active text editing (`has_focus`) from broader control ownership
  (`keyboard_captured`). Normal text editing suppresses object commands; a game
  may deliberately keep Cmd/Ctrl undo/copy shortcuts on non-text controls while
  respecting popup/modal ownership. Dispatch only after the UI frame settles.
- Keep room and shared-library save scopes explicit. A failed save must retain
  the draft and error until recovery; a partial document loaded with missing
  assets must remain unsavable after discard. These are game document contracts,
  not reusable widget state.

Validate the consuming tool's authoring, save/reload, dirty-exit, clipboard,
and focus behavior on its supported platforms. Widget tests do not establish
that a game's platform adapter or persistence workflow works.
