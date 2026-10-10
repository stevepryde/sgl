//! The game→renderer seam (AR-6): [`SpriteInstance`] + [`DrawList`].
//!
//! `DrawList` is the **only** channel from game code into the renderer: the
//! game (`game::view`) fills it each frame; `Renderer::render` consumes it.
//! Sprites are addressed by asset [`Handle`] + pixel source rect — atlas
//! placement is a renderer implementation detail the game never sees.
//!
//! Two channels (both the same instance type):
//! - **world**: drawn through the world camera (position/zoom apply);
//! - **screen**: drawn through the fixed screen camera over the logical
//!   960×540 space (UI/HUD later; identity placement).
//!
//! Draw order (PR-2): each channel is stable-sorted by ascending `z`, so
//! equal-`z` sprites keep their push order ("tree order" parity with Godot).
//! A NaN `z` draws last.
//!
//! Two CPU expansions turn one template instance into the quads that cover a
//! target rect, so the GPU path stays a dumb textured-quad batcher:
//! [`expand_tiled`] (repeat a source rect across the target) and
//! [`expand_nine_slice`] (native corners, stretched edges and center). Both
//! append to a plain `Vec<SpriteInstance>` so either channel can use them;
//! [`DrawList::push_tiled`] and friends are the thin conveniences. The
//! expansions lay their grid out in a [`WorldUnits`] convention — the world
//! camera's for the world channel, `WorldUnits::default()` (logical pixels,
//! y-down) for the screen channel — so a world-unit, y-up game pushes unit
//! sizes straight in and never remaps the emitted quads (#262).

use crate::assets::{Handle, Texture};
use crate::canvas::camera::WorldUnits;
use sgl_core::math::Vec2;

/// An axis-aligned pixel rectangle (`min` inclusive .. `max` exclusive),
/// used for sprite source rects within a texture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

impl Rect {
    /// A rect from its top-left corner and size, in pixels.
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            min: Vec2::new(x, y),
            max: Vec2::new(x + w, y + h),
        }
    }

    /// The source rect of grid frame `index` in a sheet laid out
    /// left-to-right, top-to-bottom (`columns` frames per row) with
    /// `frame_w × frame_h` cells — the Godot sheet convention (PR-7 sheets
    /// are single-row: e.g. `firebody-sheet` 96×32 = 3 columns of 32×32).
    pub fn sheet_frame(index: usize, frame_w: u32, frame_h: u32, columns: usize) -> Self {
        let columns = columns.max(1);
        let col = index % columns;
        let row = index / columns;
        Self::new(
            (col as u32 * frame_w) as f32,
            (row as u32 * frame_h) as f32,
            frame_w as f32,
            frame_h as f32,
        )
    }

    /// Width × height in pixels.
    pub fn size(&self) -> Vec2 {
        self.max - self.min
    }

    /// The overlap of two rects, or `None` when they do not overlap (an
    /// empty intersection is `None` — zero-area rects clip everything).
    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let min = self.min.max(other.min);
        let max = self.max.min(other.max);
        if min.x < max.x && min.y < max.y {
            Some(Rect { min, max })
        } else {
            None
        }
    }
}

/// One sprite to draw this frame — the AR-6 contract
/// (`texture/src-rect/transform/color/z/flip`).
///
/// Transform semantics match Godot's `Sprite2D`: `pos` is the sprite
/// **center** in logical pixels, `rot` is radians (positive = clockwise on
/// screen, y-down), and `scale` multiplies the natural frame size (the `src`
/// rect size — `scale (1,1)` draws pixel-for-pixel).
///
/// Under a world-unit camera (`Camera::with_units`) the **world** channel
/// reads `pos` in world units, the quad spans `src × scale /
/// pixels_per_unit` units, and with `y_up` the texture's top row lands at
/// the larger world y and positive `rot` reads counter-clockwise on screen
/// (it is the standard rotation in world axes either way). The screen
/// channel is always y-down logical pixels.
#[derive(Debug, Clone, Copy)]
pub struct SpriteInstance {
    /// Which texture to sample; resolved to a GPU page + UVs by the renderer.
    pub texture: Handle<Texture>,
    /// Source sub-rect in **texture pixels**; `None` = the full texture.
    pub src: Option<Rect>,
    /// Sprite center: the world camera's units on the world channel
    /// (logical pixels by default), logical pixels on the screen channel.
    pub pos: Vec2,
    /// Rotation about the center, radians — the standard rotation in world
    /// axes (clockwise on screen in y-down space, counter-clockwise under a
    /// y-up camera).
    pub rot: f32,
    /// Multiplier on the natural (src) size; `(1,1)` = native pixel size.
    pub scale: Vec2,
    /// RGBA modulate (straight alpha), multiplied with the texel — Godot
    /// `modulate`. Used as given: sRGB under the default gamma lighting,
    /// linear under `LightingSpace::Linear` (world channel only; the screen
    /// channel is always gamma).
    pub color: [f32; 4],
    /// Draw depth: lower draws first; equal `z` keeps push order (PR-2).
    /// NaN draws last.
    pub z: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    /// Companion normal-map texture for lighting (R-6, PR-4), sharing the
    /// diffuse's `src` rect. Must have been registered with
    /// `Renderer::upload_normal_map` for this `texture` — otherwise the
    /// sprite is lit as unmapped (full strength). `None` = no normal map.
    pub normal: Option<Handle<Texture>>,
    /// Godot light-mask layers this sprite receives light on (bits 1|2|4,
    /// PR-4). Default 1; the tilemap uses 7 (PR-3).
    pub light_mask: u32,
    /// Clip rectangle in **logical view pixels** (the 960×540 space both
    /// channels render into — never world units, whatever the camera's
    /// convention): pixels outside it are scissored away on the GPU.
    /// `None` = unclipped. Used by scrollable/clipped UI panels and by
    /// editor viewports to confine world rendering to a screen region.
    pub clip: Option<Rect>,
}

