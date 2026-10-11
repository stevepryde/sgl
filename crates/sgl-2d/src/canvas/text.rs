//! Glyph-atlas text rendering (AR-6): TTF glyphs rasterized on demand with
//! **fontdue** (antialiased, at the exact target pixel size — matching Godot's
//! antialiased font rendering at 1:1 scale) into shelf-packed CPU atlas
//! pages, then drawn through the ordinary sprite pipeline as
//! [`SpriteInstance`] quads on either [`DrawList`] channel.
//!
//! Flow per frame:
//! 1. [`TextRenderer::draw`] / [`draw_in_rect`](TextRenderer::draw_in_rect)
//!    lay out a line, rasterizing any glyph not yet in a page, and queue
//!    positioned quads (shadow → outline → fill push order; z ties resolve
//!    by push order, PR-2).
//! 2. [`TextRenderer::end_frame`] republishes each dirty page into its one
//!    [`Texture`] asset, in place under a handle that stays stable for the
//!    page's lifetime, flushes the queued quads into the `DrawList`, and
//!    returns the changed pages' handles, which the app passes to
//!    `Renderer::upload_texture` or `SpritePass::upload` before rendering
//!    (an already-uploaded page has its GPU pixels replaced in place). A
//!    page costs one asset and one texture however often it changes;
//!    unchanged pages and other textures are not uploaded again.
//!
//! **Outline** (Godot `outline_size`) is done by re-drawing the glyph quads
//! offset along concentric rings of the outline radius, in the outline
//! color, before the fill — no font metrics involved. **Shadow** (Godot
//! `shadow_offset`/`shadow_size`) is a copy drawn first at the shadow
//! offset, with its own ring spread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::assets::{Assets, Handle, Texture};
use crate::canvas::atlas::{PADDING, ShelfPacker};
use crate::canvas::draw::{DrawList, Rect, SpriteInstance};
use sgl_core::math::Vec2;

/// Pixel size of one (square) glyph atlas page. Comfortably holds the 84 px
/// title alphabet; more pages open as needed. A glyph too large for it (a
/// large size at a high pixel scale) gets a square page of its own, sized to
/// it and holding nothing else, which uploads like any texture: one wider
/// than the device's `max_texture_dimension_2d` is refused by the upload
/// with `TextureError::TooLarge`. A glyph whose page would exceed
/// [`MAX_GLYPH_PAGE_SIZE`] is not drawn (its advance still applies).
pub const GLYPH_PAGE_SIZE: u32 = 512;

/// Largest glyph page side, in pixels: 16384 natively, the widest texture
/// any common GPU accepts (its page is 1 GiB of RGBA), and 8192 on wasm32,
/// WebGPU's default `max_texture_dimension_2d`, past which a page could
/// never upload. A larger glyph is neither rasterized nor packed.
#[cfg(not(target_arch = "wasm32"))]
pub const MAX_GLYPH_PAGE_SIZE: u32 = 16384;
/// Largest glyph page side, in pixels: 8192 on wasm32, WebGPU's default
/// `max_texture_dimension_2d`, past which a page could never upload (16384
/// natively). A larger glyph is neither rasterized nor packed.
#[cfg(target_arch = "wasm32")]
pub const MAX_GLYPH_PAGE_SIZE: u32 = 8192;

/// Which [`DrawList`] channel text lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextChannel {
    /// World-space (drawn through the world camera) — damage numbers etc.
    /// Text is laid out in y-down pixels (glyph advances, ascent, outline
    /// and shadow offsets are pixel offsets from the anchor), so it is
    /// correct under the default pixel camera. Under a world-unit camera
    /// (`Camera::with_units`) the sprite pass sizes each glyph quad to
    /// `px / pixels_per_unit` units, but the pixel layout offsets are still
    /// added to the world-unit anchor unconverted — the world channel is
    /// not converted for units cameras.
    World,
    /// Screen-space over the logical 960×540 view — UI/HUD (the usual case).
    Screen,
}

/// Horizontal alignment inside a rect (Godot label `horizontal_alignment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

/// Vertical alignment inside a rect (Godot label `vertical_alignment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAlign {
    Top,
    Center,
    Bottom,
}

/// Outline pass parameters (Godot `outline_size` + `font_outline_color`).
#[derive(Debug, Clone, Copy)]
pub struct Outline {
    /// Outline radius in pixels (Godot `outline_size`; PR-9: 10 for the
    /// title, 1 elsewhere). Capped at [`MAX_RING_WIDTH`]; a non-finite
    /// width draws no outline.
    pub width: f32,
    pub color: [f32; 4],
}

/// Drop-shadow pass parameters (Godot `shadow_offset` + `shadow_size`).
#[derive(Debug, Clone, Copy)]
pub struct Shadow {
    /// Pixel offset of the shadow copy (PR-9 title: `(5, 5)`).
    pub offset: Vec2,
    /// Ring spread of the shadow, like an outline width (Godot
    /// `shadow_size`; PR-9 title: 10). `0.0` = a plain offset copy. Capped
    /// at [`MAX_RING_WIDTH`]; a non-finite spread draws a plain copy.
    pub spread: f32,
    pub color: [f32; 4],
}

/// How a piece of text is drawn: size, fill color, optional outline and
/// shadow. Sizes are pixels of em (Godot font "pt" at default settings).
#[derive(Debug, Clone, Copy)]
pub struct TextStyle {
    pub px: f32,
    /// Fill color (straight-alpha sRGB, like sprite modulate).
    pub color: [f32; 4],
    pub outline: Option<Outline>,
    pub shadow: Option<Shadow>,
}

impl TextStyle {
    /// Plain text of the given pixel size and fill color.
    pub fn new(px: f32, color: [f32; 4]) -> Self {
        Self {
            px,
            color,
            outline: None,
            shadow: None,
        }
    }

    /// Add a black outline of `width` pixels (the Godot default outline
    /// color is black).
    #[must_use]
    pub fn with_outline(mut self, width: f32) -> Self {
        self.outline = Some(Outline {
            width,
            color: [0.0, 0.0, 0.0, 1.0],
        });
        self
    }

    /// Add a black drop shadow at `offset` with ring `spread` (Godot
    /// `shadow_offset` / `shadow_size`).
    #[must_use]
    pub fn with_shadow(mut self, offset: Vec2, spread: f32) -> Self {
        self.shadow = Some(Shadow {
            offset,
            spread,
            color: [0.0, 0.0, 0.0, 1.0],
        });
        self
    }
}

