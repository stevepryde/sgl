//! Real GPU pixels for compact controls and bounded focus tooltips.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::too_many_lines, clippy::float_cmp)]
use sgl_2d::assets::{Assets, Texture};
use sgl_2d::canvas::text::TextRenderer;
use sgl_2d::canvas::{Camera, DrawList, Gpu, LightFrame, LightingSpace, Rect, Renderer};
use sgl_2d::ui::{IconButton, Splitter, SplitterAxis, Ui, UiInput, UiKey};
use sgl_core::math::Vec2;

#[test]
fn focus_disabled_selection_and_wrapped_tooltips_render_inside_viewport() {
    let gpu = match Gpu::headless() {
        Ok(gpu) => gpu,
        Err(error) => {
            assert!(
                !std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0"),
                "required GPU: {error}"
            );
            eprintln!("skipping GPU UI test: {error}");
            return;
        }
    };
    let mut renderer = Renderer::headless(&gpu, 128, 128, [0.0; 3], LightingSpace::Gamma);
    let mut assets = Assets::<Texture>::new();
    let mut ui = Ui::new(&mut assets);
    renderer
        .upload_texture(
            &gpu,
            ui.white_texture(),
            assets.get(ui.white_texture()).unwrap(),
        )
        .unwrap();
    let mut text = TextRenderer::new(include_bytes!("fixtures/IBMPlexSans-Regular.ttf")).unwrap();
    let camera = Camera::new(128, 128);
    let control = Rect::new(90.0, 90.0, 24.0, 24.0);
    let viewport = Rect::new(16.0, 16.0, 100.0, 100.0);
    let mut extent = 30.0;
    for (step, x) in [32.0, 82.0, 110.0].into_iter().enumerate() {
        if step == 2 {
            ui.cancel_interactions();
        }
        let mut list = DrawList::new();
        let mut frame = ui.begin(
            &mut text,
            &mut list,
            UiInput {
                mouse_pos: Vec2::new(x, 30.0),
                mouse_down: true,
                mouse_pressed: step == 0,
                ..UiInput::default()
            },
        );
        frame.splitter(
            "pane",
            Rect::new(extent, 0.0, 4.0, 128.0),
            &mut extent,
            Splitter {
                axis: SplitterAxis::Horizontal,
                min: 20.0,
                max: 100.0,
            },
        );
        frame.rect(Rect::new(0.0, 0.0, extent, 128.0), [1.0, 1.0, 1.0, 1.0]);
        frame.end();
        renderer.render_scene(&gpu, &mut list, &camera, &LightFrame::default());
        let pixels = renderer.read_scene(&gpu).unwrap();
        let sample = &pixels[(30 * 128 + 60) * 4..(30 * 128 + 60) * 4 + 3];
        assert_eq!(
            sample,
            if step == 0 {
                &[0, 0, 0]
            } else {
                &[255, 255, 255]
            }
        );
        assert_eq!(
            extent,
            if step == 0 { 30.0 } else { 80.0 },
            "focus loss must stop resizing despite the held pointer"
        );
    }
    ui.cancel_interactions();
    let mut frames = Vec::new();
    for step in 0..4 {
        let mut list = DrawList::new();
        let input = if step == 1 {
            UiInput {
                keys: vec![UiKey::Tab],
                ..UiInput::default()
            }
        } else {
            UiInput::default()
        };
        let mut frame = ui.begin(&mut text, &mut list, input);
        assert!(!frame.icon_button(
            "lock",
            control,
            "X",
            12.0,
            IconButton {
                disabled: step >= 2,
                selected: step == 3
            }
        ));
        if step >= 2 {
            frame.tooltip_for(
                "lock",
                control,
                viewport,
                "Unavailable until the layer is unlocked",
                12.0,
            );
        }
        frame.end();
        for page in text.end_frame(&mut assets, &mut list) {
            renderer
                .upload_texture(&gpu, page, assets.get(page).unwrap())
                .unwrap();
        }
        renderer.render_scene(&gpu, &mut list, &camera, &LightFrame::default());
        frames.push(renderer.read_scene(&gpu).unwrap());
    }
    let pixel = |frame: usize, x: usize, y: usize| -> &[u8] {
        &frames[frame][(y * 128 + x) * 4..(y * 128 + x) * 4 + 3]
    };
    // Keyboard focus produces an observable outline; selection adds geometry
    // inside that outline, even though disabled buttons cannot activate.
    assert_ne!(pixel(0, 89, 100), pixel(1, 89, 100));
    assert_ne!(pixel(2, 92, 100), pixel(3, 92, 100));
    // The lower-right anchor moves wrapped help above itself. Rasterized glyph
    // pixels occur on multiple rows, and no draw escapes the supplied viewport.
    for y in 0..128 {
        for x in 0..128 {
            if !(16..116).contains(&x) || !(16..116).contains(&y) {
                assert_eq!(pixel(2, x, y), &[0, 0, 0], "escaped viewport at {x},{y}");
            }
        }
    }
    let bright_rows = (16..86)
        .filter(|&y| (24..108).any(|x| pixel(2, x, y).iter().all(|&channel| channel > 140)))
        .count();
    assert!(
        bright_rows > 15,
        "help text must visibly wrap across multiple lines"
    );
}

