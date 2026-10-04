use crate::renderer::Renderer;
use crate::settings::{Antialiasing, Fsr2Quality, Settings};
use crate::*;
use glam::camera;

const SIZE: [u32; 2] = [96, 54];

/// A static cube under an environment of constant radiance .25, which lights
/// frames that name it.
fn scene(device: &wgpu::Device, queue: &wgpu::Queue) -> (Scene, EnvironmentId) {
    let mut scene = Scene::new(device, queue);
    test_support::add_static(device, queue, &mut scene, test_support::cube());
    let environment = scene
        .add_environment(
            device,
            queue,
            &test_support::environment([40, 60, 90, 255], &[0, 0x34]),
        )
        .unwrap();
    (scene, environment)
}

// Defects: choosing FSR2 on a device that cannot run it shrinks the targets
// or records invalid GPU work instead of falling back to TAA; with a running
// context, a quality mode renders at the wrong size, the upscaled output is
// not the scene size, or a frame records invalid work. The expected render
// sizes come from the per-dimension ratios in AMD's documentation ("Scaling
// modes": 1.5, 1.7, 2 and 3, rounded down), the extents from the textures
// and the validity from wgpu's validation.
#[test]
#[ignore = "real GPU; FSR2 per-quality frames or TAA fallback"]
fn fsr2_renders_each_quality_or_falls_back_to_taa() {
    let adapter =
        pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default())).unwrap();
    for with_features in [false, true] {
        let features = if with_features {
            graphics_device::fsr2_features(&adapter)
        } else {
            wgpu::Features::empty()
        };
        if with_features && features.is_empty() {
            eprintln!("the adapter lacks FSR2's features");
            continue;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: features,
            required_limits: graphics_device::limits(&adapter),
            ..Default::default()
        }))
        .unwrap();
        let (mut scene, environment) = scene(&device, &queue);
        let mut settings = Settings {
            scene_resolution: settings::SceneResolution::Full,
            ..Settings::default()
        };
        let mut renderer = Renderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            SIZE,
            1.,
            &settings,
        )
        .unwrap();
        settings.antialiasing = Antialiasing::Fsr2;
        let output =
            view::targets::target(&device, "FSR2 frame", SIZE, wgpu::TextureFormat::Rgba8Unorm);
        for (quality, render) in [
            (Fsr2Quality::NativeAa, SIZE),
            (Fsr2Quality::Quality, [64, 36]),
            (Fsr2Quality::Balanced, [56, 31]),
            (Fsr2Quality::Performance, [48, 27]),
            (Fsr2Quality::UltraPerformance, [32, 18]),
        ] {
            settings.fsr2_quality = quality;
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            renderer.resize(&device, SIZE, 1., &settings);
            for frame in 0..4 {
                let eye = glam::Vec3::new(2.4 + frame as f32 * 0.05, 2., 3.3);
                let mut input = FrameInput::new(Camera {
                    eye,
                    view: camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
                    projection: perspective(
                        55f32.to_radians(),
                        SIZE[0] as f32 / SIZE[1] as f32,
                        0.1,
                    ),
                });
                input.camera_cut = frame == 0;
                input.environment = Some(environment);
                let mut encoder = device.create_command_encoder(&Default::default());
                renderer.render(
                    &device,
                    &queue,
                    &mut encoder,
                    &mut scene,
                    &input,
                    &settings,
                    &output,
                    None,
                );
                queue.submit([encoder.finish()]);
                renderer.finish_frame(&mut scene);
            }
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            if let Some(error) = pollster::block_on(scope.pop()) {
                panic!("{quality:?}: {error}");
            }
            let extent = |view: &wgpu::TextureView| {
                let size = view.texture().size();
                [size.width, size.height]
            };
            if renderer.antialiasing_in_effect(&settings) == Antialiasing::Fsr2 {
                assert_eq!(extent(&renderer.targets().color), render, "{quality:?}");
                assert_eq!(extent(renderer.test_fsr2().unwrap().output()), SIZE);
            } else {
                eprintln!(
                    "{quality:?} with {features:?}: TAA, because {}",
                    renderer.fsr2_error().unwrap()
                );
                assert_eq!(
                    renderer.antialiasing_in_effect(&settings),
                    Antialiasing::Taa
                );
                assert_eq!(extent(&renderer.targets().color), SIZE, "{quality:?}");
            }
        }
    }
}