/// Text-layer failures (font file loading/parsing).
#[derive(Debug)]
pub enum TextError {
    /// Reading the font file failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// fontdue could not parse the font data.
    Font(&'static str),
}

impl std::fmt::Display for TextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "font read {}: {source}", path.display()),
            Self::Font(msg) => write!(f, "font parse: {msg}"),
        }
    }
}

impl std::error::Error for TextError {}

/// Cache key: character + quantized pixel size (quarter-pixel buckets — the
/// game's sizes are discrete integers, PR-8/PR-9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    c: char,
    px_q: u32,
}

/// A rasterized glyph's page placement + the metrics needed to position it.
#[derive(Debug, Clone, Copy)]
struct GlyphBitmap {
    page: usize,
    /// Placement in page pixels.
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    /// fontdue metrics: left side bearing…
    xmin: f32,
    /// …and the y-up offset of the bitmap *bottom* from the baseline.
    ymin: f32,
}

/// A cached glyph: advance always; bitmap only when it has ink (space etc.
/// advance without a quad).
#[derive(Debug, Clone, Copy)]
struct GlyphSlot {
    bitmap: Option<GlyphBitmap>,
    advance: f32,
}

/// One CPU-side atlas page: pixels + packer + the publish state.
struct GlyphPage {
    /// Width and height in pixels: [`GLYPH_PAGE_SIZE`], or an oversized
    /// glyph's larger side plus padding.
    side: u32,
    /// Holds one oversized glyph and nothing else: never packed into, so
    /// ordinary glyphs never depend on a page that may not upload.
    dedicated: bool,
    packer: ShelfPacker,
    /// RGBA8: white RGB, glyph coverage in alpha (the shader multiplies the
    /// instance color in, so one page serves every text color).
    pixels: Vec<u8>,
    dirty: bool,
    /// The page's asset handle (`None` until first publish), stable from
    /// then on: republishing replaces the asset in place.
    handle: Option<Handle<Texture>>,
    /// The last frame a glyph on this page was drawn or placed.
    last_used: u64,
}

impl GlyphPage {
    /// A page of `side` pixels square; `side` is at most
    /// [`MAX_GLYPH_PAGE_SIZE`].
    fn new(side: u32, dedicated: bool, frame: u64) -> Self {
        Self {
            side,
            dedicated,
            packer: ShelfPacker::new(side, side),
            pixels: vec![0; page_bytes(side)],
            dirty: false,
            handle: None,
            last_used: frame,
        }
    }

    /// Empty the page for reuse at `side` pixels square, keeping its kind
    /// and handle (republishing replaces the texture, resized if need be).
    fn reset(&mut self, side: u32, frame: u64) {
        if side == self.side {
            self.pixels.fill(0);
        } else {
            self.pixels = vec![0; page_bytes(side)];
            self.side = side;
        }
        self.packer = ShelfPacker::new(side, side);
        self.last_used = frame;
    }
}

/// RGBA bytes of a page `side` pixels square; `side` is at most
/// [`MAX_GLYPH_PAGE_SIZE`].
fn page_bytes(side: u32) -> usize {
    usize::try_from(u64::from(side) * u64::from(side) * 4)
        .expect("a page within MAX_GLYPH_PAGE_SIZE is addressable")
}

/// A queued glyph quad, waiting for [`TextRenderer::end_frame`] to resolve
/// its page handle.
struct Quad {
    page: usize,
    src: Rect,
    /// Quad center in logical pixels (`SpriteInstance` convention).
    center: Vec2,
    /// Instance scale: `1 / pixel_scale` at queue time, so a glyph
    /// rasterized at `s ×` draws at layout size (`1.0` on the game path —
    /// bit-identical to the pre-scale default).
    scale: f32,
    color: [f32; 4],
    z: f32,
    screen: bool,
    /// Clip rect captured from [`TextRenderer::set_clip`] at queue time
    /// (quads flush at `end_frame`, after the clip state has moved on).
    clip: Option<Rect>,
}

/// On-demand glyph rasterizer + atlas + line layout over one TTF font.
pub struct TextRenderer {
    font: fontdue::Font,
    /// Unique per-renderer id namespacing the published glyph-page asset
    /// paths — two renderers (game Risque + editor sans, D-26) must never
    /// collide on `sgl://glyph-page/...` (path collisions alias handles
    /// and scramble glyphs).
    instance: u64,
    pages: Vec<GlyphPage>,
    glyphs: HashMap<GlyphKey, GlyphSlot>,
    queued: Vec<Quad>,
    /// Current clip rect applied to newly queued quads (scrollable UI).
    clip: Option<Rect>,
    /// Physical pixels per layout unit (see [`set_pixel_scale`]).
    ///
    /// [`set_pixel_scale`]: Self::set_pixel_scale
    pixel_scale: f32,
    /// Frames completed by [`end_frame`](Self::end_frame); pages record the
    /// last one that used them.
    frame: u64,
}

impl TextRenderer {
    /// Parse a TTF from raw bytes.
    pub fn new(font_bytes: &[u8]) -> Result<Self, TextError> {
        let settings = fontdue::FontSettings {
            // Optimize glyph geometry for the largest size in use (the 84 px
            // title, PR-9); smaller sizes only get more accurate.
            scale: 84.0,
            ..fontdue::FontSettings::default()
        };
        let font = fontdue::Font::from_bytes(font_bytes, settings).map_err(TextError::Font)?;
        // A process-wide counter: usize atomics exist on every target
        // (wasm32 lacks 64-bit atomics without the threads proposal).
        static NEXT_INSTANCE: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        let instance = NEXT_INSTANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as u64;
        Ok(Self {
            font,
            instance,
            pages: Vec::new(),
            glyphs: HashMap::new(),
            queued: Vec::new(),
            clip: None,
            pixel_scale: 1.0,
            frame: 0,
        })
    }

    /// Set the physical-pixels-per-layout-unit factor. At scale `s`, glyphs
    /// rasterize at `s ×` the requested size and their quads shrink back to
    /// layout units, snapped to the `1/s` grid — so a render target `s ×`
    /// the layout space samples them 1:1 and text stays crisp at native
    /// resolution (the editor scene). The default `1.0` is bit-identical to
    /// the pre-scale behavior; game scenes never change it (parity, D-11).
    pub fn set_pixel_scale(&mut self, scale: f32) {
        self.pixel_scale = scale.max(0.25);
    }

