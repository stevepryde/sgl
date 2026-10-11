//! The renderer accepts every output size, however thin, and restarts
//! history when its targets change.
use crate::renderer::Renderer;
use crate::settings::Settings;
use crate::{Camera, FrameInput, Scene, test_support};
use glam::camera;

// Plausible defect: a target sized from the output by a ratio (bloom's mip
// chain scales the scene to 512 texels high) exceeds the device's texture
// limit, or a mip chain has more levels than a one-pixel side allows, so a
// one-pixel-tall or one-pixel-wide window panics in wgpu. The oracle is
// wgpu's validation: creating, resizing to and rendering frames at 64×1 and
// 1×64 with default settings raises no validation error, nor with Crystal
// SSR at half resolution, whose ray targets are half the output (#290).
#[test]
fn one_pixel_thin_sizes_render_with_default_settings() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    test_support::add_static(&device, &queue, &mut scene, test_support::cube());
    let environment = scene
        .add_environment(
            &device,
            &queue,
            &test_support::environment([40, 60, 90, 255], &[0, 0x34]),
        )
        .unwrap();
    let half_ssr = Settings {
        screen_space_reflections: crate::settings::ScreenSpaceReflections::Half,
        ..Settings::default()
    };
    for settings in [Settings::default(), half_ssr] {
        for (first, second) in [([64, 1], [1, 64]), ([1, 64], [64, 1])] {
            let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let mut renderer = Renderer::new(
                &device,
                &queue,
                wgpu::TextureFormat::Rgba8Unorm,
                first,
                1.,
                &settings,
            )
            .unwrap();
            for size in [first, second] {
                renderer.resize(&device, size, 1., &settings);
                let output = crate::view::targets::target(
                    &device,
                    "thin",
                    size,
                    wgpu::TextureFormat::Rgba8Unorm,
                );
                for frame in 0..2 {
                    let eye = glam::Vec3::new(2.4 + frame as f32 * 0.05, 2., 3.3);
                    let mut input = FrameInput::new(Camera {
                        eye,
                        view: camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
                        projection: crate::perspective(
                            55f32.to_radians(),
                            size[0] as f32 / size[1] as f32,
                            0.1,
                        ),
                    });
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
            }
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            if let Some(error) = pollster::block_on(validation.pop()) {
                panic!(
                    "{first:?} then {second:?}, SSR {:?}: {error}",
                    settings.screen_space_reflections
                );
            }
        }
    }
}

// Plausible defect: `finish_frame` clears the reset a resize requested
// between `render` and `finish_frame`, so the next frame reuses history
// (TAA, Crystal SSR) rendered at the old size. The oracle is the documented
// contract of `render`: history restarts after a resize that changed the
// targets, so the frame after render, resize, finish_frame starts invalid,
// while the same sequence without a resize continues history.
#[test]
fn resize_between_render_and_finish_restarts_history() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    test_support::add_static(&device, &queue, &mut scene, test_support::cube());
    let settings = Settings::default();
    let first = [64, 48];
    let second = [80, 48];
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8Unorm,
        first,
        1.,
        &settings,
    )
    .unwrap();
    let render = |renderer: &mut Renderer, scene: &mut Scene, size: [u32; 2]| {
        let output =
            crate::view::targets::target(&device, "resize", size, wgpu::TextureFormat::Rgba8Unorm);
        let eye = glam::Vec3::new(2.4, 2., 3.3);
        let input = FrameInput::new(Camera {
            eye,
            view: camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
            projection: crate::perspective(
                55f32.to_radians(),
                size[0] as f32 / size[1] as f32,
                0.1,
            ),
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            scene,
            &input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.rendered.as_ref().unwrap().0.valid
    };
    render(&mut renderer, &mut scene, first);
    renderer.finish_frame(&mut scene);
    assert!(
        render(&mut renderer, &mut scene, first),
        "history continues"
    );
    renderer.resize(&device, second, 1., &settings);
    renderer.finish_frame(&mut scene);
    assert!(
        !render(&mut renderer, &mut scene, second),
        "the frame after the resize restarts history"
    );
    renderer.finish_frame(&mut scene);
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
}

// Plausible defect: the fog's froxel volume is sized from the output by a
// ratio with no bound, or without the device's 3D texture limit, so a tall
// portrait frame asks for a volume taller than that limit and wgpu panics
// creating it: at High, 1×8192 asked for about 524k froxels tall, past any
// device's limit, and 60×1900 for 2090, past the 2048 of Metal, D3D12 and
// WebGPU. The oracle is wgpu's validation: rendering those frames with
// High fog on (atmosphere allowed and turned on, a dense medium) raises no
// validation error. The sizing's other shapes and qualities are
// `stages::fog::tests`'.
#[test]
fn thin_sizes_render_with_fog() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    test_support::add_static(&device, &queue, &mut scene, test_support::cube());
    let settings = Settings {
        fog_quality: crate::settings::FogQuality::High,
        ..Settings::default()
    };
    for size in [[60, 1900], [1, 8192]] {
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut renderer = Renderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            size,
            1.,
            &settings,
        )
        .unwrap();
        let output = crate::view::targets::target(
            &device,
            "thin fog",
            size,
            wgpu::TextureFormat::Rgba8Unorm,
        );
        let eye = glam::Vec3::new(2.4, 2., 3.3);
        let mut input = FrameInput::new(Camera {
            eye,
            view: camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
            projection: crate::perspective(
                55f32.to_radians(),
                size[0] as f32 / size[1] as f32,
                0.1,
            ),
        });
        input.atmosphere = true;
        input.fog.density = 0.05;
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
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        if let Some(error) = pollster::block_on(validation.pop()) {
            panic!("{size:?}: {error}");
        }
    }
}
