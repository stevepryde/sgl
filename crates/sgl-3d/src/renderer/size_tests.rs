//! The renderer accepts every output size, however thin.
use crate::renderer::Renderer;
use crate::settings::Settings;
use crate::{Camera, FrameInput, Scene, test_support};
use glam::camera;

// Plausible defect: a target sized from the output by a ratio (bloom's mip
// chain scales the scene to 512 texels high) exceeds the device's texture
// limit, or a mip chain has more levels than a one-pixel side allows, so a
// one-pixel-tall or one-pixel-wide window panics in wgpu. The oracle is
// wgpu's validation: creating, resizing to and rendering frames at 64×1 and
// 1×64 with default settings raises no validation error.
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
    let settings = Settings::default();
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
            panic!("{first:?} then {second:?}: {error}");
        }
    }
}