    /// Set the clip rect applied to text queued from now on (logical view
    /// pixels; `None` = unclipped). Captured per quad — safe to change
    /// between draws within a frame (scrollable panels set it around their
    /// content and reset it after).
    pub fn set_clip(&mut self, clip: Option<Rect>) {
        self.clip = clip;
    }

    /// Read and parse a TTF file (e.g. `assets/ui/fonts/Risque-Regular.ttf`).
    pub fn load(path: &Path) -> Result<Self, TextError> {
        let bytes = std::fs::read(path).map_err(|source| TextError::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::new(&bytes)
    }

    /// Number of atlas pages opened so far (diagnostics/tests). It grows only
    /// when every page is full and was used in the current frame: otherwise
    /// the least recently used page is emptied and reused, so text whose
    /// size changes every frame keeps a bounded number of pages.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Single-line size of `text` at `px`: width = advances + kerning,
    /// height = [`line_height`](Self::line_height). Pure metrics — no
    /// rasterization. A size that is not positive and finite measures
    /// zero, matching [`draw`](Self::draw), which draws nothing at it.
    ///
    /// **Coupling with [`draw`](Self::draw)**: `draw` advances the pen with
    /// metrics fetched at `px × pixel_scale` and divided back by the scale,
    /// while this measures at `px` directly. fontdue's advances are linear
    /// in the requested size, so the two agree to floating-point rounding
    /// (sub-ULP today — layout code may treat this as `draw`'s width). If
    /// `draw`'s advance math ever stops being a pure rescale of the `px`
    /// metrics, route this through the same `raster_px / s` computation.
    pub fn measure(&self, text: &str, px: f32) -> Vec2 {
        if !drawable_px(px) {
            return Vec2::ZERO;
        }
        let mut width = 0.0;
        let mut prev: Option<char> = None;
        for c in text.chars() {
            if let Some(p) = prev
                && let Some(kern) = self.font.horizontal_kern(p, c, px)
            {
                width += kern;
            }
            width += self.font.metrics(c, px).advance_width;
            prev = Some(c);
        }
        Vec2::new(width, self.line_height(px))
    }

    /// Line height at `px`: ascent − descent (descent is negative), the box
    /// a single line occupies for vertical alignment (Godot label sizing).
    pub fn line_height(&self, px: f32) -> f32 {
        self.font
            .horizontal_line_metrics(px)
            .map_or(px, |m| m.ascent - m.descent)
    }

    /// Baseline offset from the top of the line box at `px`.
    pub fn ascent(&self, px: f32) -> f32 {
        self.font
            .horizontal_line_metrics(px)
            .map_or(px * 0.8, |m| m.ascent)
    }

    /// Queue one line of text with its top-left at `top_left` (logical
    /// pixels). Pass order per glyph run: shadow, outline rings, fill — all
    /// at `z`, relying on the `DrawList`'s stable sort (PR-2 push order).
    /// Quads reach the list at [`end_frame`](Self::end_frame). A size that
    /// is not positive and finite draws nothing.
    pub fn draw(
        &mut self,
        text: &str,
        top_left: Vec2,
        style: &TextStyle,
        z: f32,
        channel: TextChannel,
    ) {
        // Layout pass: absolute, pixel-snapped fill positions (glyphs are
        // pre-antialiased at the target size; placement snaps to the
        // physical-pixel grid — the `1/pixel_scale` layout grid — so they
        // stay crisp under the nearest-sampled 1:1 target mapping). At
        // `pixel_scale = 1` every operation below is bit-identical to the
        // pre-scale layout (multiplying/dividing by 1.0 is exact).
        let s = self.pixel_scale;
        let raster_px = style.px * s;
        if !drawable_px(raster_px) {
            return;
        }
        let snap = |v: f32| (v * s).round() / s;
        let baseline = top_left.y + self.ascent(raster_px) / s;
        let mut placed: Vec<(GlyphBitmap, Vec2)> = Vec::new();
        let mut pen = top_left.x;
        let mut prev: Option<char> = None;
        for c in text.chars() {
            if let Some(p) = prev
                && let Some(kern) = self.font.horizontal_kern(p, c, raster_px)
            {
                pen += kern / s;
            }
            let slot = self.glyph(c, raster_px);
            if let Some(b) = slot.bitmap {
                // In use this frame: never recycled before its quads flush.
                self.pages[b.page].last_used = self.frame;
                let gx = snap(pen + b.xmin / s);
                let gy = snap(baseline - b.ymin / s - b.h as f32 / s);
                placed.push((b, Vec2::new(gx, gy)));
            }
            pen += slot.advance / s;
            prev = Some(c);
        }

        // Emission passes: each queues every placed glyph at an extra offset.
        let screen = channel == TextChannel::Screen;
        let clip = self.clip;
        let queued = &mut self.queued;
        let mut emit = |offset: Vec2, color: [f32; 4]| {
            for (b, pos) in &placed {
                let size = Vec2::new(b.w as f32, b.h as f32) / s;
                queued.push(Quad {
                    page: b.page,
                    src: Rect::new(b.x as f32, b.y as f32, b.w as f32, b.h as f32),
                    center: *pos + offset + size * 0.5,
                    scale: 1.0 / s,
                    color,
                    z,
                    screen,
                    clip,
                });
            }
        };

        if let Some(shadow) = style.shadow {
            for o in ring_offsets(shadow.spread) {
                emit(shadow.offset + o, shadow.color);
            }
            emit(shadow.offset, shadow.color);
        }
        if let Some(outline) = style.outline {
            for o in ring_offsets(outline.width) {
                emit(o, outline.color);
            }
        }
        emit(Vec2::ZERO, style.color);
    }

    /// Queue one line aligned inside `rect` — how Godot labels position text
    /// (rect + h/v alignment, PR-9).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_in_rect(
        &mut self,
        text: &str,
        rect: Rect,
        h_align: HAlign,
        v_align: VAlign,
        style: &TextStyle,
        z: f32,
        channel: TextChannel,
    ) {
        let size = self.measure(text, style.px);
        let space = rect.size();
        let x = match h_align {
            HAlign::Left => rect.min.x,
            HAlign::Center => rect.min.x + (space.x - size.x) * 0.5,
            HAlign::Right => rect.max.x - size.x,
        };
        let y = match v_align {
            VAlign::Top => rect.min.y,
            VAlign::Center => rect.min.y + (space.y - size.y) * 0.5,
            VAlign::Bottom => rect.max.y - size.y,
        };
        self.draw(text, Vec2::new(x, y), style, z, channel);
    }

