//! Property tests for the CPU-side geometry in `sgl-2d` (client.md 2–6).
//! Each property names the defect it catches and takes its oracle from rect
//! algebra, a roundtrip identity, a brute-force check, or the GPU matrix the
//! CPU mapping must agree with — never from the code under test. Fixed seed
//! (testing.md 3); `PROPTEST_CASES` widens a local run. Native only:
//! proptest's `getrandom` does not build for `wasm32-unknown-unknown`.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::similar_names,
    clippy::too_many_lines
)]

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use sgl_2d::assets::{Assets, Handle, Texture, white_texture};
use sgl_2d::canvas::atlas::{PADDING, ShelfPacker};
use sgl_2d::canvas::text::{TextChannel, TextRenderer, TextStyle};
use sgl_2d::canvas::{
    Camera, DrawList, Letterbox, Overlay, Rect, SpriteInstance, WorldUnits, fit_fractional,
};
use sgl_core::math::{Vec2, Vec4};

const SEED: [u8; 32] = *b"sgl-client geometry props seed01";

fn check<S: Strategy>(strategy: S, test: impl Fn(S::Value) -> Result<(), TestCaseError>) {
    let config = Config {
        failure_persistence: None,
        ..Config::default()
    };
    let mut runner =
        TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &SEED));
    if let Err(failure) = runner.run(&strategy, test) {
        panic!("{failure}");
    }
}

fn finite(range: std::ops::Range<f32>) -> impl Strategy<Value = f32> {
    range.prop_filter("finite", |v| v.is_finite())
}

fn vec2(range: std::ops::Range<f32>) -> impl Strategy<Value = Vec2> {
    (finite(range.clone()), finite(range)).prop_map(|(x, y)| Vec2::new(x, y))
}

fn handle() -> Handle<Texture> {
    let mut assets: Assets<Texture> = Assets::new();
    white_texture(&mut assets)
}

/// The on-screen rect a quad covers: `src.size() × scale` around `pos` (a
/// `None` src is the white pixel's natural 1×1).
fn quad_rect(quad: &SpriteInstance) -> Rect {
    quad_rect_in(quad, WorldUnits::default())
}

/// The rect a world-channel quad covers in the camera's `units`: the
/// renderer's sprite contract, `src.size() × scale / pixels_per_unit`
/// around `pos`.
fn quad_rect_in(quad: &SpriteInstance, units: WorldUnits) -> Rect {
    let natural = quad.src.map_or(Vec2::ONE, |s| s.size());
    let size = natural * quad.scale / units.pixels_per_unit;
    Rect {
        min: quad.pos - size * 0.5,
        max: quad.pos + size * 0.5,
    }
}

/// A world-unit convention with a power-of-two `pixels_per_unit` (1 = the
/// pixel convention), so the unit→pixel scaling is exact and an oracle's
/// `ceil(size / tile)` in units cannot straddle an integer differently from
/// the pixel grid.
fn units() -> impl Strategy<Value = WorldUnits> {
    (0u32..7, any::<bool>()).prop_map(|(shift, y_up)| WorldUnits {
        pixels_per_unit: (1u32 << shift) as f32,
        y_up,
    })
}

fn same_color(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| x.to_bits() == y.to_bits())
}

fn area(rect: &Rect) -> f32 {
    let size = rect.size();
    size.x * size.y
}

fn inside(inner: &Rect, outer: &Rect, eps: f32) -> bool {
    inner.min.x >= outer.min.x - eps
        && inner.min.y >= outer.min.y - eps
        && inner.max.x <= outer.max.x + eps
        && inner.max.y <= outer.max.y + eps
}

fn interiors_overlap(a: &Rect, b: &Rect, eps: f32) -> bool {
    a.min.x < b.max.x - eps
        && a.max.x > b.min.x + eps
        && a.min.y < b.max.y - eps
        && a.max.y > b.min.y + eps
}

/// Quads inside `target` and pairwise disjoint.
fn assert_disjoint_inside(
    quads: &[SpriteInstance],
    target: &Rect,
    eps: f32,
) -> Result<f32, TestCaseError> {
    assert_disjoint_inside_in(quads, target, eps, WorldUnits::default())
}

