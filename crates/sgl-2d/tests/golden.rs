//! Golden frames: fixed scenes rendered headless and compared with committed
//! reference PNGs (rendering.md, D-8). References come from this repo's
//! macOS/Metal host; other backends may rasterize an edge differently, so a
//! small per-channel tolerance over a small fraction of pixels is allowed.
//! Scenes keep integer-aligned edges and nearest sampling to stay portable.
//!
//! `SGL_UPDATE_GOLDEN=1` rewrites the references; a deliberate change to one
//! needs a `specs/decisions.md` entry (testing.md 4). On a mismatch the
//! actual and diff images are written to the temp dir and named in the panic.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::path::{Path, PathBuf};

use sgl_2d::assets::{Assets, Handle, Texture};
use sgl_2d::canvas::text::{TextChannel, TextRenderer, TextStyle};
use sgl_2d::canvas::{
    Camera, DrawList, Gpu, LightFrame, LightingSpace, Overlay, PointLight, Rect, Renderer,
    SpriteInstance, WorldUnits,
};
use sgl_core::math::Vec2;

const SIZE: u32 = 64;
const RED: [u8; 4] = [220, 40, 40, 255];
const BLUE: [u8; 4] = [40, 60, 220, 255];

/// A headless device, or `None` after printing why. `SGL_REQUIRE_GPU` turns
/// the skip into a failure (see `canvas::test_gpu`).
fn gpu() -> Option<Gpu> {
    match Gpu::headless() {
        Ok(gpu) => Some(gpu),
        Err(err) => {
            let required =
                std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "GPU test cannot run but SGL_REQUIRE_GPU is set: {err}"
            );
            eprintln!("skipping GPU test: {err}");
            None
        }
    }
}

fn checker(size: u32, cell: u32, a: [u8; 4], b: [u8; 4]) -> Texture {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let odd = ((x / cell) + (y / cell)) % 2 == 1;
            rgba.extend_from_slice(if odd { &b } else { &a });
        }
    }
    Texture {
        width: size,
        height: size,
        rgba,
    }
}