impl SpriteInstance {
    /// A plain sprite of `texture` centered at `pos`: full texture, no
    /// rotation, native scale, white modulate, `z = 0`, no normal map,
    /// light-mask layer 1. Adjust fields from there (struct-update syntax).
    pub fn new(texture: Handle<Texture>, pos: Vec2) -> Self {
        Self {
            texture,
            src: None,
            pos,
            rot: 0.0,
            scale: Vec2::ONE,
            color: [1.0, 1.0, 1.0, 1.0],
            z: 0.0,
            flip_x: false,
            flip_y: false,
            normal: None,
            light_mask: 1,
            clip: None,
        }
    }
}

/// Upper bound on the quads a single [`expand_tiled`] call may emit.
///
/// A target that is millions of tiles wide is a caller bug (a unit mix-up, an
/// unclamped camera rect), not a draw. The grid is dropped whole rather than
/// allocating an unbounded number of instances, so the failure is visible
/// instead of hanging the frame.
pub const MAX_TILED_QUADS: usize = 1 << 16;

/// Build one expanded quad from `template`.
///
/// `offset` is the quad's center relative to the target rect's center, in
/// unrotated **pixel** grid space (y-down). It is taken into `units` first —
/// divided by `pixels_per_unit`, y negated when `y_up` — and then
/// `template.rot` (if any) rotates it about that center in world axes and
/// rides along on the quad, so a rotated target expands as a rigid grid
/// rather than a sheared one. The default units multiply by `1.0` and divide
/// by `1.0` only, so the pixel path is bit-identical.
fn quad(
    template: &SpriteInstance,
    offset: Vec2,
    src: Rect,
    scale: Vec2,
    units: WorldUnits,
) -> SpriteInstance {
    let offset = Vec2::new(offset.x, offset.y * units.screen_y_sign()) / units.pixels_per_unit;
    let offset = if template.rot == 0.0 {
        offset
    } else {
        Vec2::from_angle(template.rot).rotate(offset)
    };
    SpriteInstance {
        src: Some(src),
        pos: template.pos + offset,
        scale,
        ..*template
    }
}

/// Repeat `src` across a `size`-sized target rect centered on `template.pos`,
/// appending one quad per tile to `out`.
///
/// `size` and `tile` are in `units` — the world camera's
/// [`Camera::units`](crate::canvas::Camera::units) on the world channel,
/// `WorldUnits::default()` for logical pixels (the screen channel) — and
/// `template.pos` is in the same units. `tile` is one tile's **on-screen**
/// size: `src` is stretched onto it, so a 8×8 source drawn with `tile = (16,
/// 16)` in pixels doubles every texel, and each emitted quad's `scale` is
/// `tile × pixels_per_unit / src`. The grid is `ceil(size / tile)` columns ×
/// rows laid out from the target's top-left *on screen* — row 0 (the source's
/// top rows) lands at the smallest y in pixels and at the largest world y
/// under `y_up`; the last column/row is clipped to the target and samples a
/// proportionally partial `src` (no wrap, no stretch to fit).
///
/// Every other template field — texture, color, `z`, `rot`, flips, `normal`,
/// `light_mask`, `clip` — propagates unchanged to every quad. The template's
/// `src` and `scale` are **recomputed** per quad and so are ignored: pass the
/// tile's source rect as `src` (`Rect::new(0.0, 0.0, w, h)` for a whole
/// texture, whose pixel size the expansion cannot otherwise know).
///
/// Flips apply **per emitted quad**: each tile is mirrored inside its own
/// cell, and a partial tile still samples the leading (`src.min`) edge before
/// mirroring, so the visible grid does not shift when a flip is toggled.
///
/// Nothing is emitted when the target, the tile or the source rect is empty or
/// negative on either axis, or when the grid would exceed [`MAX_TILED_QUADS`].
pub fn expand_tiled(
    out: &mut Vec<SpriteInstance>,
    template: &SpriteInstance,
    src: Rect,
    size: Vec2,
    tile: Vec2,
    units: WorldUnits,
) {
    // The grid is laid out in pixels; `quad` takes each offset into `units`.
    let size = size * units.pixels_per_unit;
    let tile = tile * units.pixels_per_unit;
    let src_size = src.size();
    // Written as a negated conjunction so NaN inputs fail the guard too.
    if !(size.x > 0.0
        && size.y > 0.0
        && tile.x > 0.0
        && tile.y > 0.0
        && src_size.x > 0.0
        && src_size.y > 0.0)
    {
        return;
    }
    let cols = (size.x / tile.x).ceil();
    let rows = (size.y / tile.y).ceil();
    // `cols`/`rows` are at least 1 and the guard above rules out NaN, so the
    // product is well-ordered (an infinite target trips the bound).
    if cols * rows > MAX_TILED_QUADS as f32 {
        return;
    }
    // A partial tile shrinks its quad and its source rect by the same
    // fraction, so the scale is one constant across the whole grid.
    let scale = tile / src_size;
    let half = size * 0.5;
    for row in 0..rows as usize {
        let y = row as f32 * tile.y;
        let h = (y + tile.y).min(size.y) - y;
        if h <= 0.0 {
            continue;
        }
        for col in 0..cols as usize {
            let x = col as f32 * tile.x;
            let w = (x + tile.x).min(size.x) - x;
            if w <= 0.0 {
                continue;
            }
            let frac = Vec2::new(w / tile.x, h / tile.y);
            out.push(quad(
                template,
                Vec2::new(x + w * 0.5, y + h * 0.5) - half,
                Rect {
                    min: src.min,
                    max: src.min + src_size * frac,
                },
                scale,
                units,
            ));
        }
    }
}

