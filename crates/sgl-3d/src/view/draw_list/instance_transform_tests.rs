//! GPU observations of transformed geometry, independent of the normal transform.
use crate::asset::Vertex;
use crate::renderer::Renderer;
use crate::settings::{RenderPreset, Settings};
use crate::view::pipelines::GeometryPass;
use crate::*;
use glam::{Mat4, Vec3};

const SIZE: u32 = 32;

fn target(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("transformed plane observation"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn center(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let bpp = texture.format().block_copy_size(None).unwrap();
    let bytes = test_support::read(device, queue, texture, bpp);
    let offset = ((SIZE / 2 * SIZE + SIZE / 2) * bpp) as usize;
    bytes[offset..offset + bpp as usize].to_vec()
}

#[test]
#[ignore = "real GPU; affine normal direction and mirrored authored material sides"]
fn scaled_and_mirrored_instances_preserve_normals_and_material_sides() {
    pollster::block_on(async {
        let adapter = wgpu::Instance::default()
            .request_adapter(&Default::default())
            .await
            .unwrap();
        // Exercise actual pipeline creation/rendering at the minimal budget,
        // the former incorrect full-pass boundary, and its supported boundary.
        for budget in [32, 40, 56, 64]
            .into_iter()
            .filter(|budget| *budget <= adapter.limits().max_color_attachment_bytes_per_sample)
        {
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    required_features: adapter.features() & wgpu::Features::SHADER_F16,
                    required_limits: wgpu::Limits {
                        max_color_attachment_bytes_per_sample: budget,
                        ..graphics_device::limits(&adapter)
                    },
                    ..Default::default()
                })
                .await
                .unwrap();
            let mut asset = test_support::cube();
            asset.meshes = vec![asset::CpuMesh {
                vertices: [(-0.4, -0.4), (0.4, -0.4), (0.4, 0.4), (-0.4, 0.4)]
                    .map(|(x, y)| Vertex {
                        tangent: [0.0; 4],
                        lightmap_uv: [0.; 2],
                        lightmap_bounds: [0., 0., 1., 1.],
                        position: [x, y, 0.2 * x],
                        normal: Vec3::new(-0.2, 0., 1.).normalize().to_array(),
                        uv: [0.; 2],
                        color: [1.; 4],
                    })
                    .to_vec(),
                indices: vec![0, 1, 2, 0, 2, 3],
                material: 0,
                deformation: Default::default(),
            }];
            asset.materials[0].unlit = true;
            asset.materials[0].base = [0.25, 0.5, 0.75, 1.];
            let mut scene = Scene::new(&device, &queue);
            let ids = scene.add_asset(&device, &queue, asset).unwrap();
            let mut instance = None;
            let mut renderer = Renderer::for_test(
                &device,
                &queue,
                [SIZE, SIZE],
                &Settings {
                    preset: RenderPreset::Low,
                    ..Settings::default()
                },
            );
            let mut stable_formats = vec![
                shading::gbuffer::NORMAL,
                shading::gbuffer::MATERIAL,
                shading::gbuffer::MOTION,
                shading::gbuffer::F0,
            ];
            if renderer.test_anisotropy_inline() {
                stable_formats.push(shading::gbuffer::ANISOTROPY);
            }
            let stable: Vec<_> = stable_formats
                .into_iter()
                .map(|format| target(&device, format))
                .collect();
            let color = [shading::gbuffer::COLOR, shading::gbuffer::MOTION]
                .map(|format| target(&device, format));
            let full = renderer.test_fused_supported().then(|| {
                [
                    shading::gbuffer::NORMAL,
                    shading::gbuffer::MATERIAL,
                    shading::gbuffer::MOTION,
                    shading::gbuffer::F0,
                    shading::gbuffer::COLOR,
                    shading::gbuffer::AMBIENT,
                    shading::gbuffer::SOURCE_ID,
                    shading::gbuffer::ANISOTROPY,
                ]
                .map(|format| target(&device, format))
            });
            let depth = target(&device, wgpu::TextureFormat::Depth32Float);
            let depth_view = depth.create_view(&Default::default());
            let shadow = target(&device, wgpu::TextureFormat::Depth32Float);
            let shadow_view = shadow.create_view(&Default::default());
            for scale_x in [2., -2.] {
                let pose = Mat4::from_translation(Vec3::new(0., 0., 0.5))
                    * Mat4::from_scale(Vec3::new(scale_x, 1., 1.));
                let state = InstanceState {
                    model: ids.model,
                    pose,
                    visible: true,
                    capture_visible: true,
                };
                match instance {
                    Some(instance) => scene.set_instance(&queue, instance, state).unwrap(),
                    None => {
                        instance = Some(
                            scene
                                .add_instance(&device, &queue, state, Mobility::Moving)
                                .unwrap(),
                        )
                    }
                }
                for double_sided in [false, true] {
                    let mut values = scene.material(ids.materials[0]).unwrap();
                    values.double_sided = double_sided;
                    scene
                        .set_material(&queue, ids.materials[0], values)
                        .unwrap();
                    for back in [false, true] {
                        // Flip X and Z to look at the physical back, retaining depth 0..1.
                        let view = if back {
                            Mat4::from_translation(Vec3::Z)
                                * Mat4::from_scale(Vec3::new(-1., 1., -1.))
                        } else {
                            Mat4::IDENTITY
                        };
                        let mut input = FrameInput::new(Camera {
                            view,
                            projection: Mat4::IDENTITY,
                            eye: if back { Vec3::ZERO } else { Vec3::Z },
                        });
                        input.directional_lights[0] = Some(DirectionalLight {
                            direction: Vec3::NEG_Z,
                            color: [1.; 3],
                            illuminance: 1.,
                            shadow: None,
                            ..Default::default()
                        });
                        input.baked_lighting = false;
                        input.atmosphere = false;
                        let prepared = renderer.prepare_test_frame(
                            &device,
                            &queue,
                            &mut scene,
                            &input,
                            &Settings::default(),
                        );
                        // The light's shadow view is the camera's.
                        renderer.set_test_cascade((&device, &queue), &scene, &prepared, view);
                        let mut encoder = device.create_command_encoder(&Default::default());
                        let mut draws = vec![(0, stable.as_slice()), (1, color.as_slice())];
                        if let Some(targets) = &full {
                            draws.push((2, targets.as_slice()));
                        }
                        for (kind, targets) in draws {
                            let views: Vec<_> = targets
                                .iter()
                                .map(|texture| texture.create_view(&Default::default()))
                                .collect();
                            let attachments: Vec<_> = views
                                .iter()
                                .map(|view| {
                                    Some(wgpu::RenderPassColorAttachment {
                                        view,
                                        depth_slice: None,
                                        resolve_target: None,
                                        ops: wgpu::Operations {
                                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                            store: wgpu::StoreOp::Store,
                                        },
                                    })
                                })
                                .collect();
                            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: Some("production transformed scene draw"),
                                color_attachments: &attachments,
                                depth_stencil_attachment: Some(
                                    wgpu::RenderPassDepthStencilAttachment {
                                        view: &depth_view,
                                        depth_ops: Some(wgpu::Operations {
                                            load: wgpu::LoadOp::Clear(0.),
                                            store: wgpu::StoreOp::Store,
                                        }),
                                        stencil_ops: None,
                                    },
                                ),
                                ..Default::default()
                            });
                            match kind {
                                0 => renderer.draw_test_camera(
                                    &scene,
                                    &mut pass,
                                    GeometryPass::GBuffer,
                                ),
                                1 => renderer.draw_test_camera(
                                    &scene,
                                    &mut pass,
                                    GeometryPass::Forward,
                                ),
                                _ => renderer.draw_test_camera(
                                    &scene,
                                    &mut pass,
                                    GeometryPass::Fused,
                                ),
                            }
                        }
                        renderer.encode_test_shadows(
                            &device,
                            &queue,
                            &mut encoder,
                            &scene,
                            &prepared,
                            Some(&shadow_view),
                        );
                        queue.submit([encoder.finish()]);
                        if let Some(full) = &full {
                            // These are real attachment readbacks from separate indexed
                            // and vertex-pulled raster paths. A binding, output location,
                            // winding or clip-position mismatch changes the pixel bytes.
                            for (fused, split) in [(0, 0), (1, 1), (3, 3)] {
                                let bpp = full[fused].format().block_copy_size(None).unwrap();
                                assert_eq!(
                                    test_support::read(&device, &queue, &full[fused], bpp),
                                    test_support::read(&device, &queue, &stable[split], bpp),
                                    "fused/split material target {fused}, scale={scale_x} double={double_sided} back={back}",
                                );
                            }
                        }
                        let visible = double_sided || !back;
                        let observed_depth =
                            f32::from_le_bytes(center(&device, &queue, &depth).try_into().unwrap());
                        assert_eq!(
                            observed_depth > 0.,
                            visible,
                            "primary scale={scale_x} double={double_sided} back={back}, depth={observed_depth}"
                        );
                        let shadow_depth = f32::from_le_bytes(
                            center(&device, &queue, &shadow).try_into().unwrap(),
                        );
                        // The light looks along the camera here, so it records the
                        // faces the camera does: a single-sided material casts from
                        // its front faces, as in Bevy.
                        assert_eq!(
                            shadow_depth > 0.1,
                            visible,
                            "directional scale={scale_x} double={double_sided} back={back}, depth={shadow_depth}"
                        );
                        let encoded = center(&device, &queue, &stable[0]);
                        if visible {
                            // Decode the public octahedral base-normal attachment.
                            let x = test_support::half(&encoded);
                            let y = test_support::half(&encoded[2..]);
                            let z = 1. - x.abs() - y.abs();
                            let t = (-z).clamp(0., 1.);
                            let normal =
                                Vec3::new(x - t.copysign(x), y - t.copysign(y), z).normalize();
                            // The geometric surface tangent comes directly from the two transformed endpoints.
                            let tangent = (pose.transform_point3(Vec3::new(0.4, 0., 0.08))
                                - pose.transform_point3(Vec3::new(-0.4, 0., -0.08)))
                            .normalize();
                            assert!(
                                normal.dot(tangent).abs() < 0.002,
                                "normal {normal:?} is not perpendicular to geometric tangent {tangent:?}"
                            );
                            assert!(normal.dot(Vec3::Y).abs() < 0.002);
                            assert!(
                                normal.z * if back { -1. } else { 1. } > 0.98,
                                "wrong normal orientation: {normal:?}, back={back}"
                            );
                            let rgba = center(&device, &queue, &color[0]);
                            for (channel, expected) in [0.25, 0.5, 0.75].into_iter().enumerate() {
                                assert!(
                                    (test_support::half(&rgba[channel * 2..]) - expected).abs()
                                        < 0.001,
                                    "mirrored primary material was lost"
                                );
                            }
                        } else {
                            assert_eq!(
                                test_support::half(&encoded[6..]),
                                0.,
                                "single-sided stable back must be discarded"
                            );
                        }
                        scene.finish_frame();
                    }
                }
            }
        }
    });
}