    /// Publish dirty pages into their texture assets and flush the queued
    /// quads into `list`. Each page keeps one asset, replaced in place, so
    /// its handle never changes. Returns the handles of the pages that
    /// changed this frame — the app must upload each
    /// (`Renderer::upload_texture` or `SpritePass::upload`, which replace an
    /// already-uploaded page's pixels under its handle) before rendering the
    /// frame. Call once per presented frame after all `draw` calls. The glyph
    /// instances it emits are valid for that frame only: pages are reused, so
    /// a retained `DrawList`, or a second `end_frame` before the first is
    /// submitted, can sample a reused page's new pixels. An upload can fail
    /// (`TextureError::TooLarge` for an oversized glyph's dedicated page on a
    /// device with a smaller texture limit): log it and carry on rather than
    /// unwrapping. A dedicated page holds one glyph and is sized to it, so
    /// only that glyph goes undrawn.
    pub fn end_frame(
        &mut self,
        assets: &mut Assets<Texture>,
        list: &mut DrawList,
    ) -> Vec<Handle<Texture>> {
        let mut changed = Vec::new();
        for (i, page) in self.pages.iter_mut().enumerate() {
            if !page.dirty {
                continue;
            }
            let path = PathBuf::from(format!("sgl://glyph-page/{}/{i}", self.instance));
            let handle = assets.insert(
                path,
                Texture {
                    width: page.side,
                    height: page.side,
                    rgba: page.pixels.clone(),
                },
            );
            page.handle = Some(handle);
            page.dirty = false;
            changed.push(handle);
        }

        for quad in self.queued.drain(..) {
            let Some(handle) = self.pages[quad.page].handle else {
                continue; // unreachable: a quad's page was dirtied at least once
            };
            let instance = SpriteInstance {
                src: Some(quad.src),
                scale: Vec2::splat(quad.scale),
                color: quad.color,
                z: quad.z,
                clip: quad.clip,
                ..SpriteInstance::new(handle, quad.center)
            };
            if quad.screen {
                list.push_screen(instance);
            } else {
                list.push(instance);
            }
        }
        self.frame += 1;
        changed
    }

    /// Fetch (or rasterize + pack) the glyph for `c` at `px`.
    fn glyph(&mut self, c: char, px: f32) -> GlyphSlot {
        let key = GlyphKey {
            c,
            px_q: (px * 4.0).round() as u32,
        };
        if let Some(slot) = self.glyphs.get(&key) {
            return *slot;
        }
        let metrics = self.font.metrics(c, px);
        // A size fontdue reports past `u32` (a negative size wraps its
        // dimensions near `usize::MAX`) is not drawn, like one past the cap.
        let dims = u32::try_from(metrics.width)
            .ok()
            .zip(u32::try_from(metrics.height).ok())
            .filter(|&(w, h)| {
                w != 0
                    && h != 0
                    && u64::from(w.max(h)) + u64::from(PADDING) <= u64::from(MAX_GLYPH_PAGE_SIZE)
            });
        let bitmap = if dims.is_none() {
            None
        } else {
            let (metrics, coverage) = self.font.rasterize(c, px);
            let (w, h) = (metrics.width as u32, metrics.height as u32);
            let (page, x, y) = self.place(w, h);
            let stride = self.pages[page].side as usize;
            let pixels = &mut self.pages[page].pixels;
            for row in 0..metrics.height {
                for col in 0..metrics.width {
                    let a = coverage[row * metrics.width + col];
                    let at = ((y as usize + row) * stride + x as usize + col) * 4;
                    // White ink, coverage in alpha (straight alpha — the
                    // sprite shader premultiplies).
                    pixels[at..at + 4].copy_from_slice(&[255, 255, 255, a]);
                }
            }
            self.pages[page].dirty = true;
            Some(GlyphBitmap {
                page,
                x,
                y,
                w,
                h,
                xmin: metrics.xmin as f32,
                ymin: metrics.ymin as f32,
            })
        };
        let slot = GlyphSlot {
            bitmap,
            advance: metrics.advance_width,
        };
        self.glyphs.insert(key, slot);
        slot
    }

    /// Find room for `w × h`, returning `(page index, x, y)` and marking the
    /// page used this frame. A glyph that fits a [`GLYPH_PAGE_SIZE`] page
    /// takes a standard page with space; else the least recently used
    /// standard page not drawn from this frame, emptied (its cached glyphs
    /// re-rasterize on next use); else a new standard page. A larger glyph
    /// takes a dedicated page of its own, never packed into again, exactly its
    /// larger side plus the packer's padding: the least recently used stale
    /// dedicated page, rebuilt at that size under its handle, or else a new
    /// one. So dedicated pages never outnumber the oversized glyphs one frame
    /// draws.
    fn place(&mut self, w: u32, h: u32) -> (usize, u32, u32) {
        let frame = self.frame;
        let need = w.max(h) + PADDING;
        let dedicated = need > GLYPH_PAGE_SIZE;
        let found = if dedicated {
            None
        } else {
            self.pages
                .iter_mut()
                .enumerate()
                .filter(|(_, page)| !page.dedicated)
                .find_map(|(i, page)| page.packer.insert(w, h).map(|(x, y)| (i, x, y)))
        };
        let (i, x, y) = found.unwrap_or_else(|| {
            let stale = self
                .pages
                .iter()
                .enumerate()
                .filter(|(_, page)| page.last_used < frame && page.dedicated == dedicated)
                .min_by_key(|(_, page)| page.last_used)
                .map(|(i, _)| i);
            let side = need.max(GLYPH_PAGE_SIZE);
            let i = if let Some(i) = stale {
                self.glyphs
                    .retain(|_, slot| slot.bitmap.is_none_or(|b| b.page != i));
                self.pages[i].reset(side, frame);
                i
            } else {
                self.pages.push(GlyphPage::new(side, dedicated, frame));
                self.pages.len() - 1
            };
            let (x, y) = self.pages[i]
                .packer
                .insert(w, h)
                .expect("an empty page one padding wider than the glyph fits it");
            (i, x, y)
        });
        self.pages[i].last_used = frame;
        (i, x, y)
    }
}