/// Expand `src` as a 9-slice over a `size`-sized target rect centered on
/// `template.pos`, appending up to 9 quads to `out`.
///
/// `size` and `template.pos` are in `units`, as for [`expand_tiled`]; the
/// top border lands at the smallest y in pixels and at the largest world y
/// under `y_up`. `border_px` is a uniform border in **source** pixels
/// whatever the units, so the four corners draw at native scale (`scale ==
/// (1, 1)` exactly — `border_px / pixels_per_unit` units across), the four
/// edges stretch along their long axis, and the center stretches both. The
/// border is clamped to half the source *and* half the target on each axis,
/// which is what keeps the corners native on a target smaller than `2 ×
/// border`; the center and edge quads that then have zero target or zero
/// source extent are dropped rather than emitted with zero area or a
/// negative scale.
///
/// The emitted quads tile the target exactly: adjacent quads share an edge and
/// none overlap, and their union is the full target rect whenever the middle
/// band survives.
///
/// Template-field propagation, `src`/`scale` override and flip semantics match
/// [`expand_tiled`]; nothing is emitted for an empty or negative target or
/// source rect, or a negative `border_px`.
pub fn expand_nine_slice(
    out: &mut Vec<SpriteInstance>,
    template: &SpriteInstance,
    src: Rect,
    size: Vec2,
    border_px: f32,
    units: WorldUnits,
) {
    // The bands are laid out in pixels; `quad` takes each offset into `units`.
    let size = size * units.pixels_per_unit;
    let src_size = src.size();
    if !(size.x > 0.0 && size.y > 0.0 && border_px >= 0.0 && src_size.x > 0.0 && src_size.y > 0.0) {
        return;
    }
    let bx = border_px.min(src_size.x * 0.5).min(size.x * 0.5);
    let by = border_px.min(src_size.y * 0.5).min(size.y * 0.5);
    // `2 * bx <= size.x` and `2 * bx <= src_size.x` hold exactly (halving and
    // doubling are exact in binary floating point), so no span goes negative.
    let col_w = [bx, size.x - 2.0 * bx, bx];
    let row_h = [by, size.y - 2.0 * by, by];
    let src_w = [bx, src_size.x - 2.0 * bx, bx];
    let src_h = [by, src_size.y - 2.0 * by, by];

    let half = size * 0.5;
    let mut y = 0.0;
    let mut src_y = src.min.y;
    for (&h, &sh) in row_h.iter().zip(&src_h) {
        let mut x = 0.0;
        let mut src_x = src.min.x;
        for (&w, &sw) in col_w.iter().zip(&src_w) {
            if w > 0.0 && h > 0.0 && sw > 0.0 && sh > 0.0 {
                out.push(quad(
                    template,
                    Vec2::new(x + w * 0.5, y + h * 0.5) - half,
                    Rect {
                        min: Vec2::new(src_x, src_y),
                        max: Vec2::new(src_x + sw, src_y + sh),
                    },
                    Vec2::new(w / sw, h / sh),
                    units,
                ));
            }
            x += w;
            src_x += sw;
        }
        y += h;
        src_y += sh;
    }
}

/// A frame's worth of sprites: the world channel and the screen channel.
/// Reused across frames ([`clear`](Self::clear) keeps allocations).
#[derive(Debug, Default)]
pub struct DrawList {
    /// Sprites in world space (drawn through the world camera).
    pub world: Vec<SpriteInstance>,
    /// Sprites in screen space (fixed logical-resolution camera; UI/HUD).
    pub screen: Vec<SpriteInstance>,
}