/// [`assert_disjoint_inside`] with the quads measured in `units`.
fn assert_disjoint_inside_in(
    quads: &[SpriteInstance],
    target: &Rect,
    eps: f32,
    units: WorldUnits,
) -> Result<f32, TestCaseError> {
    let rects: Vec<Rect> = quads.iter().map(|q| quad_rect_in(q, units)).collect();
    let mut total = 0.0;
    for (i, a) in rects.iter().enumerate() {
        prop_assert!(inside(a, target, eps), "quad {a:?} escapes {target:?}");
        prop_assert!(area(a) > 0.0, "empty quad {a:?}");
        total += area(a);
        for b in &rects[i + 1..] {
            prop_assert!(!interiors_overlap(a, b, eps), "{a:?} overlaps {b:?}");
        }
    }
    Ok(total)
}

/// Quads tile `target` exactly in `units`: disjoint, inside, areas summing
/// to its area.
fn assert_tiling(
    quads: &[SpriteInstance],
    target: &Rect,
    eps: f32,
    units: WorldUnits,
) -> Result<(), TestCaseError> {
    let total = assert_disjoint_inside_in(quads, target, eps, units)?;
    let want = area(target);
    prop_assert!(
        (total - want).abs() <= want * 1e-4 + eps,
        "quads cover {total} of {want}"
    );
    Ok(())
}

// ----------------------------------------------------------------- draw

/// Defect: `ceil` vs `floor` on the last tile, a partial tile whose src is
/// scaled wrong so the visible grid shifts, or (#262) a world-unit grid laid
/// out in pixels or with its rows running the wrong way under `y_up`.
/// Oracle: rect algebra in the camera's units — the tiles cover the target
/// exactly, every src lies inside the sheet src, a partial tile samples the
/// leading part of the source, and row 0 sits at the screen top (the
/// smallest y in pixels, the largest under `y_up`).
#[test]
fn tiled_expansion_tiles_the_target_exactly_with_leading_partial_sources() {
    let strategy = (
        vec2(-300.0..300.0),
        vec2(1.0..120.0),
        vec2(1.0..40.0),
        (vec2(0.0..64.0), vec2(1.0..48.0)),
        any::<bool>(),
        any::<bool>(),
        units(),
    );
    check(
        strategy,
        |(pos, size, tile, (src_min, src_size), flip_x, flip_y, units)| {
            let src = Rect {
                min: src_min,
                max: src_min + src_size,
            };
            let template = SpriteInstance {
                flip_x,
                flip_y,
                z: 3.0,
                ..SpriteInstance::new(handle(), pos)
            };
            let mut list = DrawList::new();
            list.push_tiled(&template, src, size, tile, units);
            let target = Rect {
                min: pos - size * 0.5,
                max: pos + size * 0.5,
            };
            let cols = (size.x / tile.x).ceil() as usize;
            let rows = (size.y / tile.y).ceil() as usize;
            prop_assert_eq!(list.world.len(), cols * rows);
            assert_tiling(&list.world, &target, 1e-3, units)?;
            let top = list.world[0].pos.y;
            for quad in &list.world {
                prop_assert!(
                    if units.y_up {
                        quad.pos.y <= top + 1e-3
                    } else {
                        quad.pos.y >= top - 1e-3
                    },
                    "row 0 (y {top}) is not at the screen top of {quad:?} under {units:?}"
                );
                let s = quad.src.expect("expanded quads carry a src");
                prop_assert!(inside(&s, &src, 1e-3), "src {s:?} escapes sheet {src:?}");
                prop_assert_eq!(s.min, src.min, "partial tiles must sample the leading edge");
                prop_assert_eq!(
                    (quad.flip_x, quad.flip_y, quad.z.to_bits()),
                    (flip_x, flip_y, 3.0f32.to_bits())
                );
                // A full tile is `tile` on screen; a partial one is proportionally
                // smaller in both the quad and its source.
                let on_screen = quad_rect_in(quad, units).size();
                let frac = on_screen / tile;
                let src_frac = s.size() / src_size;
                prop_assert!(
                    (frac - src_frac).abs().max_element() < 1e-3,
                    "{frac} vs {src_frac}"
                );
            }
            Ok(())
        },
    );
}