/// Widest outline or shadow spread drawn, in pixels. Each ring copies every
/// glyph quad, so the quad count grows with the square of the width: 64 px is
/// several times the widest authored outline (the 10 px title) and still
/// bounds a glyph to about 3,300 copies.
pub const MAX_RING_WIDTH: f32 = 64.0;

/// Offsets approximating a solid stroke of radius `width`: concentric rings
/// sampled every ~2 px, from `width` inward. Glyph coverage overlap fills
/// the gaps; a 1 px outline is the classic 8-direction ring. A width above
/// [`MAX_RING_WIDTH`] draws at that width; a non-finite width draws no ring.
fn ring_offsets(width: f32) -> Vec<Vec2> {
    let mut out = Vec::new();
    if !width.is_finite() {
        return out;
    }
    let mut r = width.min(MAX_RING_WIDTH);
    while r > 0.25 {
        let samples = ((std::f32::consts::TAU * r) / 2.0).ceil().max(8.0) as usize;
        for i in 0..samples {
            let a = std::f32::consts::TAU * i as f32 / samples as f32;
            out.push(Vec2::new(a.cos(), a.sin()) * r);
        }
        r -= 2.0;
    }
    out
}

/// Whether text at `px` is drawn: a size that is not positive and finite is
/// not (fontdue's metrics for a negative size are meaningless).
fn drawable_px(px: f32) -> bool {
    px > 0.0 && px.is_finite()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #255: `draw_in_rect` alignment is exact for every corner and the
    /// center against `measure`, and a 1 px outline ring is the classic
    /// eight directions at radius 1.
    #[wasm_bindgen_test(unsupported = test)]
    fn alignment_and_the_unit_outline_ring_are_exact() {
        let mut r = renderer();
        let style = TextStyle::new(16.0, [1.0; 4]);
        let text = "Hg";
        let size = r.measure(text, 16.0);
        let rect = Rect::new(100.0, 100.0, 200.0, 80.0);
        let mut assets: Assets<Texture> = Assets::new();
        let mut anchors = Vec::new();
        for (h, v, expected) in [
            (HAlign::Left, VAlign::Top, Vec2::new(100.0, 100.0)),
            (
                HAlign::Right,
                VAlign::Bottom,
                Vec2::new(300.0 - size.x, 180.0 - size.y),
            ),
            (
                HAlign::Center,
                VAlign::Center,
                Vec2::new(
                    100.0 + (200.0 - size.x) * 0.5,
                    100.0 + (80.0 - size.y) * 0.5,
                ),
            ),
        ] {
            let mut list = DrawList::new();
            r.draw_in_rect(text, rect, h, v, &style, 0.0, TextChannel::Screen);
            r.end_frame(&mut assets, &mut list);
            let mut direct = DrawList::new();
            r.draw(text, expected, &style, 0.0, TextChannel::Screen);
            r.end_frame(&mut assets, &mut direct);
            let centers = |l: &DrawList| l.screen.iter().map(|q| q.pos).collect::<Vec<_>>();
            assert_eq!(centers(&list), centers(&direct), "{h:?} {v:?}");
            anchors.push(centers(&list));
        }
        assert_ne!(anchors[0], anchors[1]);

        let ring = ring_offsets(1.0);
        assert_eq!(ring.len(), 8);
        for o in &ring {
            assert!((o.length() - 1.0).abs() < 1e-5, "{o}");
        }
        assert!(ring_offsets(0.25).is_empty());
        assert_eq!(ring_offsets(3.0).len(), ring_offsets(1.0).len() + 10);
    }

    fn renderer() -> TextRenderer {
        TextRenderer::new(include_bytes!(
            "../../tests/fixtures/IBMPlexSans-Regular.ttf"
        ))
        .expect("test font should parse")
    }

    /// `measure()` is monotone over prefixes: adding characters never shrinks
    /// the width, and adding an inked character strictly grows it.
    #[wasm_bindgen_test(unsupported = test)]
    fn measure_is_monotone_over_prefixes() {
        let tr = renderer();
        let text = "ELEMENTAL CHAOS";
        let mut last = 0.0;
        for end in 1..=text.len() {
            let w = tr.measure(&text[..end], 48.0).x;
            assert!(w >= last, "width shrank at prefix {end}: {w} < {last}");
            if !text[end - 1..end].trim().is_empty() {
                assert!(w > last, "inked char did not widen at prefix {end}");
            }
            last = w;
        }
        // Height is the size-scaled line box.
        assert!(tr.measure(text, 48.0).y > tr.measure(text, 24.0).y);
    }

    /// Bigger sizes measure wider (the same string scales up).
    #[wasm_bindgen_test(unsupported = test)]
    fn measure_scales_with_size() {
        let tr = renderer();
        let w8 = tr.measure("99", 8.0).x;
        let w24 = tr.measure("99", 24.0).x;
        let w84 = tr.measure("99", 84.0).x;
        assert!(w8 < w24 && w24 < w84);
    }

    /// Rasterizing the game's glyph load fills a page and opens a second one
    /// (dynamic growth), and every glyph stays inside page bounds.
    #[wasm_bindgen_test(unsupported = test)]
    fn glyph_atlas_grows_new_pages() {
        let mut tr = renderer();
        for px in [84.0, 72.0, 48.0] {
            for c in ('A'..='Z').chain('a'..='z').chain('0'..='9') {
                let slot = tr.glyph(c, px);
                if let Some(b) = slot.bitmap {
                    assert!(b.x + b.w <= GLYPH_PAGE_SIZE, "{c}@{px} escapes page x");
                    assert!(b.y + b.h <= GLYPH_PAGE_SIZE, "{c}@{px} escapes page y");
                }
            }
        }
        assert!(
            tr.page_count() >= 2,
            "expected atlas growth past one page, got {}",
            tr.page_count()
        );
        // The cache is stable: re-requesting returns the same placement.
        let a = tr.glyph('A', 84.0).bitmap.unwrap();
        let b = tr.glyph('A', 84.0).bitmap.unwrap();
        assert_eq!((a.page, a.x, a.y), (b.page, b.x, b.y));
    }

    /// Space advances the pen but emits no quad.
    #[wasm_bindgen_test(unsupported = test)]
    fn space_has_advance_but_no_bitmap() {
        let mut tr = renderer();
        let slot = tr.glyph(' ', 24.0);
        assert!(slot.bitmap.is_none());
        assert!(slot.advance > 0.0);
    }

    /// `end_frame` republishes a page only when new glyphs landed, and
    /// flushes quads to the requested channel.
    #[wasm_bindgen_test(unsupported = test)]
    fn end_frame_republishes_dirty_pages_and_flushes_quads() {
        let mut tr = renderer();
        let mut assets: Assets<Texture> = Assets::new();
        let mut list = DrawList::new();

        let style = TextStyle::new(24.0, [1.0; 4]);
        tr.draw("Hello", Vec2::ZERO, &style, 5.0, TextChannel::Screen);
        let first = tr.end_frame(&mut assets, &mut list);
        assert_eq!(first.len(), 1, "one dirty page after first draw");
        assert_eq!(list.screen.len(), 5, "one quad per inked glyph");
        assert!(list.world.is_empty());
        assert!(list.screen.iter().all(|i| i.z == 5.0));
        assert!(assets.get(first[0]).is_some(), "published page resolves");

        // Same glyphs again: nothing to republish, quads still flush.
        list.clear();
        tr.draw("Hello", Vec2::ZERO, &style, 0.0, TextChannel::World);
        let again = tr.end_frame(&mut assets, &mut list);
        assert!(again.is_empty(), "clean page must not republish");
        assert_eq!(list.world.len(), 5, "world channel supported");

        // New glyphs dirty the page again → republished under its handle.
        list.clear();
        tr.draw("xyz!", Vec2::ZERO, &style, 0.0, TextChannel::Screen);
        let third = tr.end_frame(&mut assets, &mut list);
        assert_eq!(third, first, "a page keeps its handle");
    }

    /// Outline and shadow multiply the quad count (ring copies + fill), and
    /// push order is shadow → outline → fill.
    #[wasm_bindgen_test(unsupported = test)]
    fn outline_and_shadow_add_ring_copies() {
        let mut tr = renderer();
        let mut assets: Assets<Texture> = Assets::new();
        let mut list = DrawList::new();

        let plain = TextStyle::new(24.0, [1.0, 1.0, 0.0, 1.0]);
        tr.draw("AB", Vec2::ZERO, &plain, 0.0, TextChannel::Screen);
        tr.end_frame(&mut assets, &mut list);
        let plain_count = list.screen.len();
        assert_eq!(plain_count, 2);

        list.clear();
        let fancy = plain
            .with_outline(1.0)
            .with_shadow(Vec2::new(5.0, 5.0), 0.0);
        tr.draw("AB", Vec2::ZERO, &fancy, 0.0, TextChannel::Screen);
        tr.end_frame(&mut assets, &mut list);
        // 8-dir outline ring + 1 shadow copy + 1 fill = 10 per glyph.
        assert_eq!(list.screen.len(), 2 * (8 + 1 + 1));
        // Push order: shadow (black) first, fill (yellow) last.
        assert_eq!(list.screen.first().unwrap().color, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(list.screen.last().unwrap().color, [1.0, 1.0, 0.0, 1.0]);
    }

    /// Rect alignment lands the line inside the rect per the PR-9 title
    /// metrics (centered in the top 216 px band).
    #[wasm_bindgen_test(unsupported = test)]
    fn draw_in_rect_centers_within_the_rect() {
        let mut tr = renderer();
        let mut assets: Assets<Texture> = Assets::new();
        let mut list = DrawList::new();

        let style = TextStyle::new(84.0, [1.0, 1.0, 0.0, 1.0]);
        let band = Rect::new(0.0, 0.0, 960.0, 216.0);
        tr.draw_in_rect(
            "ELEMENTAL CHAOS",
            band,
            HAlign::Center,
            VAlign::Center,
            &style,
            0.0,
            TextChannel::Screen,
        );
        tr.end_frame(&mut assets, &mut list);
        assert!(!list.screen.is_empty());

        // The glyph quads' collective bounds sit centered in the band.
        let (mut min_x, mut max_x) = (f32::MAX, f32::MIN);
        let (mut min_y, mut max_y) = (f32::MAX, f32::MIN);
        for i in &list.screen {
            let half = i.src.unwrap().size() * 0.5;
            min_x = min_x.min(i.pos.x - half.x);
            max_x = max_x.max(i.pos.x + half.x);
            min_y = min_y.min(i.pos.y - half.y);
            max_y = max_y.max(i.pos.y + half.y);
        }
        let center_x = (min_x + max_x) * 0.5;
        assert!(
            (center_x - 480.0).abs() < 4.0,
            "title not horizontally centered: {center_x}"
        );
        assert!(min_y > 0.0 && max_y < 216.0, "title escapes the top band");
        // 84 px text is actually large.
        assert!(max_y - min_y > 50.0, "title glyphs suspiciously small");
    }

    /// Pixel scale 1.0 (the game path, D-11) is bit-identical to the
    /// default: same quad positions, sizes, and page sources.
    #[wasm_bindgen_test(unsupported = test)]
    fn pixel_scale_one_is_bit_identical() {
        let mut assets: Assets<Texture> = Assets::new();
        let style = TextStyle::new(24.0, [1.0; 4]);

        let mut base = renderer();
        let mut base_list = DrawList::new();
        base.draw(
            "Zoom 100%",
            Vec2::new(3.7, 9.2),
            &style,
            1.0,
            TextChannel::Screen,
        );
        base.end_frame(&mut assets, &mut base_list);

        let mut scaled = renderer();
        scaled.set_pixel_scale(1.0);
        let mut scaled_list = DrawList::new();
        scaled.draw(
            "Zoom 100%",
            Vec2::new(3.7, 9.2),
            &style,
            1.0,
            TextChannel::Screen,
        );
        scaled.end_frame(&mut assets, &mut scaled_list);

        assert_eq!(base_list.screen.len(), scaled_list.screen.len());
        for (a, b) in base_list.screen.iter().zip(&scaled_list.screen) {
            assert_eq!(a.pos, b.pos, "positions must match bit-for-bit");
            assert_eq!(a.src, b.src);
            assert_eq!(a.scale, b.scale);
        }
    }

    /// Pixel scale 2.0 (the editor at devicePixelRatio 2): glyphs rasterize
    /// at twice the size while quads keep layout-unit dimensions snapped to
    /// the half-point grid — so the 2× render target samples them 1:1.
    #[wasm_bindgen_test(unsupported = test)]
    fn pixel_scale_two_rasterizes_double_and_snaps_half_grid() {
        let mut assets: Assets<Texture> = Assets::new();
        let style = TextStyle::new(24.0, [1.0; 4]);

        let mut one = renderer();
        let mut one_list = DrawList::new();
        one.draw(
            "Hg",
            Vec2::new(10.0, 10.0),
            &style,
            0.0,
            TextChannel::Screen,
        );
        one.end_frame(&mut assets, &mut one_list);

        let mut two = renderer();
        two.set_pixel_scale(2.0);
        let mut two_list = DrawList::new();
        two.draw(
            "Hg",
            Vec2::new(10.0, 10.0),
            &style,
            0.0,
            TextChannel::Screen,
        );
        two.end_frame(&mut assets, &mut two_list);

        assert_eq!(one_list.screen.len(), two_list.screen.len());
        for (a, b) in one_list.screen.iter().zip(&two_list.screen) {
            let (sa, sb) = (a.src.unwrap(), b.src.unwrap());
            // The 2× raster is roughly double the coverage box…
            assert!(
                (sb.size().x - sa.size().x * 2.0).abs() <= 2.0,
                "raster width should double: {} vs {}",
                sa.size().x,
                sb.size().x
            );
            // …but the on-screen quad stays layout-sized (bitmap / scale)…
            let quad = sb.size() * b.scale;
            assert!(
                (quad.x - sa.size().x).abs() <= 1.5 && (quad.y - sa.size().y).abs() <= 1.5,
                "quad size should stay in layout units: {quad:?} vs {sa:?}"
            );
            // …with its corner on the half-point grid (physical pixels).
            let corner = b.pos - quad * 0.5;
            for v in [corner.x, corner.y] {
                let doubled = v * 2.0;
                assert!(
                    (doubled - doubled.round()).abs() < 1e-3,
                    "corner {v} is off the 1/2-point grid"
                );
            }
        }
    }

    /// #291: a glyph larger than a standard page (160 px at pixel scale 4,
    /// so 640 px) gets a page of its own size instead of panicking, and its
    /// quad samples exactly fontdue's coverage for that glyph.
    #[wasm_bindgen_test(unsupported = test)]
    fn an_oversized_high_dpi_glyph_gets_its_own_page() {
        let mut tr = renderer();
        let mut assets: Assets<Texture> = Assets::new();
        let mut list = DrawList::new();
        tr.set_pixel_scale(4.0);
        tr.draw(
            "W",
            Vec2::ZERO,
            &TextStyle::new(160.0, [1.0; 4]),
            0.0,
            TextChannel::Screen,
        );
        tr.end_frame(&mut assets, &mut list);

        let (metrics, coverage) = renderer().font.rasterize('W', 640.0);
        assert!(metrics.width.max(metrics.height) > GLYPH_PAGE_SIZE as usize);
        let [quad] = list.screen.as_slice() else {
            panic!("one quad for one glyph, got {}", list.screen.len());
        };
        let page = assets.get(quad.texture).expect("published page");
        let src = quad.src.expect("glyph quads have a src rect");
        assert_eq!(
            (src.size().x as usize, src.size().y as usize),
            (metrics.width, metrics.height)
        );
        let (x0, y0) = (src.min.x as usize, src.min.y as usize);
        let stride = page.width as usize;
        let alpha: Vec<u8> = (0..metrics.height)
            .flat_map(|row| {
                (0..metrics.width).map(move |col| ((y0 + row) * stride + x0 + col) * 4 + 3)
            })
            .map(|at| page.rgba[at])
            .collect();
        assert_eq!(alpha, coverage);
    }

    /// #291: an oversized glyph's page holds only that glyph: standard glyphs
    /// drawn after it, enough to need a new page, all land on
    /// `GLYPH_PAGE_SIZE` pages.
    #[wasm_bindgen_test(unsupported = test)]
    fn standard_glyphs_never_share_an_oversized_page() {
        let mut tr = renderer();
        let mut assets: Assets<Texture> = Assets::new();
        let mut list = DrawList::new();
        tr.set_pixel_scale(4.0);
        let big = TextStyle::new(160.0, [1.0; 4]);
        tr.draw("W", Vec2::ZERO, &big, 0.0, TextChannel::Screen);
        tr.end_frame(&mut assets, &mut list);
        list.clear();

        tr.set_pixel_scale(1.0);
        let alphabet: String = ('A'..='Z').chain('a'..='z').chain('0'..='9').collect();
        for px in [84.0, 72.0, 48.0] {
            let style = TextStyle::new(px, [1.0; 4]);
            tr.draw(&alphabet, Vec2::ZERO, &style, 0.0, TextChannel::Screen);
        }
        tr.end_frame(&mut assets, &mut list);
        assert!(tr.page_count() >= 3, "standard glyphs needed a second page");
        for quad in &list.screen {
            let page = assets.get(quad.texture).expect("published page");
            assert_eq!(page.width, GLYPH_PAGE_SIZE);
        }
    }

    /// #291/#327: oversized text whose size shrinks or grows every frame
    /// reuses its stale dedicated page, resized to each glyph, so the page
    /// count stays bounded, and standard pages are never handed to it.
    #[wasm_bindgen_test(unsupported = test)]
    fn animated_oversized_text_reuses_dedicated_pages() {
        let mut tr = renderer();
        let mut assets: Assets<Texture> = Assets::new();
        let mut list = DrawList::new();
        tr.set_pixel_scale(4.0);
        // Sizes shrinking from 200 px (800 px raster), then growing back.
        for frame in 0..80u8 {
            let px = 200.0 - f32::from(frame.min(80 - frame));
            let style = TextStyle::new(px, [1.0; 4]);
            tr.draw("W", Vec2::ZERO, &style, 0.0, TextChannel::Screen);
            tr.end_frame(&mut assets, &mut list);
            for quad in &list.screen {
                let page = assets.get(quad.texture).expect("published page");
                let src = quad.src.expect("glyph quads have a src rect");
                assert!(page.width > GLYPH_PAGE_SIZE, "a dedicated page");
                // Sized to the glyph: its larger side plus the padding.
                let glyph_side = src.size().x.max(src.size().y) as u32;
                assert_eq!(page.width, glyph_side + PADDING);
            }
            list.clear();
            assert!(
                tr.page_count() <= 2,
                "{} pages at frame {frame}",
                tr.page_count()
            );
        }
    }

    /// #291: oversized text that grows every frame keeps one dedicated page:
    /// no stale page is ever large enough, so it is rebuilt at each size.
    #[wasm_bindgen_test(unsupported = test)]
    fn growing_oversized_text_keeps_a_bounded_page_count() {
        let mut tr = renderer();
        let mut assets: Assets<Texture> = Assets::new();
        let mut list = DrawList::new();
        tr.set_pixel_scale(4.0);
        for frame in 0..40u8 {
            let style = TextStyle::new(160.0 + f32::from(frame), [1.0; 4]);
            tr.draw("W", Vec2::ZERO, &style, 0.0, TextChannel::Screen);
            tr.end_frame(&mut assets, &mut list);
            list.clear();
            assert!(
                tr.page_count() <= 2,
                "{} pages at frame {frame}",
                tr.page_count()
            );
        }
    }

    /// #291: a glyph whose page would exceed `MAX_GLYPH_PAGE_SIZE` is neither
    /// rasterized nor packed, and still advances the pen.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_glyph_beyond_the_page_cap_is_skipped() {
        let mut tr = renderer();
        let slot = tr.glyph('W', 40_000.0);
        assert!(slot.bitmap.is_none());
        assert!(slot.advance > 0.0);
        assert_eq!(tr.page_count(), 0);
    }

    /// #429: a negative or infinite size draws nothing, measures zero and
    /// does not panic.
    #[wasm_bindgen_test(unsupported = test)]
    fn negative_and_non_finite_sizes_draw_nothing() {
        let mut tr = renderer();
        let mut assets: Assets<Texture> = Assets::new();
        for px in [-16.0, f32::NEG_INFINITY, f32::INFINITY, f32::NAN] {
            let mut list = DrawList::new();
            let style = TextStyle::new(px, [1.0; 4]);
            tr.draw("Hello", Vec2::ZERO, &style, 0.0, TextChannel::Screen);
            tr.end_frame(&mut assets, &mut list);
            assert!(list.screen.is_empty(), "{px} drew glyphs");
            assert_eq!(tr.measure("Hello", px), Vec2::ZERO, "{px}");
        }
        assert_eq!(tr.page_count(), 0);
    }

    /// The glyph pixels each queued quad samples, in push order.
    fn sampled_glyphs(assets: &Assets<Texture>, list: &DrawList) -> Vec<Vec<u8>> {
        list.screen
            .iter()
            .map(|quad| {
                let page = assets.get(quad.texture).expect("published page");
                let src = quad.src.expect("glyph quads have a src rect");
                let (x0, y0) = (src.min.x as usize, src.min.y as usize);
                let (x1, y1) = (src.max.x as usize, src.max.y as usize);
                let stride = GLYPH_PAGE_SIZE as usize * 4;
                (y0..y1)
                    .flat_map(|y| page.rgba[y * stride + x0 * 4..y * stride + x1 * 4].to_vec())
                    .collect()
            })
            .collect()
    }

    /// #327: text whose size changes every frame reuses stale pages instead
    /// of opening new ones, and a glyph whose page was reused re-rasterizes:
    /// drawn again, its quads sample the same pixels a fresh renderer gives.
    #[wasm_bindgen_test(unsupported = test)]
    fn animated_text_sizes_keep_a_bounded_page_count() {
        let mut tr = renderer();
        let mut assets: Assets<Texture> = Assets::new();
        let mut list = DrawList::new();
        // 1000 distinct quarter-pixel sizes from 16 px to about 266 px; one
        // frame's two glyphs always fit on one page.
        for frame in 0..1000u16 {
            let style = TextStyle::new(16.0 + f32::from(frame) * 0.25, [1.0; 4]);
            tr.draw("Hi", Vec2::ZERO, &style, 0.0, TextChannel::Screen);
            tr.end_frame(&mut assets, &mut list);
            list.clear();
            assert!(
                tr.page_count() <= 3,
                "{} pages at frame {frame}",
                tr.page_count()
            );
        }

        let style = TextStyle::new(16.0, [1.0; 4]);
        tr.draw("Hi", Vec2::ZERO, &style, 0.0, TextChannel::Screen);
        tr.end_frame(&mut assets, &mut list);
        let mut fresh = renderer();
        let mut fresh_assets: Assets<Texture> = Assets::new();
        let mut fresh_list = DrawList::new();
        fresh.draw("Hi", Vec2::ZERO, &style, 0.0, TextChannel::Screen);
        fresh.end_frame(&mut fresh_assets, &mut fresh_list);
        assert_eq!(
            sampled_glyphs(&assets, &list),
            sampled_glyphs(&fresh_assets, &fresh_list)
        );
    }

    /// #327: an infinite or NaN outline or shadow width draws no ring (and
    /// returns), and an enormous one draws at the cap.
    #[wasm_bindgen_test(unsupported = test)]
    fn non_finite_and_huge_ring_widths_are_bounded() {
        assert!(ring_offsets(f32::INFINITY).is_empty());
        assert!(ring_offsets(f32::NAN).is_empty());
        assert_eq!(ring_offsets(1e9).len(), ring_offsets(MAX_RING_WIDTH).len());
    }

    /// The 1 px outline ring is the classic 8 directions; a 10 px ring has
    /// multiple concentric rings.
    #[wasm_bindgen_test(unsupported = test)]
    fn ring_offsets_scale_with_width() {
        assert_eq!(ring_offsets(1.0).len(), 8);
        assert!(ring_offsets(0.0).is_empty());
        let big = ring_offsets(10.0);
        assert!(big.len() > 40, "10 px ring too sparse: {}", big.len());
        assert!(big.iter().all(|o| o.length() <= 10.001));
        assert!(big.iter().any(|o| o.length() < 5.0), "no inner ring");
    }
}
