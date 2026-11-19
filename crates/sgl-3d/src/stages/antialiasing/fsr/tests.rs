use crate::renderer::test_frame::TestRenderer;
use crate::settings::{Antialiasing, FsrQuality, Settings};
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

// Defects: choosing FSR on a device that cannot run it shrinks the targets
// or records invalid GPU work instead of falling back to TAA; with a running
// context, a quality mode renders at the wrong size, the upscaled output is
// not the scene size, or a frame records invalid work. The expected render
// sizes come from the per-dimension ratios in AMD's documentation ("Scaling
// modes": 1.5, 1.7, 2 and 3, rounded down), the extents from the textures
// and the validity from wgpu's validation.
#[test]
fn fsr_renders_each_quality_or_falls_back_to_taa() {
    for request in [test_support::device, test_support::fsr_device] {
        let Some((device, queue)) = request() else {
            continue;
        };
        let (mut scene, environment) = scene(&device, &queue);
        let mut settings = Settings {
            scene_resolution: settings::SceneResolution::Full,
            ..Settings::default()
        };
        let mut renderer = TestRenderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            SIZE,
            1.,
            &settings,
        )
        .unwrap();
        // Reuse the renderer at identical dimensions across version changes.
        for (quality, render) in [
            (FsrQuality::NativeAa, SIZE),
            (FsrQuality::Quality, [64, 36]),
            (FsrQuality::Balanced, [56, 31]),
            (FsrQuality::Performance, [48, 27]),
            (FsrQuality::UltraPerformance, [32, 18]),
        ] {
            settings.fsr_quality = quality;
            for method in [Antialiasing::Fsr2, Antialiasing::Fsr3, Antialiasing::Fsr2] {
                settings.antialiasing = method;
                let output = view::targets::target(
                    &device,
                    "FSR frame",
                    SIZE,
                    wgpu::TextureFormat::Rgba8Unorm,
                );
                let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                renderer.view.resize(&device, SIZE, 1., &settings);
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
                    input.environment = crate::FrameEnvironment::Map(environment);
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
                if device
                    .features()
                    .contains(sp_fidelity_wgpu::required_features())
                {
                    assert_eq!(
                        renderer.view.antialiasing_in_effect(&settings),
                        method,
                        "{:?}",
                        renderer.view.fsr_error()
                    );
                    assert!(renderer.targets().temporal_masks.is_some());
                    assert_eq!(extent(&renderer.targets().color), render, "{quality:?}");
                    assert_eq!(extent(renderer.test_fsr().unwrap().output()), SIZE);
                } else {
                    assert!(
                        renderer.targets().temporal_masks.is_none(),
                        "unsupported FSR must allocate no masks"
                    );
                    eprintln!(
                        "{quality:?} with {:?}: TAA, because {}",
                        device.features(),
                        renderer.view.fsr_error().unwrap()
                    );
                    assert_eq!(
                        renderer.view.antialiasing_in_effect(&settings),
                        Antialiasing::Taa
                    );
                    assert_eq!(extent(&renderer.targets().color), SIZE, "{quality:?}");
                }
            }
        }
    }
}

// Defects: the camera's additive effects do not reach FSR's reactive mask,
// a mask keeps the previous frame's effects (not cleared each frame), or
// marks pixels no effect covers. A glow far brighter than AMD's documented
// 0.9 reactivity cap must read as that cap (R8: 229 or 230), independent of
// fog; coverage is where the quad was placed; the effects draw nothing into
// the transparency and composition mask (AMD's reactive particles).
#[test]
fn additive_effects_mark_fsr_reactivity_for_their_frame_only() {
    for method in [Antialiasing::Fsr2, Antialiasing::Fsr3] {
        additive_effects_mark_fsr_reactivity_for_their_frame_only_for(method);
    }
}

fn additive_effects_mark_fsr_reactivity_for_their_frame_only_for(method: Antialiasing) {
    let Some((device, queue)) = test_support::fsr_device() else {
        return;
    };
    if !device
        .features()
        .contains(sp_fidelity_wgpu::required_features())
    {
        eprintln!("skipping: the device lacks FSR's features");
        return;
    }
    let (mut scene, environment) = scene(&device, &queue);
    let settings = Settings {
        scene_resolution: settings::SceneResolution::Full,
        antialiasing: method,
        fsr_quality: FsrQuality::NativeAa,
        ..Settings::default()
    };
    let mut renderer = TestRenderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8Unorm,
        SIZE,
        1.,
        &settings,
    )
    .unwrap();
    assert_eq!(
        renderer.view.antialiasing_in_effect(&settings),
        method,
        "{:?}",
        renderer.view.fsr_error()
    );
    let output = view::targets::target(&device, "FSR frame", SIZE, wgpu::TextureFormat::Rgba8Unorm);
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
        scene.update_effects(&device, &queue, glow).unwrap();
        let mut input = FrameInput::new(Camera {
            eye,
            view: camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
            projection: perspective(55f32.to_radians(), SIZE[0] as f32 / SIZE[1] as f32, 0.1),
        });
        input.camera_cut = index == 0;
        input.environment = crate::FrameEnvironment::Map(environment);
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
            .temporal_masks
            .as_ref()
            .unwrap()
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

// Defect: FSR stops below a 64-pixel render size. Its luminance pyramid
// binds mips 4 and 5 of a texture half the maximum render size
// (`ffx_fsr.cpp`), which sp-fidelity-wgpu before 0.1.1 viewed even where
// the texture lacked them, so wgpu rejected the frame. Expected: wgpu's
// validation accepts every frame and FSR stays in effect, at the smallest
// size (a single mip), either side of the 32-pixel (mip 4) boundary and just
// below 64 (mip 5).
#[test]
fn fsr_runs_below_a_64_pixel_render_size() {
    for method in [Antialiasing::Fsr2, Antialiasing::Fsr3] {
        fsr_runs_below_a_64_pixel_render_size_for(method);
    }
}

