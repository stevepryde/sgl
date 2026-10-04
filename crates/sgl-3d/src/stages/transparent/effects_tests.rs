//! Measured GPU radiance against independently positioned planes.
use crate::renderer::Renderer;
use crate::settings::{RenderPreset, Settings};
use crate::{Camera, FrameInput, Scene};
use glam::Mat4;
use glam::camera;
#[test]
#[ignore = "real GPU: metric soft intersections and depth target replacement"]
fn soft_intersection_metric_depth_and_clear_background() {
    pollster::block_on(async {
        let adapter = wgpu::Instance::default()
            .request_adapter(&Default::default())
            .await
            .unwrap();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: crate::graphics_device::limits(&adapter),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut world = crate::test_support::cube();
        for vertex in &mut world.meshes[0].vertices {
            vertex.position = [vertex.position[0] * 100., vertex.position[1] * 100., -6.];
        }
        world.materials[0].unlit = true;
        world.materials[0].base = [1.; 4];
        world.materials[0].emissive = [0.; 3];
        let mut scene = Scene::new(&device, &queue);
        crate::test_support::add_static(&device, &queue, &mut scene, world);

        for size in [[25, 25], [39, 17]] {
            let color = crate::view::targets::target(
                &device,
                "soft fixture",
                size,
                crate::shading::gbuffer::COLOR,
            );
            let mut renderer = Renderer::for_test(
                &device,
                &queue,
                size,
                &Settings {
                    preset: RenderPreset::Low,
                    ..Settings::default()
                },
            );
            let depth = renderer.targets().depth.clone();
            for projection in [
                crate::perspective(1.2, size[0] as f32 / size[1] as f32, 0.1),
                camera::rh::proj::directx::orthographic(-50., 50., -50., 50., 100., 0.1),
            ] {
                let prepared = renderer.prepare_test_frame(
                    &device,
                    &queue,
                    &mut scene,
                    &FrameInput::new(Camera {
                        view: Mat4::IDENTITY,
                        projection,
                        eye: glam::Vec3::ZERO,
                    }),
                    &Settings::default(),
                );
                for plane in [3., 30.] {
                    // (distance in front of the opaque plane, fade width, independent expected fraction).
                    for (gap, width, fraction) in [
                        (-1., 2., 0.),
                        (0., 2., 0.),
                        (0.5, 2., 0.25),
                        (1., 2., 0.5),
                        (2., 2., 1.),
                        (0.5, 0., 1.),
                        (0.5, 2., 1.),
                    ] {
                        let sky = gap == 0.5 && width == 2. && fraction == 1.;
                        let z = plane - gap;
                        let vertices = [[-200., -200., -z], [600., -200., -z], [-200., 600., -z]]
                            .map(|position| crate::effects::Glow {
                                position,
                                uv: [0.; 2],
                                color: [1., 2., 3., 0.4],
                                kind: 0.,
                                other: [0.; 3],
                                soft_distance: width,
                            });
                        scene.update_effects(&device, &queue, &vertices);
                        let mut encoder = device.create_command_encoder(&Default::default());
                        {
                            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                color_attachments: &[crate::view::targets::attachment(&color)],
                                depth_stencil_attachment: Some(
                                    wgpu::RenderPassDepthStencilAttachment {
                                        view: &depth,
                                        depth_ops: Some(wgpu::Operations {
                                            load: wgpu::LoadOp::Clear(if sky {
                                                0.
                                            } else {
                                                projection
                                                    .project_point3(glam::Vec3::new(0., 0., -plane))
                                                    .z
                                            }),
                                            store: wgpu::StoreOp::Store,
                                        }),
                                        stencil_ops: None,
                                    },
                                ),
                                ..Default::default()
                            });
                        }
                        renderer.encode_test_transparent(
                            &device,
                            &queue,
                            &mut encoder,
                            &scene,
                            &prepared,
                            &color,
                        );
                        queue.submit([encoder.finish()]);
                        let pixels = crate::test_support::read(&device, &queue, color.texture(), 8);
                        let offset = ((size[1] / 2 * size[0] + size[0] / 2) * 8) as usize;
                        for c in 0..3 {
                            let actual = crate::test_support::half(&pixels[offset + c * 2..]);
                            let expected = 0.4 * [1., 2., 3.][c] * fraction;
                            assert!(
                                (actual - expected).abs() < 0.003,
                                "plane={plane} gap={gap} width={width} sky={sky} size={size:?} channel={c}: GPU={actual} expected={expected}"
                            );
                        }
                    }
                }
            }
        }
    });
}
