use super::*;
use crate::view::bindings::FogVolume;
use glam::camera;
use glam::{Mat4, Vec3, Vec4};

use super::tests::{no_fog, probe, visible};

#[test]
fn incident_environment_is_complete_without_doubling_primary() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    {
        let size = [2, 1];
        let texture =
            |name: &str, format: wgpu::TextureFormat, layers: u32, usage: wgpu::TextureUsages| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(name),
                    size: wgpu::Extent3d {
                        width: 2,
                        height: 1,
                        depth_or_array_layers: layers,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
            };
        let color = |name: &str, rgba: [f32; 4], array: bool| {
            let t = texture(
                name,
                wgpu::TextureFormat::Rgba16Float,
                1,
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            );
            let view = t.create_view(&Default::default());
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some(name),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: f64::from(rgba[0]),
                                g: f64::from(rgba[1]),
                                b: f64::from(rgba[2]),
                                a: f64::from(rgba[3]),
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
            }
            queue.submit([encoder.finish()]);
            t.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(if array {
                    wgpu::TextureViewDimension::D2Array
                } else {
                    wgpu::TextureViewDimension::D2
                }),
                ..Default::default()
            })
        };
        let scene = color("no direct light or emission", [0.; 4], false);
        let zero = color("zero emission", [0.; 4], false);
        let normal = color("toward camera", [0.; 4], false);
        let f0 = color("metal source", [1., 1., 1., 1.], false);
        let sky = color("uniform HDR sky", [2., 2., 2., 1.], true);
        let fog = no_fog(&device);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let lookup_tables = crate::scene::lookup_tables::lookup_tables(&device, &queue);
        let parameters = environment_uniform(&device, 0., 1.);
        let empty_collection = crate::scene::probes::UploadedProbes::empty(&device);
        let projection = camera::rh::proj::directx::orthographic(-10., 10., -1., 1., 100., 0.1);
        let camera = reflection_camera::Camera::new(Mat4::IDENTITY, projection);
        let projected = projection * Vec4::new(0., 0., -10., 1.);
        let depth = texture(
            "view depth ten",
            wgpu::TextureFormat::Depth32Float,
            1,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let depth_view = depth.create_view(&Default::default());
        let visibility = visible(&device, &queue);
        let mut source = ReflectionSource::new(
            &device,
            size,
            Variant {
                environment: true,
                incident: true,
                diffuse_occlusion: false,
            },
        );
        let output = ReflectionSource::target(&device, size, "complete opaque beauty");
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(projected.z / projected.w),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
        }
        queue.submit([encoder.finish()]);
        let read = |view: &wgpu::TextureView| -> Vec<f32> {
            crate::test_support::read(&device, &queue, view.texture(), 8)
                .chunks_exact(2)
                .map(crate::test_support::half)
                .collect()
        };
        // Independent optical oracle: an almost smooth white metal in a
        // uniform radiance-2 environment returns approximately radiance 2.
        // SSR eligibility must not change its illumination. With zero hit
        // confidence primary composition must recover that same environment
        // exactly once. This runs the production completion/composition GPU
        // pipelines, where a shared incomplete source or doubled fallback fails.
        let probes = crate::scene::probes::UploadedProbes::new(
            &device,
            &queue,
            &[probe(
                [1., 0.5, 2.],
                Vec3::splat(-20.),
                Vec3::splat(20.),
                0.,
            )],
        )
        .unwrap();
        for (lighting, collection, radiance) in [
            ("sky", &empty_collection, [2.; 3]),
            ("probe", &probes, [1., 0.5, 2.]),
        ] {
            for roughness in [0.05, 0.19, 0.21, 0.69, 0.71] {
                let material = color("metal source roughness", [1., roughness, 0., 1.], false);
                let mut reference = Vec::new();
                for (name, cutoff, fade) in [
                    ("disabled", 0.0f32, 0.05),
                    ("DiligentFX", 0.2, 0.05),
                    ("Godot", 0.7, 0.1),
                ] {
                    queue.write_buffer(
                        &parameters,
                        0,
                        bytemuck::cast_slice(&[0., 1., fade, cutoff * cutoff]),
                    );
                    let input = || Inputs {
                        fog: FogVolume {
                            view: &fog.0,
                            sampler: &fog.1,
                        },
                        fog_slices: None,
                        camera,
                        scene: &scene,
                        ambient: &zero,
                        output: &output,
                        normal: &normal,
                        anisotropy: &zero,
                        f0: &f0,
                        depth: &depth_view,
                        lookup_tables: &lookup_tables,
                        ambient_occlusion: &visibility,
                        environment: Environment {
                            sky: &sky,
                            sampler: &sampler,
                            parameters: &parameters,
                            material: &material,
                            baked: &collection.view,
                            collection: &collection.metadata,
                        },
                    };
                    let mut encoder = device.create_command_encoder(&Default::default());
                    source.encode(&mut encoder, &device, &queue, input(), None);
                    queue.submit([encoder.finish()]);
                    let incident = read(&source.incident);
                    let base = read(&output);
                    let mut encoder = device.create_command_encoder(&Default::default());
                    source.compose(&mut encoder, &device, input(), &zero, Some(&zero), None);
                    queue.submit([encoder.finish()]);
                    let primary = read(&output);
                    eprintln!(
                        "{lighting} roughness={roughness} {name}: incident={:?}, base={:?}, primary={:?}",
                        &incident[..3],
                        &base[..3],
                        &primary[..3]
                    );
                    if cutoff == 0. {
                        reference = primary.clone();
                    }
                    for pixel in 0..2 {
                        for (channel, expected_radiance) in radiance.iter().enumerate() {
                            let i = pixel * 4 + channel;
                            if roughness < 0.1 {
                                assert!(
                                    (incident[i] - expected_radiance).abs() < 0.1,
                                    "{lighting} {name}: smooth metal lost environment radiance: {}",
                                    incident[i]
                                );
                            }
                            assert!(
                                (incident[i] - reference[i]).abs() < 0.004,
                                "{lighting} {name}: incident illumination depends on tracing eligibility: {} vs {}",
                                incident[i],
                                reference[i]
                            );
                            assert!(
                                (primary[i] - reference[i]).abs() < 0.004,
                                "{lighting} {name}: primary environment missing or doubled: {} vs {}",
                                primary[i],
                                reference[i]
                            );
                        }
                    }
                }
            }
        }
    }
}