// Defects: the camera's additive effects do not reach FSR2's reactive mask,
// a mask keeps the previous frame's effects (not cleared each frame), or
// marks pixels no effect covers. A glow far brighter than AMD's documented
// 0.9 reactivity cap must read as that cap (R8: 229 or 230), independent of
// fog; coverage is where the quad was placed; the effects draw nothing into
// the transparency and composition mask (AMD's reactive particles).
#[test]
#[ignore = "real GPU: FSR2 masks from the camera's transparent draws"]
fn additive_effects_mark_fsr2_reactivity_for_their_frame_only() {
    let adapter =
        pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default())).unwrap();
    let features = graphics_device::fsr2_features(&adapter);
    if features.is_empty() {
        eprintln!("the adapter lacks FSR2's features");
        return;
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        required_limits: graphics_device::limits(&adapter),
        ..Default::default()
    }))
    .unwrap();
    let (mut scene, environment) = scene(&device, &queue);
    let settings = Settings {
        scene_resolution: settings::SceneResolution::Full,
        antialiasing: Antialiasing::Fsr2,
        fsr2_quality: Fsr2Quality::NativeAa,
        ..Settings::default()
    };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8Unorm,
        SIZE,
        1.,
        &settings,
    )
    .unwrap();
    assert_eq!(
        renderer.antialiasing_in_effect(&settings),
        Antialiasing::Fsr2,
        "{:?}",
        renderer.fsr2_error()
    );
    let output =
        view::targets::target(&device, "FSR2 frame", SIZE, wgpu::TextureFormat::Rgba8Unorm);
    let eye = glam::Vec3::new(2.4, 2., 3.3);
    let forward = -eye.normalize();
    let right = forward.cross(glam::Vec3::Y).normalize();
    let up = right.cross(forward);
    // A quad one metre ahead covering the middle third of the view.
    let corner = |x: f32, y: f32| effects::Glow {
        position: (eye + forward + (right * x + up * y) * 0.15).to_array(),
        color: [4., 2., 1., 0.5],
        kind: effects::GlowKind::Uniform,
        soft_distance: 0.,
    };
    let quad = [
        (-1., -1.),
        (1., -1.),
        (1., 1.),
        (-1., -1.),
        (1., 1.),
        (-1., 1.),
    ]
    .map(|(x, y)| corner(x, y));
    let mut frame = |index: u32, glow: &[effects::Glow]| {
        scene.update_effects(&device, &queue, glow);
        let mut input = FrameInput::new(Camera {
            eye,
            view: camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
            projection: perspective(55f32.to_radians(), SIZE[0] as f32 / SIZE[1] as f32, 0.1),
        });
        input.camera_cut = index == 0;
        input.environment = Some(environment);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
        renderer
            .targets()
            .fsr2_masks
            .clone()
            .map(|mask| test_support::read(&device, &queue, mask.texture(), 1))
    };
    let at = |x: u32, y: u32| (y * SIZE[0] + x) as usize;
    let [reactive, composition] = frame(0, &quad);
    assert!(
        (229..=230).contains(&reactive[at(SIZE[0] / 2, SIZE[1] / 2)]),
        "reactivity under the glow: {}",
        reactive[at(SIZE[0] / 2, SIZE[1] / 2)]
    );
    for (x, y) in [
        (0, 0),
        (SIZE[0] - 1, 0),
        (0, SIZE[1] - 1),
        (SIZE[0] - 1, SIZE[1] - 1),
    ] {
        assert_eq!(
            reactive[at(x, y)],
            0,
            "reactivity outside the glow at {x},{y}"
        );
    }
    assert!(
        composition.iter().all(|&v| v == 0),
        "effects marked transparency"
    );
    let [reactive, _] = frame(1, &[]);
    assert!(
        reactive.iter().all(|&v| v == 0),
        "the previous frame's glow stayed reactive"
    );
}

// Defect: FSR2 stops below a 64-pixel render size. Its luminance pyramid
// binds mips 4 and 5 of a texture half the maximum render size
// (`ffx_fsr2.cpp`), which sp-fidelity-wgpu before 0.1.1 viewed even where
// the texture lacked them, so wgpu rejected the frame. Expected: wgpu's
// validation accepts every frame and FSR2 stays in effect, at the smallest
// size (a single mip), either side of the 32-pixel (mip 4) boundary and just
// below 64 (mip 5).
#[test]
fn fsr2_runs_below_a_64_pixel_render_size() {
    let Some((device, queue)) = test_support::fsr2_device() else {
        return;
    };
    if !device
        .features()
        .contains(sp_fidelity_wgpu::required_features())
    {
        eprintln!("skipping: the device lacks FSR2's features");
        return;
    }
    let (mut scene, environment) = scene(&device, &queue);
    let settings = Settings {
        scene_resolution: settings::SceneResolution::Full,
        antialiasing: Antialiasing::Fsr2,
        fsr2_quality: Fsr2Quality::NativeAa,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, [64, 64], &settings);
    for size in [[2, 2], [31, 31], [32, 32], [33, 33], [63, 63]] {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        renderer.resize(&device, size, 1., &settings);
        let output = view::targets::target(&device, "FSR2 frame", size, shading::gbuffer::COLOR);
        for frame in 0..2 {
            let eye = glam::Vec3::new(2.4 + frame as f32 * 0.05, 2., 3.3);
            let mut input = FrameInput::new(Camera {
                eye,
                view: camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
                projection: perspective(55f32.to_radians(), 1., 0.1),
            });
            input.camera_cut = frame == 0;
            input.environment = Some(environment);
            let mut encoder = device.create_command_encoder(&Default::default());
            renderer.render(
                &device,
                &queue,
                &mut encoder,
                &mut scene,
                &input,
                &settings,
                &output,
                None,
            );
            queue.submit([encoder.finish()]);
            renderer.finish_frame(&mut scene);
        }
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        if let Some(error) = pollster::block_on(scope.pop()) {
            panic!("{size:?}: {error}");
        }
        assert_eq!(
            renderer.antialiasing_in_effect(&settings),
            Antialiasing::Fsr2,
            "{size:?}: {:?}",
            renderer.fsr2_error()
        );
    }
}