/// Defect: a corner scaled, an edge stretched on the wrong axis, a border
/// larger than half the target producing inverted quads, or (#262) a
/// world-unit target sliced in pixels or with its top border at the wrong
/// end under `y_up`. Oracle: corners keep native scale, edges scale on one
/// axis only, the quads tile the target exactly in the camera's units while
/// the source's middle bands survive (a consumed band drops its column or
/// row by contract), nothing has negative extent, and the source's top
/// border sits at the screen top.
#[test]
fn nine_slice_keeps_native_corners_and_tiles_the_target() {
    let strategy = (
        vec2(-300.0..300.0),
        vec2(1.0..200.0),
        (vec2(0.0..64.0), vec2(2.0..64.0)),
        finite(0.0..40.0),
        units(),
    );
    check(
        strategy,
        |(pos, size, (src_min, src_size), border, units)| {
            let src = Rect {
                min: src_min,
                max: src_min + src_size,
            };
            let mut list = DrawList::new();
            list.push_nine_slice(
                &SpriteInstance::new(handle(), pos),
                src,
                size,
                border,
                units,
            );
            let target = Rect {
                min: pos - size * 0.5,
                max: pos + size * 0.5,
            };
            prop_assert!(!list.world.is_empty() && list.world.len() <= 9);
            // The border is source pixels, clamped to half the target in pixels.
            let size_px = size * units.pixels_per_unit;
            let border_x = border.min(src_size.x * 0.5).min(size_px.x * 0.5);
            let border_y = border.min(src_size.y * 0.5).min(size_px.y * 0.5);
            let middle = src_size - 2.0 * Vec2::new(border_x, border_y);
            let middle_survives = middle.x > 1e-4 && middle.y > 1e-4;
            // A sliver of source stretched over the whole target amplifies the
            // f32 cancellation in the band's size by the stretch factor, so
            // the geometric tolerance scales with it (≈ 1e-5 per unit).
            let stretch = (size / middle.max(Vec2::splat(1e-4))).max_element();
            let eps = 1e-3 + 1e-5 * stretch;
            if middle_survives {
                assert_tiling(&list.world, &target, eps, units)?;
            } else {
                assert_disjoint_inside_in(&list.world, &target, eps, units)?;
            }
            // Quad 0 samples the source's top-left; it must be the topmost band.
            let top = list.world[0].pos.y;
            for quad in &list.world {
                prop_assert!(
                    if units.y_up {
                        quad.pos.y <= top + 1e-3
                    } else {
                        quad.pos.y >= top - 1e-3
                    },
                    "top border (y {top}) is not at the screen top of {quad:?} under {units:?}"
                );
            }
            for quad in &list.world {
                let s = quad.src.expect("expanded quads carry a src");
                prop_assert!(inside(&s, &src, 1e-3));
                let on_x_border =
                    s.min.x < src.min.x + border_x - 1e-4 || s.max.x > src.max.x - border_x + 1e-4;
                let on_y_border =
                    s.min.y < src.min.y + border_y - 1e-4 || s.max.y > src.max.y - border_y + 1e-4;
                if on_x_border {
                    prop_assert!(
                        (quad.scale.x - 1.0).abs() < 1e-4,
                        "border column scaled: {quad:?}"
                    );
                }
                if on_y_border {
                    prop_assert!(
                        (quad.scale.y - 1.0).abs() < 1e-4,
                        "border row scaled: {quad:?}"
                    );
                }
            }
            Ok(())
        },
    );
}

/// Defect: an unstable sort introduced for speed, breaking Godot tree-order
/// parity for equal-z sprites. Oracle: the definition of a stable sort.
#[test]
fn draw_list_sort_is_stable_within_equal_z() {
    let strategy = prop::collection::vec((0u8..4).prop_map(f32::from), 0..40);
    check(strategy, |zs| {
        let mut list = DrawList::new();
        for (i, z) in zs.iter().enumerate() {
            list.push(SpriteInstance {
                z: *z,
                color: [i as f32, 0.0, 0.0, 1.0],
                ..SpriteInstance::new(handle(), Vec2::ZERO)
            });
            list.push_screen(SpriteInstance {
                z: -*z,
                color: [i as f32, 0.0, 0.0, 1.0],
                ..SpriteInstance::new(handle(), Vec2::ZERO)
            });
        }
        list.sort();
        for channel in [&list.world, &list.screen] {
            for pair in channel.windows(2) {
                prop_assert!(pair[0].z <= pair[1].z, "z order broken");
                if pair[0].z.to_bits() == pair[1].z.to_bits() {
                    prop_assert!(
                        pair[0].color[0] < pair[1].color[0],
                        "push order lost at equal z"
                    );
                }
            }
        }
        Ok(())
    });
}