#[test]
fn opaque_tool_chrome_keeps_text_and_focus_legible_over_bright_and_dark_scenes() {
    let gpu = match Gpu::headless() {
        Ok(gpu) => gpu,
        Err(error) => {
            assert!(
                !std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0"),
                "required GPU: {error}"
            );
            eprintln!("skipping GPU UI test: {error}");
            return;
        }
    };
    let mut renderer = Renderer::headless(&gpu, 256, 128, [0.0; 3], LightingSpace::Gamma);
    let mut assets = Assets::<Texture>::new();
    let mut ui = Ui::new(&mut assets);
    renderer
        .upload_texture(
            &gpu,
            ui.white_texture(),
            assets.get(ui.white_texture()).unwrap(),
        )
        .unwrap();
    let mut text = TextRenderer::new(include_bytes!("fixtures/IBMPlexSans-Regular.ttf")).unwrap();
    let camera = Camera::new(256, 128);
    let pixel = |pixels: &[u8], x: usize, y: usize| -> [u8; 3] {
        pixels[(y * 256 + x) * 4..(y * 256 + x) * 4 + 3]
            .try_into()
            .unwrap()
    };
    for (theme, light) in [
        (sgl_2d::ui::UiTheme::tools_dark(), false),
        (sgl_2d::ui::UiTheme::tools_light(), true),
    ] {
        ui.set_theme(theme);
        let mut scenes = Vec::new();
        for background in [[0.02, 0.01, 0.03, 1.0], [1.0, 0.95, 0.8, 1.0]] {
            ui.cancel_interactions();
            ui.clear_focus();
            let mut unfocused = Vec::new();
            for focused in [false, true] {
                let mut list = DrawList::new();
                let mut frame = ui.begin(
                    &mut text,
                    &mut list,
                    UiInput {
                        keys: if focused { vec![UiKey::Tab] } else { vec![] },
                        ..UiInput::default()
                    },
                );
                frame.rect(Rect::new(0.0, 0.0, 256.0, 128.0), background);
                frame.panel(Rect::new(12.0, 12.0, 220.0, 100.0));
                frame.button(
                    "save",
                    Rect::new(24.0, 32.0, 160.0, 40.0),
                    "Save layer",
                    18.0,
                );
                frame.icon_button(
                    "disabled",
                    Rect::new(196.0, 32.0, 24.0, 40.0),
                    "X",
                    16.0,
                    IconButton {
                        disabled: true,
                        selected: false,
                    },
                );
                frame.end();
                for page in text.end_frame(&mut assets, &mut list) {
                    renderer
                        .upload_texture(&gpu, page, assets.get(page).unwrap())
                        .unwrap();
                }
                renderer.render_scene(&gpu, &mut list, &camera, &LightFrame::default());
                let pixels = renderer.read_scene(&gpu).unwrap();
                if focused {
                    assert_ne!(
                        pixel(&unfocused, 23, 50),
                        pixel(&pixels, 23, 50),
                        "keyboard focus must add a visible border"
                    );
                    let border = pixel(&pixels, 23, 50);
                    let panel = pixel(&pixels, 20, 50);
                    assert!(
                        border
                            .iter()
                            .zip(panel)
                            .map(|(&a, b)| u16::from(a.abs_diff(b)))
                            .sum::<u16>()
                            > 100,
                        "focus border must contrast with the tool panel"
                    );
                    let glyph_pixels = (38..66)
                        .flat_map(|y| (32..175).map(move |x| (x, y)))
                        .filter(|&(x, y)| {
                            let p = pixel(&pixels, x, y);
                            if light {
                                p.iter().all(|&v| v < 80)
                            } else {
                                p.iter().all(|&v| v > 190)
                            }
                        })
                        .count();
                    assert!(
                        glyph_pixels > 80,
                        "control label must have readable rasterized glyphs"
                    );
                    scenes.push(pixels);
                } else {
                    unfocused = pixels;
                }
            }
        }
        assert_ne!(
            pixel(&scenes[0], 4, 4),
            pixel(&scenes[1], 4, 4),
            "the scene really changed"
        );
        for y in 12..112 {
            for x in 12..232 {
                assert_eq!(
                    pixel(&scenes[0], x, y),
                    pixel(&scenes[1], x, y),
                    "scene leaked through tool chrome at {x},{y}"
                );
            }
        }
    }
}