fn fsr_runs_below_a_64_pixel_render_size_for(method: Antialiasing) {
    let Some((device, queue)) = test_support::fsr_device() else {
        return;
    };
    if !device
        .features()
        .contains(sp_fidelity_wgpu::required_features())
    {
        eprintln!("skipping: the device lacks FSR's features");
        return;
    }
    let (mut scene, environment) = scene(&device, &queue);
    let settings = Settings {
        scene_resolution: settings::SceneResolution::Full,
        antialiasing: method,
        fsr_quality: FsrQuality::NativeAa,
        ..Settings::default()
    };
    let mut renderer = TestRenderer::for_test(&device, &queue, [64, 64], &settings);
    for size in [[2, 2], [31, 31], [32, 32], [33, 33], [63, 63]] {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        renderer.view.resize(&device, size, 1., &settings);
        let output = view::targets::target(&device, "FSR frame", size, shading::gbuffer::COLOR);
        for frame in 0..2 {
            let eye = glam::Vec3::new(2.4 + frame as f32 * 0.05, 2., 3.3);
            let mut input = FrameInput::new(Camera {
                eye,
                view: camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
                projection: perspective(55f32.to_radians(), 1., 0.1),
            });
            input.camera_cut = frame == 0;
            input.environment = crate::FrameEnvironment::Map(environment);
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
            renderer.view.antialiasing_in_effect(&settings),
            method,
            "{size:?}: {:?}",
            renderer.view.fsr_error()
        );
    }
}

// Defects: a failed FSR dispatch leaves the backend's work, recorded with a
// bind group wgpu rejected, in the frame's encoder, so the game's `finish`
// reports the encoder invalid (a panic under wgpu's default handler) and the
// frame never runs; or the failure goes unnoticed, since FSR's dispatch
// ignores its jobs' result, and the frame presents FSR's undefined output;
// instead of completing without FSR and TAA taking over. The failure is
// forced as a backend call fails: FSR's output is destroyed before its
// first dispatch, so the pass that writes it cannot view it. Expected, from
// wgpu's validation and the textures: no validation error reaches the game,
// the frame's output is no longer the zeroed start (the composite reached
// it), FSR stops with wgpu's reason, which names the destroyed texture by
// its label, and the next frame runs TAA at the scene size.
#[test]
fn a_failed_fsr_dispatch_completes_the_frame_and_taa_takes_over() {
    for method in [Antialiasing::Fsr2, Antialiasing::Fsr3] {
        a_failed_fsr_dispatch_completes_the_frame_and_taa_takes_over_for(method);
    }
}

fn a_failed_fsr_dispatch_completes_the_frame_and_taa_takes_over_for(method: Antialiasing) {
    let Some((device, queue)) = test_support::fsr_device() else {
        return;
    };
    if !device
        .features()
        .contains(sp_fidelity_wgpu::required_features())
    {
        eprintln!("skipping: the device lacks FSR's features");
        return;
    }
    let (mut scene, environment) = scene(&device, &queue);
    let mut settings = Settings {
        scene_resolution: settings::SceneResolution::Full,
        antialiasing: method,
        fsr_quality: FsrQuality::Quality,
        ..Settings::default()
    };
    let mut renderer = TestRenderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8Unorm,
        SIZE,
        1.,
        &settings,
    )
    .unwrap();
    assert_eq!(
        renderer.view.antialiasing_in_effect(&settings),
        method,
        "{:?}",
        renderer.view.fsr_error()
    );
    assert_ne!(renderer.view.render_size(), SIZE);
    renderer.test_fsr().unwrap().output().texture().destroy();
    let output = view::targets::target(&device, "FSR frame", SIZE, wgpu::TextureFormat::Rgba8Unorm);
    let mut frame = |renderer: &mut TestRenderer, index: u32, settings: &Settings| {
        let eye = glam::Vec3::new(2.4 + index as f32 * 0.05, 2., 3.3);
        let mut input = FrameInput::new(Camera {
            eye,
            view: camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
            projection: perspective(55f32.to_radians(), SIZE[0] as f32 / SIZE[1] as f32, 0.1),
        });
        input.camera_cut = index == 0;
        input.environment = crate::FrameEnvironment::Map(environment);
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        renderer.view.resize(&device, SIZE, 1., settings);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &input,
            settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        if let Some(error) = pollster::block_on(scope.pop()) {
            panic!("frame {index}: {error}");
        }
    };
    frame(&mut renderer, 0, &settings);
    assert!(
        test_support::read(&device, &queue, output.texture(), 4)
            .chunks(4)
            .any(|pixel| pixel[..3] != [0; 3]),
        "the frame left its output black"
    );
    assert_eq!(
        renderer.view.antialiasing_in_effect(&settings),
        Antialiasing::Taa
    );
    let reason = renderer.view.fsr_error().unwrap();
    assert!(reason.contains("destroyed"), "{reason}");
    frame(&mut renderer, 1, &settings);
    assert_eq!(renderer.view.render_size(), SIZE);
    settings.antialiasing = match method {
        Antialiasing::Fsr2 => Antialiasing::Fsr3,
        Antialiasing::Fsr3 => Antialiasing::Fsr2,
        _ => unreachable!(),
    };
    frame(&mut renderer, 2, &settings);
    assert_eq!(
        renderer.view.antialiasing_in_effect(&settings),
        settings.antialiasing,
        "{:?}",
        renderer.view.fsr_error()
    );
    assert!(renderer.view.fsr_error().is_none());
    assert_ne!(renderer.view.render_size(), SIZE);
}