// --------------------------------------------------------------- camera

fn camera() -> impl Strategy<Value = Camera> {
    (
        (16u32..2000, 16u32..2000),
        vec2(-2000.0..2000.0),
        finite(0.1..32.0),
        finite(0.5..64.0),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|((w, h), center, zoom, ppu, y_up, pixels)| {
            let mut camera = Camera::new(w, h);
            if !pixels {
                camera = camera.with_units(WorldUnits {
                    pixels_per_unit: ppu,
                    y_up,
                });
            }
            camera.center = center;
            camera.zoom = zoom;
            camera
        })
}

/// Defect: a y-up sign error or zoom applied on one side only, so a cursor
/// hit test lands away from where a sprite is drawn. Oracle: the GPU matrix
/// (`view_proj`) and the CPU forward map must agree, and the forward map
/// composed with the cursor map through any letterbox is the identity.
#[test]
fn camera_cpu_maps_agree_with_the_gpu_matrix_and_invert_each_other() {
    let strategy = (camera(), vec2(-3000.0..3000.0), (1u32..4000, 1u32..4000));
    check(strategy, |(camera, world, (win_w, win_h))| {
        let logical = camera.view_size();
        let screen = camera.world_to_screen(world);
        let clip = camera.view_proj() * Vec4::new(world.x, world.y, 0.0, 1.0);
        let from_gpu = Vec2::new(
            (clip.x + 1.0) * 0.5 * logical.x,
            (1.0 - clip.y) * 0.5 * logical.y,
        );
        let tol = 1e-3 * (1.0 + screen.abs().max_element());
        prop_assert!(
            (from_gpu - screen).abs().max_element() < tol,
            "gpu {from_gpu} vs cpu {screen}"
        );

        let lb = fit_fractional(win_w, win_h, logical.x as u32, logical.y as u32);
        let cursor = Vec2::new(
            lb.x + screen.x * (lb.width / logical.x),
            lb.y + screen.y * (lb.height / logical.y),
        );
        let back = camera.screen_to_world(cursor, &lb);
        let scale = camera.zoom * camera.units().pixels_per_unit;
        let tol = 1e-2 * (1.0 + world.abs().max_element()) / scale.min(1.0);
        prop_assert!(
            (back - world).abs().max_element() < tol,
            "roundtrip {world} -> {back}"
        );
        prop_assert_eq!(camera.world_to_screen(camera.center), logical * 0.5);
        Ok(())
    });
}

/// Defect: a fit that overflows the window, distorts the aspect, is not
/// centered, or leaves room on both axes. Oracle: the definition of the
/// largest centered aspect-preserving rect.
#[test]
fn letterbox_is_the_largest_centered_aspect_preserving_fit() {
    check(
        ((1u32..5000, 1u32..5000), (1u32..3000, 1u32..3000)),
        |((ww, wh), (lw, lh))| {
            let lb: Letterbox = fit_fractional(ww, wh, lw, lh);
            let (ww, wh) = (ww as f32, wh as f32);
            prop_assert!(
                lb.width <= ww + 1e-3 && lb.height <= wh + 1e-3,
                "{lb:?} overflows"
            );
            prop_assert!(lb.x >= -1e-3 && lb.y >= -1e-3);
            prop_assert!(
                (2.0 * lb.x + lb.width - ww).abs() < 1e-2,
                "not centered horizontally"
            );
            prop_assert!(
                (2.0 * lb.y + lb.height - wh).abs() < 1e-2,
                "not centered vertically"
            );
            let aspect = lw as f32 / lh as f32;
            prop_assert!(
                (lb.width / lb.height - aspect).abs() < 1e-3 * aspect,
                "aspect distorted"
            );
            prop_assert!(
                (lb.width - ww).abs() < 1e-2 || (lb.height - wh).abs() < 1e-2,
                "not maximal: {lb:?} in {ww}x{wh}"
            );
            Ok(())
        },
    );
}

// ---------------------------------------------------------------- atlas