impl DrawList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Empty both channels for reuse next frame (keeps the allocations).
    pub fn clear(&mut self) {
        self.world.clear();
        self.screen.clear();
    }

    /// Queue a world-space sprite.
    pub fn push(&mut self, instance: SpriteInstance) {
        self.world.push(instance);
    }

    /// Queue a screen-space sprite (logical 960×540 coordinates).
    pub fn push_screen(&mut self, instance: SpriteInstance) {
        self.screen.push(instance);
    }

    /// Queue a world-space tiled expansion in the world camera's `units`
    /// (`camera.units()`) — see [`expand_tiled`].
    pub fn push_tiled(
        &mut self,
        template: &SpriteInstance,
        src: Rect,
        size: Vec2,
        tile: Vec2,
        units: WorldUnits,
    ) {
        expand_tiled(&mut self.world, template, src, size, tile, units);
    }

    /// Queue a screen-space tiled expansion (logical pixels) — see
    /// [`expand_tiled`].
    pub fn push_screen_tiled(
        &mut self,
        template: &SpriteInstance,
        src: Rect,
        size: Vec2,
        tile: Vec2,
    ) {
        expand_tiled(
            &mut self.screen,
            template,
            src,
            size,
            tile,
            WorldUnits::default(),
        );
    }

    /// Queue a world-space 9-slice expansion in the world camera's `units`
    /// (`camera.units()`) — see [`expand_nine_slice`].
    pub fn push_nine_slice(
        &mut self,
        template: &SpriteInstance,
        src: Rect,
        size: Vec2,
        border_px: f32,
        units: WorldUnits,
    ) {
        expand_nine_slice(&mut self.world, template, src, size, border_px, units);
    }

    /// Queue a screen-space 9-slice expansion (logical pixels) — see
    /// [`expand_nine_slice`].
    pub fn push_screen_nine_slice(
        &mut self,
        template: &SpriteInstance,
        src: Rect,
        size: Vec2,
        border_px: f32,
    ) {
        expand_nine_slice(
            &mut self.screen,
            template,
            src,
            size,
            border_px,
            WorldUnits::default(),
        );
    }

    /// Stable-sort both channels by ascending `z` (equal `z` keeps push
    /// order — PR-2 tree-order parity). A NaN `z` draws after every other
    /// `z`. Called by the renderer before batching; idempotent.
    pub fn sort(&mut self) {
        // A total order (a NaN-tolerant `partial_cmp` is not one, and
        // `sort_by` may panic on it) that still treats `-0.0` as `0.0`.
        let key = |s: &SpriteInstance| match s.z {
            z if z.is_nan() => f32::NAN,
            0.0 => 0.0, // Also matches `-0.0`.
            z => z,
        };
        let by_z = |a: &SpriteInstance, b: &SpriteInstance| key(a).total_cmp(&key(b));
        self.world.sort_by(by_z);
        self.screen.sort_by(by_z);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #255: the expansion guards are strict — a zero target, tile, source,
    /// or border-less zero source emits nothing, while the smallest positive
    /// inputs emit one quad.
    #[wasm_bindgen_test(unsupported = test)]
    fn expansion_guards_are_strict_at_zero() {
        let template = template();
        let unit = Rect::new(0.0, 0.0, 1.0, 1.0);
        // (size, tile, src, applies to nine-slice too)
        let cases = [
            (Vec2::new(0.0, 4.0), Vec2::ONE, unit, true),
            (Vec2::new(4.0, 0.0), Vec2::ONE, unit, true),
            (Vec2::splat(4.0), Vec2::new(0.0, 1.0), unit, false),
            (Vec2::splat(4.0), Vec2::new(1.0, 0.0), unit, false),
            (
                Vec2::splat(4.0),
                Vec2::ONE,
                Rect::new(0.0, 0.0, 0.0, 1.0),
                true,
            ),
            (
                Vec2::splat(4.0),
                Vec2::ONE,
                Rect::new(0.0, 0.0, 1.0, 0.0),
                true,
            ),
        ];
        for (size, tile, src, nine_slice_too) in cases {
            let mut out = Vec::new();
            expand_tiled(&mut out, &template, src, size, tile, WorldUnits::default());
            assert!(out.is_empty(), "tiled {size} {tile} {src:?}");
            if nine_slice_too {
                let mut out = Vec::new();
                expand_nine_slice(&mut out, &template, src, size, 1.0, WorldUnits::default());
                assert!(out.is_empty(), "nine-slice {size} {src:?}");
            }
        }
        let mut out = Vec::new();
        expand_tiled(
            &mut out,
            &template,
            unit,
            Vec2::splat(f32::MIN_POSITIVE),
            Vec2::ONE,
            WorldUnits::default(),
        );
        assert_eq!(out.len(), 1);
        let mut out = Vec::new();
        expand_nine_slice(
            &mut out,
            &template,
            unit,
            Vec2::splat(f32::MIN_POSITIVE),
            0.0,
            WorldUnits::default(),
        );
        assert_eq!(out.len(), 1);
    }

    /// #255: touching rects and zero-area rects have no intersection; a
    /// one-pixel overlap does.
    #[wasm_bindgen_test(unsupported = test)]
    fn intersection_is_strict_and_never_empty() {
        let a = Rect::new(0.0, 0.0, 4.0, 4.0);
        assert_eq!(
            a.intersection(&Rect::new(4.0, 0.0, 4.0, 4.0)),
            None,
            "touching right"
        );
        assert_eq!(
            a.intersection(&Rect::new(0.0, 4.0, 4.0, 4.0)),
            None,
            "touching below"
        );
        assert_eq!(
            a.intersection(&Rect::new(-4.0, 0.0, 4.0, 4.0)),
            None,
            "touching left"
        );
        assert_eq!(
            a.intersection(&Rect::new(2.0, 2.0, 0.0, 3.0)),
            None,
            "zero width"
        );
        assert_eq!(
            a.intersection(&Rect::new(3.0, 3.0, 4.0, 4.0)),
            Some(Rect::new(3.0, 3.0, 1.0, 1.0))
        );
        assert_eq!(
            Rect::new(5.0, 5.0, 1.0, 1.0).intersection(&a),
            None,
            "disjoint"
        );
    }

    use crate::assets::Assets;

    fn handle(n: u32) -> Handle<Texture> {
        // Real handles come from the cache; mint distinct ones through it.
        let mut assets: Assets<Texture> = Assets::new();
        let mut h = None;
        for i in 0..=n {
            h = Some(assets.insert(
                PathBuf::from(format!("{i}.png")),
                Texture {
                    width: 1,
                    height: 1,
                    rgba: vec![0; 4],
                },
            ));
        }
        h.unwrap()
    }

    fn sprite(z: f32, tag: f32) -> SpriteInstance {
        SpriteInstance {
            z,
            // `rot` doubles as an identity tag for order assertions.
            rot: tag,
            ..SpriteInstance::new(handle(0), Vec2::ZERO)
        }
    }

    /// Sorting is by ascending z and **stable**: equal z keeps push order.
    #[wasm_bindgen_test(unsupported = test)]
    fn sort_is_by_z_then_push_order() {
        let mut list = DrawList::new();
        list.push(sprite(2.0, 0.0));
        list.push(sprite(0.0, 1.0));
        list.push(sprite(1.0, 2.0));
        list.push(sprite(0.0, 3.0)); // same z as tag 1 — must stay after it
        list.sort();
        let order: Vec<f32> = list.world.iter().map(|s| s.rot).collect();
        assert_eq!(order, vec![1.0, 3.0, 2.0, 0.0]);
    }

    /// #321: NaN `z` values sort last without panicking, and the finite
    /// entries stay ascending and stable (`-0.0` ties with `0.0`).
    #[wasm_bindgen_test(unsupported = test)]
    fn nan_z_sorts_last_and_keeps_the_rest_stable() {
        let mut list = DrawList::new();
        let mut expected = Vec::new();
        for i in 0..200u16 {
            let tag = f32::from(i);
            let z = match i % 5 {
                0 => f32::NAN,
                1 => -f32::NAN,
                2 => -0.0,
                3 => 0.0,
                _ => f32::from(i % 7) - 3.0,
            };
            list.push(sprite(z, tag));
            if !z.is_nan() {
                expected.push((if z == 0.0 { 0.0 } else { z }, tag));
            }
        }
        expected.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        list.sort();
        let (finite, nan) = list.world.split_at(expected.len());
        let order: Vec<f32> = finite.iter().map(|s| s.rot).collect();
        let want: Vec<f32> = expected.iter().map(|e| e.1).collect();
        assert_eq!(order, want);
        assert!(nan.iter().all(|s| s.z.is_nan()));
    }

    /// The two channels sort independently.
    #[wasm_bindgen_test(unsupported = test)]
    fn screen_channel_sorts_independently() {
        let mut list = DrawList::new();
        list.push(sprite(5.0, 0.0));
        list.push_screen(sprite(9.0, 1.0));
        list.push_screen(sprite(-1.0, 2.0));
        list.sort();
        assert_eq!(list.world.len(), 1);
        let screen: Vec<f32> = list.screen.iter().map(|s| s.rot).collect();
        assert_eq!(screen, vec![2.0, 1.0]);
    }

    /// Sheet frame rects cut a single-row sheet left to right (PR-7 layout).
    #[wasm_bindgen_test(unsupported = test)]
    fn sheet_frame_cuts_single_row_sheets() {
        // firebody-sheet: 96×32 = 3 frames of 32×32.
        let f0 = Rect::sheet_frame(0, 32, 32, 3);
        let f2 = Rect::sheet_frame(2, 32, 32, 3);
        assert_eq!(f0, Rect::new(0.0, 0.0, 32.0, 32.0));
        assert_eq!(f2, Rect::new(64.0, 0.0, 32.0, 32.0));
        assert_eq!(f2.size(), Vec2::new(32.0, 32.0));
    }

    /// Multi-row sheets wrap by `columns` (e.g. beamend 4×8 grids later).
    #[wasm_bindgen_test(unsupported = test)]
    fn sheet_frame_wraps_rows() {
        let f5 = Rect::sheet_frame(5, 16, 16, 4);
        assert_eq!(f5, Rect::new(16.0, 16.0, 16.0, 16.0));
    }

    /// R-6 lighting defaults: no normal map, light-mask layer 1.
    #[wasm_bindgen_test(unsupported = test)]
    fn new_sprite_has_light_defaults() {
        let s = SpriteInstance::new(handle(0), Vec2::ZERO);
        assert_eq!(s.light_mask, 1);
        assert!(s.normal.is_none());
    }

    /// The rect an unrotated expanded quad covers in a camera's units (the
    /// renderer's sprite contract): its center ± half the sampled source
    /// size times the quad's scale over `pixels_per_unit`.
    fn extent_in(s: &SpriteInstance, ppu: f32) -> (Vec2, Vec2) {
        let size = s.src.expect("an expanded quad always carries a src").size() * s.scale / ppu;
        (s.pos - size * 0.5, s.pos + size * 0.5)
    }

    /// The union of the quads' extents and the total area they cover, in
    /// logical pixels.
    fn coverage(quads: &[SpriteInstance]) -> (Vec2, Vec2, f32) {
        coverage_in(quads, 1.0)
    }

    /// [`coverage`] measured in a camera's units at `ppu` pixels per unit.
    fn coverage_in(quads: &[SpriteInstance], ppu: f32) -> (Vec2, Vec2, f32) {
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        let mut area = 0.0;
        for q in quads {
            let (lo, hi) = extent_in(q, ppu);
            min = min.min(lo);
            max = max.max(hi);
            area += (hi.x - lo.x) * (hi.y - lo.y);
        }
        (min, max, area)
    }

    fn template() -> SpriteInstance {
        SpriteInstance::new(handle(0), Vec2::ZERO)
    }

    /// A y-up world at 32 px/unit (the shadow-sp convention from #262).
    fn units32() -> WorldUnits {
        WorldUnits {
            pixels_per_unit: 32.0,
            y_up: true,
        }
    }

    /// #262: under a world-unit, y-up camera the target and tile are in
    /// units and the source's top row lands at the larger world y. A 2×1.5
    /// unit target tiled by 1-unit tiles from a 16×16 source at 32 px/unit is
    /// a 2×2 grid whose second row is a half tile; `scale` is still `tile_px
    /// / src` (2), so each full quad spans `16 × 2 / 32` = 1 unit.
    #[wasm_bindgen_test(unsupported = test)]
    fn tiled_grid_lays_out_in_world_units_with_y_up() {
        let t = SpriteInstance {
            pos: Vec2::new(10.0, 5.0),
            ..template()
        };
        let mut out = Vec::new();
        expand_tiled(
            &mut out,
            &t,
            Rect::new(0.0, 0.0, 16.0, 16.0),
            Vec2::new(2.0, 1.5),
            Vec2::ONE,
            units32(),
        );
        assert_eq!(out.len(), 4);
        // Row 0 (the source's top) is the target's top row: y = 5 + 0.75 - 0.5.
        assert_eq!(out[0].pos, Vec2::new(9.5, 5.25));
        assert_eq!(out[0].src, Some(Rect::new(0.0, 0.0, 16.0, 16.0)));
        assert_eq!(out[0].scale, Vec2::splat(2.0));
        assert_eq!(out[1].pos, Vec2::new(10.5, 5.25));
        // Row 1 is the half-unit remainder *below* it, sampling the top 8 rows.
        assert_eq!(out[2].pos, Vec2::new(9.5, 4.5));
        assert_eq!(out[2].src, Some(Rect::new(0.0, 0.0, 16.0, 8.0)));
        assert_eq!(out[2].scale, Vec2::splat(2.0));
        // In units the quads tile the target (9, 4.25)..(11, 5.75) once.
        let (min, max, area) = coverage_in(&out, 32.0);
        assert_eq!(min, Vec2::new(9.0, 4.25));
        assert_eq!(max, Vec2::new(11.0, 5.75));
        assert_eq!(area, 2.0 * 1.5);
    }

    /// #262: a 9-slice in world units keeps its border in source pixels — the
    /// corners span `border_px / ppu` units at native scale — and its top
    /// border lands at the larger world y under `y_up`. A 16×16 source with
    /// a 4 px border over a 2×1 unit target at 32 px/unit.
    #[wasm_bindgen_test(unsupported = test)]
    fn nine_slice_lays_out_in_world_units_with_y_up() {
        let mut out = Vec::new();
        expand_nine_slice(
            &mut out,
            &template(),
            Rect::new(0.0, 0.0, 16.0, 16.0),
            Vec2::new(2.0, 1.0),
            4.0,
            units32(),
        );
        assert_eq!(out.len(), 9);
        let (min, max, area) = coverage_in(&out, 32.0);
        assert_eq!(min, Vec2::new(-1.0, -0.5));
        assert_eq!(max, Vec2::new(1.0, 0.5));
        assert_eq!(area, 2.0 * 1.0, "the nine quads tile the target once");

        // Top-left corner: the source's top-left 4×4 at native scale, an
        // eighth of a unit square in the target's top-left (larger y).
        assert_eq!(out[0].pos, Vec2::new(-0.9375, 0.4375));
        assert_eq!(out[0].src, Some(Rect::new(0.0, 0.0, 4.0, 4.0)));
        assert_eq!(out[0].scale, Vec2::ONE);
        // Top edge: the middle 8 source px over the 64 - 8 = 56 px between
        // the corners, height native.
        assert_eq!(out[1].pos, Vec2::new(0.0, 0.4375));
        assert_eq!(out[1].src, Some(Rect::new(4.0, 0.0, 8.0, 4.0)));
        assert_eq!(out[1].scale, Vec2::new(7.0, 1.0));
        // Center: the middle 8×8 over 56×24 px.
        assert_eq!(out[4].pos, Vec2::ZERO);
        assert_eq!(out[4].src, Some(Rect::new(4.0, 4.0, 8.0, 8.0)));
        assert_eq!(out[4].scale, Vec2::new(7.0, 3.0));
        // Bottom-right corner: the source's last 4×4, at the smaller y.
        assert_eq!(out[8].pos, Vec2::new(0.9375, -0.4375));
        assert_eq!(out[8].src, Some(Rect::new(12.0, 12.0, 4.0, 4.0)));
        assert_eq!(out[8].scale, Vec2::ONE);
    }

    /// #262: the y flip happens before the template rotation, so a y-up grid
    /// turns as a rigid body in world axes like the sprites themselves. A 1×2
    /// unit column of tiles under a +90° (counter-clockwise on screen) turn
    /// puts the source's top tile, unrotated at +y, on the -x side.
    #[wasm_bindgen_test(unsupported = test)]
    fn tiled_rotation_turns_a_y_up_grid_in_world_axes() {
        let t = SpriteInstance {
            rot: std::f32::consts::FRAC_PI_2,
            ..template()
        };
        let mut out = Vec::new();
        expand_tiled(
            &mut out,
            &t,
            Rect::new(0.0, 0.0, 8.0, 8.0),
            Vec2::new(1.0, 2.0),
            Vec2::ONE,
            WorldUnits {
                pixels_per_unit: 16.0,
                y_up: true,
            },
        );
        assert_eq!(out.len(), 2);
        assert!(
            (out[0].pos - Vec2::new(-0.5, 0.0)).length() < 1e-4,
            "{:?}",
            out[0].pos
        );
        assert!(
            (out[1].pos - Vec2::new(0.5, 0.0)).length() < 1e-4,
            "{:?}",
            out[1].pos
        );
    }

    /// A 40×16 target tiled with a 16×8 on-screen tile cut from an 8×8 source
    /// is a 3×2 grid whose last column is clipped to 8 px and samples the
    /// leading half of the source. Every quad shares `scale = tile / src`.
    #[wasm_bindgen_test(unsupported = test)]
    fn tiled_grid_clips_the_last_column_to_a_partial_src() {
        let src = Rect::new(0.0, 0.0, 8.0, 8.0);
        let t = SpriteInstance {
            pos: Vec2::new(100.0, 50.0),
            ..template()
        };
        let mut out = Vec::new();
        expand_tiled(
            &mut out,
            &t,
            src,
            Vec2::new(40.0, 16.0),
            Vec2::new(16.0, 8.0),
            WorldUnits::default(),
        );

        assert_eq!(out.len(), 6);
        // Target spans (80, 42)..(120, 58).
        let (min, max, area) = coverage(&out);
        assert_eq!(min, Vec2::new(80.0, 42.0));
        assert_eq!(max, Vec2::new(120.0, 58.0));
        assert_eq!(area, 40.0 * 16.0, "tiles cover the target exactly, once");

        // Row 0, column 0: a full tile at the target's top-left.
        assert_eq!(out[0].pos, Vec2::new(88.0, 46.0));
        assert_eq!(out[0].src, Some(Rect::new(0.0, 0.0, 8.0, 8.0)));
        assert_eq!(out[0].scale, Vec2::new(2.0, 1.0));
        // Row 0, column 2: 8 of 16 px wide, so half the source width.
        assert_eq!(out[2].pos, Vec2::new(116.0, 46.0));
        assert_eq!(out[2].src, Some(Rect::new(0.0, 0.0, 4.0, 8.0)));
        assert_eq!(out[2].scale, Vec2::new(2.0, 1.0));
        // Row 1, column 1: the second row starts 8 px lower.
        assert_eq!(out[4].pos, Vec2::new(104.0, 54.0));
        assert_eq!(out[4].src, Some(Rect::new(0.0, 0.0, 8.0, 8.0)));
    }

    /// Empty or negative targets, tiles and source rects emit nothing, and a
    /// grid past [`MAX_TILED_QUADS`] is dropped whole instead of flooding the
    /// draw list.
    #[wasm_bindgen_test(unsupported = test)]
    fn tiled_rejects_degenerate_and_unbounded_grids() {
        let src = Rect::new(0.0, 0.0, 8.0, 8.0);
        let t = template();
        let tile = Vec2::new(8.0, 8.0);
        let size = Vec2::new(16.0, 16.0);
        for (src, size, tile) in [
            (src, Vec2::new(0.0, 16.0), tile),
            (src, Vec2::new(16.0, -1.0), tile),
            (src, size, Vec2::new(0.0, 8.0)),
            (src, size, Vec2::new(8.0, -8.0)),
            (Rect::new(0.0, 0.0, 0.0, 8.0), size, tile),
        ] {
            let mut out = Vec::new();
            expand_tiled(&mut out, &t, src, size, tile, WorldUnits::default());
            assert!(out.is_empty(), "size {size:?} tile {tile:?} src {src:?}");
        }
        // 300 × 300 one-pixel tiles = 90_000 quads, past the 65_536 bound.
        let mut out = Vec::new();
        expand_tiled(
            &mut out,
            &t,
            src,
            Vec2::splat(300.0),
            Vec2::ONE,
            WorldUnits::default(),
        );
        assert!(out.is_empty(), "an over-large grid emits nothing");
        // One quad under the bound still draws.
        expand_tiled(
            &mut out,
            &t,
            src,
            Vec2::splat(200.0),
            Vec2::ONE,
            WorldUnits::default(),
        );
        assert_eq!(out.len(), 40_000);
    }

    /// A rotated template rotates each tile's offset about the target center
    /// and hands every tile the same rotation, so the grid stays rigid. Two
    /// 8×8 tiles side by side at 90° stack vertically instead.
    #[wasm_bindgen_test(unsupported = test)]
    fn tiled_rotation_turns_the_grid_about_the_center() {
        let quarter = std::f32::consts::FRAC_PI_2;
        let t = SpriteInstance {
            rot: quarter,
            ..template()
        };
        let mut out = Vec::new();
        expand_tiled(
            &mut out,
            &t,
            Rect::new(0.0, 0.0, 8.0, 8.0),
            Vec2::new(16.0, 8.0),
            Vec2::new(8.0, 8.0),
            WorldUnits::default(),
        );
        assert_eq!(out.len(), 2);
        // Unrotated offsets are (-4, 0) and (4, 0); a clockwise quarter turn
        // in y-down space maps them to (0, -4) and (0, 4).
        assert!(
            (out[0].pos - Vec2::new(0.0, -4.0)).length() < 1e-4,
            "{:?}",
            out[0].pos
        );
        assert!(
            (out[1].pos - Vec2::new(0.0, 4.0)).length() < 1e-4,
            "{:?}",
            out[1].pos
        );
        assert_eq!(out[0].rot, quarter);
        assert_eq!(out[1].rot, quarter);
    }

    /// Flips ride every emitted quad, and a partial tile keeps sampling from
    /// the source's leading edge — so toggling a flip never shifts the grid.
    #[wasm_bindgen_test(unsupported = test)]
    fn tiled_flips_ride_each_quad_over_the_leading_src_edge() {
        let t = SpriteInstance {
            flip_x: true,
            flip_y: true,
            color: [0.25, 0.5, 0.75, 0.5],
            z: 7.0,
            light_mask: 4,
            ..template()
        };
        let mut out = Vec::new();
        expand_tiled(
            &mut out,
            &t,
            Rect::new(2.0, 3.0, 8.0, 8.0),
            Vec2::new(12.0, 8.0),
            Vec2::new(8.0, 8.0),
            WorldUnits::default(),
        );
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|q| q.flip_x && q.flip_y));
        assert!(out.iter().all(|q| q.z == 7.0 && q.light_mask == 4));
        assert!(out.iter().all(|q| q.color == [0.25, 0.5, 0.75, 0.5]));
        assert_eq!(out[0].src, Some(Rect::new(2.0, 3.0, 8.0, 8.0)));
        // The 4 px remainder samples x 2..6 — the leading half, not 6..10.
        assert_eq!(out[1].src, Some(Rect::new(2.0, 3.0, 4.0, 8.0)));
        assert_eq!(out[0].pos, Vec2::new(-2.0, 0.0));
        assert_eq!(out[1].pos, Vec2::new(4.0, 0.0));
    }

    /// A 16×16 source with a 4 px border over a 40×24 target: native 4×4
    /// corners, edges stretched on one axis, the center on both — and the
    /// nine quads tile the target exactly (union = target, no overlap).
    #[wasm_bindgen_test(unsupported = test)]
    fn nine_slice_tiles_the_target_with_native_corners() {
        let mut out = Vec::new();
        expand_nine_slice(
            &mut out,
            &template(),
            Rect::new(0.0, 0.0, 16.0, 16.0),
            Vec2::new(40.0, 24.0),
            4.0,
            WorldUnits::default(),
        );
        assert_eq!(out.len(), 9);

        let (min, max, area) = coverage(&out);
        assert_eq!(min, Vec2::new(-20.0, -12.0));
        assert_eq!(max, Vec2::new(20.0, 12.0));
        assert_eq!(area, 40.0 * 24.0, "the nine quads tile the target once");

        // Top-left corner: 4×4 of source at 4×4 on screen.
        assert_eq!(out[0].pos, Vec2::new(-18.0, -10.0));
        assert_eq!(out[0].src, Some(Rect::new(0.0, 0.0, 4.0, 4.0)));
        assert_eq!(out[0].scale, Vec2::ONE);
        // Top edge: the middle 8 source px stretched over 32 px, height native.
        assert_eq!(out[1].pos, Vec2::new(0.0, -10.0));
        assert_eq!(out[1].src, Some(Rect::new(4.0, 0.0, 8.0, 4.0)));
        assert_eq!(out[1].scale, Vec2::new(4.0, 1.0));
        // Center: the middle 8×8 stretched over 32×16.
        assert_eq!(out[4].pos, Vec2::ZERO);
        assert_eq!(out[4].src, Some(Rect::new(4.0, 4.0, 8.0, 8.0)));
        assert_eq!(out[4].scale, Vec2::new(4.0, 2.0));
        // Bottom-right corner: the last 4×4 source px, native.
        assert_eq!(out[8].pos, Vec2::new(18.0, 10.0));
        assert_eq!(out[8].src, Some(Rect::new(12.0, 12.0, 4.0, 4.0)));
        assert_eq!(out[8].scale, Vec2::ONE);
    }

    /// A target exactly `2 × border` wide loses its center column: the two
    /// remaining columns still tile the target and keep native corners.
    #[wasm_bindgen_test(unsupported = test)]
    fn nine_slice_drops_the_zero_width_center_column() {
        let mut out = Vec::new();
        expand_nine_slice(
            &mut out,
            &template(),
            Rect::new(0.0, 0.0, 16.0, 16.0),
            Vec2::new(8.0, 24.0),
            4.0,
            WorldUnits::default(),
        );
        assert_eq!(out.len(), 6, "three rows × two surviving columns");
        let (min, max, area) = coverage(&out);
        assert_eq!(min, Vec2::new(-4.0, -12.0));
        assert_eq!(max, Vec2::new(4.0, 12.0));
        assert_eq!(area, 8.0 * 24.0);
        assert_eq!(out[0].pos, Vec2::new(-2.0, -10.0));
        assert_eq!(out[0].scale, Vec2::ONE);
        assert_eq!(out[1].pos, Vec2::new(2.0, -10.0));
        assert_eq!(out[1].src, Some(Rect::new(12.0, 0.0, 4.0, 4.0)));
        // Left edge: native width, the middle 8 source rows over 16 px.
        assert_eq!(out[2].pos, Vec2::new(-2.0, 0.0));
        assert_eq!(out[2].src, Some(Rect::new(0.0, 4.0, 4.0, 8.0)));
        assert_eq!(out[2].scale, Vec2::new(1.0, 2.0));
    }

    /// A target smaller than `2 × border` shrinks the border on both the
    /// target *and* the source, so the four corners still draw 1:1 rather
    /// than squashing native art.
    #[wasm_bindgen_test(unsupported = test)]
    fn nine_slice_corners_stay_native_on_a_tiny_target() {
        let mut out = Vec::new();
        expand_nine_slice(
            &mut out,
            &template(),
            Rect::new(0.0, 0.0, 16.0, 16.0),
            Vec2::splat(6.0),
            4.0,
            WorldUnits::default(),
        );
        assert_eq!(out.len(), 4, "only the corners survive");
        for q in &out {
            assert_eq!(q.scale, Vec2::ONE);
            assert_eq!(q.src.unwrap().size(), Vec2::splat(3.0));
        }
        let (min, max, area) = coverage(&out);
        assert_eq!((min, max), (Vec2::splat(-3.0), Vec2::splat(3.0)));
        assert_eq!(area, 36.0);
        assert_eq!(out[0].src, Some(Rect::new(0.0, 0.0, 3.0, 3.0)));
        assert_eq!(out[1].src, Some(Rect::new(13.0, 0.0, 3.0, 3.0)));
        assert_eq!(out[3].src, Some(Rect::new(13.0, 13.0, 3.0, 3.0)));
    }

    /// A border wider than half the source clamps to half the source: a 4×4
    /// texture asked for a 10 px border yields 2 px corners and no middle
    /// band at all (there is no source left to stretch).
    #[wasm_bindgen_test(unsupported = test)]
    fn nine_slice_border_clamps_to_half_the_source() {
        let mut out = Vec::new();
        expand_nine_slice(
            &mut out,
            &template(),
            Rect::new(0.0, 0.0, 4.0, 4.0),
            Vec2::splat(20.0),
            10.0,
            WorldUnits::default(),
        );
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].pos, Vec2::splat(-9.0));
        assert_eq!(out[0].src, Some(Rect::new(0.0, 0.0, 2.0, 2.0)));
        assert_eq!(out[3].pos, Vec2::splat(9.0));
        assert_eq!(out[3].src, Some(Rect::new(2.0, 2.0, 2.0, 2.0)));
        for q in &out {
            assert_eq!(q.scale, Vec2::ONE);
        }
    }

    /// Degenerate 9-slice inputs emit nothing rather than zero-area or
    /// negative-scale quads.
    #[wasm_bindgen_test(unsupported = test)]
    fn nine_slice_rejects_degenerate_inputs() {
        let src = Rect::new(0.0, 0.0, 16.0, 16.0);
        let t = template();
        for (src, size, border) in [
            (src, Vec2::new(0.0, 24.0), 4.0),
            (src, Vec2::new(40.0, -2.0), 4.0),
            (src, Vec2::new(40.0, 24.0), -1.0),
            (Rect::new(0.0, 0.0, 16.0, 0.0), Vec2::new(40.0, 24.0), 4.0),
        ] {
            let mut out = Vec::new();
            expand_nine_slice(&mut out, &t, src, size, border, WorldUnits::default());
            assert!(out.is_empty(), "size {size:?} border {border} src {src:?}");
        }
        // A zero border is legal: one stretched quad, no corners.
        let mut out = Vec::new();
        expand_nine_slice(
            &mut out,
            &t,
            src,
            Vec2::new(40.0, 24.0),
            0.0,
            WorldUnits::default(),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].scale, Vec2::new(2.5, 1.5));
    }

    /// The `DrawList` conveniences expand onto the channel they name and
    /// leave the other one alone.
    #[wasm_bindgen_test(unsupported = test)]
    fn push_helpers_target_one_channel_each() {
        let src = Rect::new(0.0, 0.0, 8.0, 8.0);
        let t = template();
        let mut list = DrawList::new();
        list.push_tiled(
            &t,
            src,
            Vec2::new(16.0, 8.0),
            Vec2::new(8.0, 8.0),
            WorldUnits::default(),
        );
        list.push_screen_nine_slice(&t, src, Vec2::new(24.0, 24.0), 2.0);
        assert_eq!(list.world.len(), 2);
        assert_eq!(list.screen.len(), 9);
        list.clear();
        list.push_screen_tiled(&t, src, Vec2::new(16.0, 8.0), Vec2::new(8.0, 8.0));
        list.push_nine_slice(&t, src, Vec2::new(24.0, 24.0), 2.0, WorldUnits::default());
        assert_eq!(list.screen.len(), 2);
        assert_eq!(list.world.len(), 9);
    }
}