/// A white cookie whose alpha falls off linearly to the edge.
fn radial(size: u32) -> Texture {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    let r = size as f32 / 2.0;
    for y in 0..size {
        for x in 0..size {
            let d = Vec2::new(x as f32 + 0.5 - r, y as f32 + 0.5 - r).length();
            let a = ((1.0 - d / r).clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba.extend_from_slice(&[255, 255, 255, a]);
        }
    }
    Texture {
        width: size,
        height: size,
        rgba,
    }
}

struct Rig {
    gpu: Gpu,
    renderer: Renderer,
    assets: Assets<Texture>,
    white: Handle<Texture>,
    camera: Camera,
}

impl Rig {
    fn new(space: LightingSpace) -> Option<Self> {
        let gpu = gpu()?;
        let mut renderer = Renderer::headless(&gpu, SIZE, SIZE, [0.1, 0.1, 0.12], space);
        let mut assets = Assets::new();
        let white = renderer.white_texture(&gpu, &mut assets);
        Some(Self {
            gpu,
            renderer,
            assets,
            white,
            camera: Camera::new(SIZE, SIZE),
        })
    }

    fn texture(&mut self, name: &str, tex: Texture) -> Handle<Texture> {
        let handle = self.assets.insert(PathBuf::from(name), tex);
        self.renderer
            .upload_texture(&self.gpu, handle, self.assets.get(handle).unwrap())
            .unwrap();
        handle
    }

    fn render(&mut self, list: &mut DrawList, lighting: &LightFrame) -> Vec<u8> {
        self.renderer
            .render_scene(&self.gpu, list, &self.camera, lighting);
        self.renderer.read_scene(&self.gpu).expect("scene readback")
    }
}

/// Compare `actual` with `tests/golden/<name>.png`, or rewrite it under
/// `SGL_UPDATE_GOLDEN=1`.
fn check(name: &str, actual: &[u8]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/{name}.png"));
    if std::env::var("SGL_UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        image::save_buffer(&path, actual, SIZE, SIZE, image::ExtendedColorType::Rgba8)
            .expect("write golden");
        return;
    }
    let expected = image::open(&path)
        .unwrap_or_else(|e| {
            panic!(
                "missing golden {}: {e} (SGL_UPDATE_GOLDEN=1 writes it)",
                path.display()
            )
        })
        .into_rgba8();
    assert_eq!(
        expected.dimensions(),
        (SIZE, SIZE),
        "golden {name} has the wrong size"
    );
    let expected = expected.as_raw();

    let mut differing = 0usize;
    let mut max_delta = 0u8;
    let mut diff = Vec::with_capacity(actual.len());
    for (a, e) in actual.chunks_exact(4).zip(expected.chunks_exact(4)) {
        let delta = (0..3).map(|c| a[c].abs_diff(e[c])).max().unwrap_or(0);
        if delta > 0 {
            differing += 1;
            max_delta = max_delta.max(delta);
        }
        diff.extend_from_slice(&[delta.saturating_mul(32), 0, 0, 255]);
    }
    // Backend rasterization slack: a couple of codes on at most 0.5 % of pixels.
    let budget = (SIZE * SIZE) as usize / 200;
    if max_delta > 2 || differing > budget {
        let dir = std::env::temp_dir();
        let actual_path = dir.join(format!("sgl-golden-{name}-actual.png"));
        let diff_path = dir.join(format!("sgl-golden-{name}-diff.png"));
        let _ = image::save_buffer(
            &actual_path,
            actual,
            SIZE,
            SIZE,
            image::ExtendedColorType::Rgba8,
        );
        let _ = image::save_buffer(
            &diff_path,
            &diff,
            SIZE,
            SIZE,
            image::ExtendedColorType::Rgba8,
        );
        panic!(
            "golden {name}: {differing} pixels differ (max delta {max_delta}); actual {} diff {}",
            actual_path.display(),
            diff_path.display()
        );
    }
}

/// Sprite batching, flips, modulate, scale, z order (stable across pushes),
/// tiled and nine-slice expansion, and screen-channel overlay primitives.
#[test]
fn sprites_tiles_slices_and_overlay_match_the_reference() {
    let Some(mut rig) = Rig::new(LightingSpace::Gamma) else {
        return;
    };
    let checker = rig.texture("checker", checker(16, 4, RED, BLUE));
    let white = rig.white;
    let mut list = DrawList::new();
    list.push(SpriteInstance::new(checker, Vec2::new(16.0, 16.0)));
    list.push(SpriteInstance {
        flip_x: true,
        color: [0.5, 1.0, 0.5, 1.0],
        ..SpriteInstance::new(checker, Vec2::new(48.0, 16.0))
    });
    list.push(SpriteInstance {
        scale: Vec2::splat(2.0),
        ..SpriteInstance::new(checker, Vec2::new(16.0, 48.0))
    });
    // Pushed after the scaled checker but sorted under it by z.
    list.push(SpriteInstance {
        scale: Vec2::splat(12.0),
        color: [0.0, 1.0, 1.0, 1.0],
        z: -1.0,
        ..SpriteInstance::new(white, Vec2::new(30.0, 40.0))
    });
    // Pushed after, drawn over.
    list.push(SpriteInstance {
        scale: Vec2::splat(8.0),
        color: [1.0, 1.0, 0.0, 1.0],
        z: 1.0,
        ..SpriteInstance::new(white, Vec2::new(22.0, 42.0))
    });
    list.push_tiled(
        &SpriteInstance::new(checker, Vec2::new(52.0, 52.0)),
        Rect::new(0.0, 0.0, 8.0, 8.0),
        Vec2::splat(20.0),
        Vec2::splat(8.0),
        WorldUnits::default(),
    );
    list.push_nine_slice(
        &SpriteInstance {
            z: 0.5,
            ..SpriteInstance::new(checker, Vec2::new(46.0, 32.0))
        },
        Rect::new(0.0, 0.0, 16.0, 16.0),
        Vec2::new(16.0, 24.0),
        4.0,
        WorldUnits::default(),
    );
    let overlay = Overlay {
        z: 10.0,
        ..Overlay::new(white)
    };
    overlay.line(
        &mut list.screen,
        Vec2::new(2.0, 62.0),
        Vec2::new(30.0, 34.0),
        2.0,
    );
    overlay.rect_outline(&mut list.screen, Rect::new(1.0, 1.0, 62.0, 62.0), 1.0);
    Overlay {
        color: [1.0, 0.0, 1.0, 0.5],
        z: 11.0,
        ..Overlay::new(white)
    }
    .circle(&mut list.screen, Vec2::new(32.0, 32.0), 12.0, 16, 1.0);

    let pixels = rig.render(&mut list, &LightFrame::default());
    check("sprites", &pixels);
}

/// Glyph rasterization, outline and shadow rings, and world-channel text.
#[test]
fn text_with_outline_and_shadow_matches_the_reference() {
    let Some(mut rig) = Rig::new(LightingSpace::Gamma) else {
        return;
    };
    let mut text = TextRenderer::new(include_bytes!("fixtures/IBMPlexSans-Regular.ttf"))
        .expect("fixture font");
    let mut list = DrawList::new();
    let heading = TextStyle::new(20.0, [1.0, 0.9, 0.2, 1.0])
        .with_outline(1.0)
        .with_shadow(Vec2::new(2.0, 2.0), 1.0);
    text.draw(
        "SGL 42",
        Vec2::new(2.0, 6.0),
        &heading,
        0.0,
        TextChannel::Screen,
    );
    text.draw(
        "wasm",
        Vec2::new(6.0, 36.0),
        &TextStyle::new(12.0, [0.6, 0.8, 1.0, 1.0]),
        0.0,
        TextChannel::World,
    );
    for page in text.end_frame(&mut rig.assets, &mut list) {
        rig.renderer
            .upload_texture(&rig.gpu, page, rig.assets.get(page).unwrap())
            .unwrap();
    }
    let pixels = rig.render(&mut list, &LightFrame::default());
    check("text", &pixels);
}

fn lit_scene(space: LightingSpace, name: &str) {
    let Some(mut rig) = Rig::new(space) else {
        return;
    };
    let checker = rig.texture("checker", checker(16, 4, RED, BLUE));
    let cookie = rig.assets.insert(PathBuf::from("cookie"), radial(32));
    rig.renderer
        .upload_light_cookie(&rig.gpu, cookie, rig.assets.get(cookie).unwrap())
        .unwrap();
    let white = rig.white;
    let mut list = DrawList::new();
    list.push(SpriteInstance {
        scale: Vec2::splat(SIZE as f32),
        color: [0.7, 0.7, 0.7, 1.0],
        ..SpriteInstance::new(white, Vec2::new(32.0, 32.0))
    });
    list.push(SpriteInstance {
        z: 1.0,
        ..SpriteInstance::new(checker, Vec2::new(32.0, 52.0))
    });
    let lighting = LightFrame {
        canvas_modulate: [0.15, 0.15, 0.2, 1.0],
        lights: vec![
            PointLight {
                color: [1.0, 0.8, 0.5],
                energy: 1.2,
                shadows: true,
                ..PointLight::analytic(Vec2::new(18.0, 20.0), 26.0, 1.0)
            },
            PointLight {
                color: [0.5, 0.7, 1.0],
                texture_scale: 1.5,
                ..PointLight::new(cookie, Vec2::new(46.0, 24.0))
            },
        ],
        occluders: vec![vec![
            Vec2::new(26.0, 30.0),
            Vec2::new(34.0, 30.0),
            Vec2::new(34.0, 38.0),
            Vec2::new(26.0, 38.0),
        ]],
    };
    let pixels = rig.render(&mut list, &lighting);
    check(name, &pixels);
}

/// D-8 default: gamma-space lighting, an analytic light with an occluder
/// shadow, a cookie light, and a dim canvas modulate.
#[test]
fn gamma_lighting_matches_the_reference() {
    lit_scene(LightingSpace::Gamma, "lit_gamma");
}

/// D-8 opt-in: the same scene lit in linear space and encoded once.
#[test]
fn linear_lighting_matches_the_reference() {
    lit_scene(LightingSpace::Linear, "lit_linear");
}