/// Defect: a shelf that reuses space or forgets the padding, so a
/// nearest-sampled sprite bleeds its neighbour. Oracle: a pairwise check
/// that every placement stays on the page and keeps `PADDING` clear of every
/// other placement.
#[test]
fn shelf_packer_placements_stay_on_page_and_keep_their_padding() {
    let strategy = prop::collection::vec((1u32..90, 1u32..90), 1..60);
    check(strategy, |rects| {
        let (page_w, page_h) = (256, 256);
        let mut packer = ShelfPacker::new(page_w, page_h);
        let mut placed: Vec<(u32, u32, u32, u32)> = Vec::new();
        for (w, h) in rects {
            if let Some((x, y)) = packer.insert(w, h) {
                prop_assert!(
                    x + w <= page_w && y + h <= page_h,
                    "({x},{y}) {w}x{h} escapes"
                );
                for &(px, py, pw, ph) in &placed {
                    let clear = x >= px + pw + PADDING
                        || px >= x + w + PADDING
                        || y >= py + ph + PADDING
                        || py >= y + h + PADDING;
                    prop_assert!(clear, "({x},{y}) {w}x{h} touches ({px},{py}) {pw}x{ph}");
                }
                placed.push((x, y, w, h));
            }
        }
        prop_assert!(packer.insert(page_w + 1, 1).is_none());
        prop_assert!(packer.insert(1, page_h + 1).is_none());
        Ok(())
    });
}

// -------------------------------------------------------------- overlay

/// Defect: thickness applied twice, an outline spilling past its rect, or
/// a circle whose sides do not sit on the circumference. Oracle: bounds
/// containment, the exact outline area, and chord geometry.
#[test]
fn overlay_primitives_stay_within_their_bounds() {
    let strategy = (
        vec2(-200.0..200.0),
        vec2(-200.0..200.0),
        finite(0.0..6.0),
        (vec2(-200.0..200.0), vec2(1.0..80.0)),
        (finite(1.0..60.0), 3u32..24),
    );
    check(
        strategy,
        |(a, b, width, (rect_min, rect_size), (radius, segments))| {
            let overlay = Overlay {
                z: 5.0,
                ..Overlay::new(handle())
            };
            let w = width.max(1.0);

            // Line: one quad, every corner within the segment's box grown by the
            // half width (plus the 1 px snap axis-aligned lines are allowed).
            let mut out = Vec::new();
            overlay.line(&mut out, a, b, width);
            if a == b {
                prop_assert!(out.is_empty());
            } else {
                prop_assert_eq!(out.len(), 1);
                let q = &out[0];
                let half = quad_rect(q).size() * 0.5;
                let corners = [
                    Vec2::new(-half.x, -half.y),
                    Vec2::new(half.x, -half.y),
                    Vec2::new(half.x, half.y),
                    Vec2::new(-half.x, half.y),
                ];
                let bounds = Rect {
                    min: a.min(b) - Vec2::splat(w * 0.5 + 1.0),
                    max: a.max(b) + Vec2::splat(w * 0.5 + 1.0),
                };
                for c in corners {
                    let p = q.pos + Vec2::from_angle(q.rot).rotate(c);
                    prop_assert!(
                        p.x >= bounds.min.x - 1e-2
                            && p.x <= bounds.max.x + 1e-2
                            && p.y >= bounds.min.y - 1e-2
                            && p.y <= bounds.max.y + 1e-2,
                        "line corner {p} outside {bounds:?}"
                    );
                }
            }

            // Rect outline: four strips inside the rect, disjoint, covering
            // exactly the frame area.
            let rect = Rect {
                min: rect_min,
                max: rect_min + rect_size,
            };
            let mut out = Vec::new();
            overlay.rect_outline(&mut out, rect, width);
            let total = assert_disjoint_inside(&out, &rect, 1e-3)?;
            let wx = w.min(rect_size.x * 0.5);
            let wy = w.min(rect_size.y * 0.5);
            let frame = area(&rect) - (rect_size.x - 2.0 * wx) * (rect_size.y - 2.0 * wy);
            prop_assert!(
                (total - frame).abs() < 1e-2 * (1.0 + frame),
                "outline area {total} vs {frame}"
            );

            // Circle: `segments` chords of equal length whose midpoints sit at the
            // polygon's apothem from the center.
            let mut out = Vec::new();
            overlay.circle(&mut out, a, radius, segments, width);
            prop_assert_eq!(out.len(), segments as usize);
            let step = std::f32::consts::TAU / segments as f32;
            let chord = 2.0 * radius * (step * 0.5).sin();
            let apothem = radius * (step * 0.5).cos();
            for q in &out {
                // The side runs along the quad's x axis when rotated and along
                // either axis when snapped, so take the closer of the two.
                let size = quad_rect(q).size();
                let len = if (size.x - chord).abs() <= (size.y - chord).abs() {
                    size.x
                } else {
                    size.y
                };
                prop_assert!(
                    (len - chord).abs() < 1e-2 * (1.0 + chord),
                    "chord {len} vs {chord}"
                );
                let dist = (q.pos - a).length();
                prop_assert!(
                    (dist - apothem).abs() < 1e-2 * (1.0 + apothem) + 1.0,
                    "midpoint at {dist}, apothem {apothem}"
                );
            }
            Ok(())
        },
    );
}

