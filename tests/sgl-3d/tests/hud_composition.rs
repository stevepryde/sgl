//! An `sgl-2d` HUD over an SGL3D view on one device (sgl#925, sgl#926):
//! the canvas's device is requested with SGL3D's `graphics_device` helpers,
//! the view renders into the canvas target, and `render_screen` draws the
//! HUD over it.
//!
//! Runs alone by name: `cargo test -p sgl-3d-tests --test hud_composition`.
#![cfg(not(target_arch = "wasm32"))]
use sgl_2d::assets::Assets;
use sgl_2d::canvas::{DrawList, Gpu, LightingSpace, Overlay, Rect, blit::SCENE_FORMAT};
use sgl_3d::glam::{Mat4, Vec3};
use sgl_3d::settings::{RenderPreset, Settings};
use sgl_3d::{Backdrop, Camera, FrameInput, Scene, View, ViewFrame, ViewOutput, perspective};

const CANVAS: (u32, u32) = (128, 72);
/// The HUD fill's colour, opaque green.
const HUD: [u8; 4] = [0, 255, 0, 255];
/// Columns 16..56, rows 8..28.
const HUD_RECT: (std::ops::Range<usize>, std::ops::Range<usize>) = (16..56, 8..28);

/// The canvas's device, requested with SGL3D's limits and features, or
/// `None` after printing why. `SGL_REQUIRE_GPU` turns the skip into a
/// failure.
fn shared_gpu() -> Option<Gpu> {
    let required = std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    match Gpu::headless_with(|adapter| wgpu::DeviceDescriptor {
        required_features: sgl_3d::graphics_device::features(adapter),
        required_limits: sgl_3d::graphics_device::limits(adapter),
        ..Default::default()
    }) {
        Ok(gpu) => Some(gpu),
        Err(error) => {
            assert!(
                !required,
                "GPU test cannot run but SGL_REQUIRE_GPU is set: {error}"
            );
            eprintln!("skipping GPU test: {error}");
            None
        }
    }
}

/// The SGL3D picture reaches the canvas, and the HUD drawn over it changes
/// only its own rectangle: inside it every pixel is the HUD colour, outside
/// it every pixel is what the canvas held before the HUD.
#[test]
fn hud_draws_over_an_sgl3d_view_on_one_device() {
    let Some(gpu) = shared_gpu() else { return };
    let (device, queue) = (&gpu.device, &gpu.queue);
    let mut canvas = sgl_2d::canvas::Renderer::headless(
        &gpu,
        CANVAS.0,
        CANVAS.1,
        [0.0; 3],
        LightingSpace::Gamma,
    );

    // The cheapest picture: an empty scene's backdrop in one colour.
    let size = [CANVAS.0, CANVAS.1];
    let settings = Settings {
        preset: RenderPreset::Low,
        ..Settings::default()
    };
    let mut renderer = sgl_3d::Renderer::new(device);
    let mut scene = Scene::new(device, queue);
    let mut view = View::new(&renderer, device, queue, SCENE_FORMAT, size, 1., &settings)
        .expect("view on the canvas device");
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: perspective(1., size[0] as f32 / size[1] as f32, 0.1),
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.5, 0.3, 0.2]);
    input.camera_cut = true;
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        device,
        queue,
        &mut encoder,
        &mut scene,
        &mut [ViewFrame {
            view: &mut view,
            input: &input,
            settings: &settings,
            output: ViewOutput::Texture(canvas.scene_view()),
            timing: None,
        }],
    );
    queue.submit([encoder.finish()]);
    renderer.finish_frame(&mut scene);

    let before = canvas.read_scene(&gpu).expect("readback of the 3D picture");
    assert!(
        before.chunks_exact(4).any(|px| px != [0, 0, 0, 255]),
        "the SGL3D view left the canvas black"
    );

    let mut assets = Assets::new();
    let white = canvas.white_texture(&gpu, &mut assets);
    let mut list = DrawList::new();
    Overlay {
        color: [0.0, 1.0, 0.0, 1.0],
        ..Overlay::new(white)
    }
    .fill_rect(
        &mut list.screen,
        Rect::new(
            HUD_RECT.0.start as f32,
            HUD_RECT.1.start as f32,
            HUD_RECT.0.len() as f32,
            HUD_RECT.1.len() as f32,
        ),
    );
    canvas.render_screen(&gpu, &mut list);
    let after = canvas.read_scene(&gpu).expect("readback of the HUD");

    let width = CANVAS.0 as usize;
    for (i, (px, old)) in after
        .chunks_exact(4)
        .zip(before.chunks_exact(4))
        .enumerate()
    {
        let (x, y) = (i % width, i / width);
        if HUD_RECT.0.contains(&x) && HUD_RECT.1.contains(&y) {
            assert_eq!(px, HUD, "inside the HUD at ({x}, {y})");
        } else {
            assert_eq!(px, old, "outside the HUD at ({x}, {y})");
        }
    }
}