// ----------------------------------------------------------------- text

/// Defect: `measure` and `draw` disagreeing on advances or kerning, glyphs
/// escaping the line box, or outline and shadow copies drifting from the
/// fill. Oracle: the fill quads lie inside the measured box (to the pixel
/// snap and glyph bearing), every outline quad is a fill quad displaced by
/// at most its width, and every shadow quad by its offset plus spread.
#[test]
fn text_quads_fit_the_measured_box_and_decorations_are_displaced_fills() {
    let strategy = (
        "[ -~]{1,12}",
        (8u32..48).prop_map(|px| px as f32),
        vec2(0.0..500.0),
        prop::option::of(finite(0.5..3.0)),
        prop::option::of((vec2(-3.0..3.0), finite(0.0..2.0))),
    );
    check(strategy, |(text, px, top_left, outline, shadow)| {
        let mut renderer = TextRenderer::new(include_bytes!("fixtures/IBMPlexSans-Regular.ttf"))
            .expect("fixture font");
        let mut style = TextStyle::new(px, [1.0, 0.5, 0.25, 1.0]);
        if let Some(width) = outline {
            style = style.with_outline(width);
        }
        if let Some((offset, spread)) = shadow {
            style = style.with_shadow(offset, spread);
        }
        renderer.draw(&text, top_left, &style, 0.0, TextChannel::Screen);
        let mut assets: Assets<Texture> = Assets::new();
        let mut list = DrawList::new();
        renderer.end_frame(&mut assets, &mut list);
        let measured = renderer.measure(&text, px);
        let line_box = Rect {
            min: top_left,
            max: top_left + measured,
        };
        // Glyph bitmaps overhang their advances by a bearing and the box by a
        // pixel of snap; `margin` bounds that independently of the renderer.
        let margin = 1.5 + px * 0.15;
        let fills: Vec<&SpriteInstance> = list
            .screen
            .iter()
            .filter(|q| same_color(q.color, style.color))
            .collect();
        prop_assert!(
            !fills.is_empty() || text.trim().is_empty(),
            "no fill quads for {text:?}"
        );
        for fill in &fills {
            prop_assert!(
                inside(&quad_rect(fill), &line_box, margin),
                "glyph {:?} escapes {line_box:?} (margin {margin})",
                quad_rect(fill)
            );
        }
        let displaced = |q: &SpriteInstance, offset: Vec2, slack: f32| {
            fills.iter().any(|f| {
                (q.pos - f.pos - offset).length() <= slack + 1e-3
                    && q.src == f.src
                    && q.scale == f.scale
            })
        };
        for q in list
            .screen
            .iter()
            .filter(|q| !same_color(q.color, style.color))
        {
            let ok_outline = outline.is_some_and(|width| {
                same_color(q.color, [0.0, 0.0, 0.0, 1.0])
                    && displaced(q, Vec2::ZERO, width * std::f32::consts::SQRT_2)
            });
            let ok_shadow = shadow.is_some_and(|(offset, spread)| {
                displaced(q, offset, spread * std::f32::consts::SQRT_2)
            });
            prop_assert!(
                ok_outline || ok_shadow,
                "decoration quad {q:?} is not a displaced fill"
            );
        }
        Ok(())
    });
}
