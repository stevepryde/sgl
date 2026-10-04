//! Source completion's environment and probe specular, probe culling, and
//! the variant a renderer builds completion for.
use super::*;
use crate::view::bindings::FogVolume;
use glam::camera;
use glam::{Mat4, Vec3, Vec4};

/// Environment and probe specular, without incident radiance or ambient
/// occlusion.
const ENVIRONMENT: Variant = Variant {
    environment: true,
    incident: false,
    diffuse_occlusion: false,
};

/// Full ambient visibility, which `Scene` binds while ambient occlusion is off.
pub(super) fn visible(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    device
        .create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("full ambient visibility"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Uint,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::bytes_of(&255u32),
        )
        .create_view(&Default::default())
}

/// A fog volume to bind while the frame has none, and its sampler.
pub(super) fn no_fog(device: &wgpu::Device) -> (wgpu::TextureView, wgpu::Sampler) {
    let view = device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("no fog"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&Default::default());
    (view, device.create_sampler(&Default::default()))
}

// A probe of `rgb` radiance whose influence box spans `min`..`max` in world
// space with `blend` on x; the proxy is omitted, so it reflects at infinity.
pub(super) fn probe(rgb: [f32; 3], min: Vec3, max: Vec3, blend: f32) -> crate::BakedSpecularProbe {
    let face_size = 64;
    let texels: usize = (0..7)
        .map(|level| (face_size >> level) * (face_size >> level) * 6)
        .sum();
    // Binary16 of zero or a positive normal value (exact for the fixture's).
    let half = |v: f32| {
        let bits = v.to_bits();
        if v == 0. {
            0
        } else {
            ((((bits >> 23) & 0xff) as i32 - 112) as u16) << 10 | ((bits >> 13) & 0x3ff) as u16
        }
    };
    crate::BakedSpecularProbe {
        center: Vec3::ZERO,
        world_to_local: Mat4::IDENTITY,
        influence: crate::SpecularProbeBox { min, max },
        blend: Vec3::new(blend, 0., 0.),
        proxy: None,
        radiance: crate::SpecularProbeRadiance {
            face_size: face_size as u32,
            texels: crate::SpecularProbeTexels::Rgba16Float(
                [half(rgb[0]), half(rgb[1]), half(rgb[2]), half(1.)]
                    .into_iter()
                    .cycle()
                    .take(texels * 4)
                    .collect(),
            ),
        },
    }
}

// A 64-texel probe spanning `min`..`max` whose every BC6H block is mode 11
// (one region, 10-bit endpoints) with both endpoints `endpoint` and every
// index zero. Per the D3D11 BC6H format, each texel decodes to the unsigned
// unquantized endpoint, ((c << 16) + 0x8000) >> 10, finished as (x * 31) >> 6:
// 495 decodes to binary16 1.0 and 0 to 0.
fn bc6h_probe(endpoint: [u16; 3], min: Vec3, max: Vec3) -> crate::BakedSpecularProbe {
    let mut block = 0b00011u128;
    for (channel, &value) in endpoint.iter().enumerate() {
        block |= u128::from(value) << (5 + 10 * channel);
        block |= u128::from(value) << (35 + 10 * channel);
    }
    let blocks: usize = (0..7)
        .map(|level| (64usize >> level).div_ceil(4).pow(2) * 6)
        .sum();
    crate::BakedSpecularProbe {
        radiance: crate::SpecularProbeRadiance {
            face_size: 64,
            texels: crate::SpecularProbeTexels::Bc6hUfloat(block.to_le_bytes().repeat(blocks)),
        },
        ..probe([0.; 3], min, max, 0.)
    }
}

// A mirror receiver at (0, 0, -10) takes the environment specular of the
// probes whose influence contains it. Two probes each half way through their
// linear blend partition it with no sky, as do two whose cores both contain
// it; one alone half way leaves the rest to the sky; outside every influence
// it is the sky alone. A BC6H probe gives its decoded radiance, and so does
// the last probe of a full collection, from the last tile bucket.
#[test]
fn source_environment_blends_overlapping_probes_and_the_sky_by_influence() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    {
        let size = [1, 1];
        let texture =
            |name: &str, format: wgpu::TextureFormat, layers: u32, usage: wgpu::TextureUsages| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(name),
                    size: wgpu::Extent3d {
                        width: 1,
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
        let scene = color("unlit black", [0.; 4], false);
        let zero = color("zero", [0.; 4], false);
        let normal = color("toward camera", [0.; 4], false);
        let f0 = color("mirror metal", [1., 1., 1., 1.], false);
        let material = color("smooth metal", [1., 0., 0., 1.], false);
        let sky = color("green sky", [0., 2., 0., 1.], true);
        let fog = no_fog(&device);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let lookup_tables = crate::scene::lookup_tables::lookup_tables(&device, &queue);
        let parameters = environment_uniform(&device, 0., 1.);
        let upload = |probes: &[crate::BakedSpecularProbe]| {
            if probes.is_empty() {
                crate::scene::probes::UploadedProbes::empty(&device)
            } else {
                crate::scene::probes::UploadedProbes::new(&device, &queue, probes).unwrap()
            }
        };
        let far = Vec3::splat(20.);
        let red = [1., 0., 0.];
        let blue = [0., 0., 1.];
        // Probe A ends 5 m past the receiver and B begins 5 m before it, each
        // blending over 10 m: both weigh one half there.
        let a = probe(
            red,
            Vec3::new(-20., -20., -20.),
            Vec3::new(5., 20., 20.),
            10.,
        );
        let b = probe(
            blue,
            Vec3::new(-5., -20., -20.),
            Vec3::new(20., 20., 20.),
            10.,
        );
        let cases = [
            upload(&[]),
            upload(&[probe(red, -far, far, 0.)]),
            upload(&[probe(red, Vec3::splat(100.), Vec3::splat(110.), 0.)]),
            upload(&[a.clone(), b]),
            upload(&[a]),
            upload(&[probe(red, -far, far, 0.), probe(blue, -far, far, 0.)]),
            upload(&[bc6h_probe([495, 0, 0], -far, far)]),
            upload(
                &std::iter::repeat_n(
                    probe(blue, Vec3::splat(100.), Vec3::splat(110.), 0.),
                    crate::baked_specular_probe::MAX_PROBES - 1,
                )
                .chain([probe(red, -far, far, 0.)])
                .collect::<Vec<_>>(),
            ),
        ];
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
        let mut source = ReflectionSource::new(&device, size, ENVIRONMENT);
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
        let [
            sky_only,
            core,
            outside,
            pair,
            alone,
            cores,
            compressed,
            last,
        ] = cases.map(|probes| {
            let mut encoder = device.create_command_encoder(&Default::default());
            source.encode(
                &mut encoder,
                &device,
                &queue,
                Inputs {
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
                    ambient_occlusion: &visible(&device, &queue),
                    environment: Environment {
                        sky: &sky,
                        sampler: &sampler,
                        parameters: &parameters,
                        material: &material,
                        baked: &probes.view,
                        collection: &probes.metadata,
                    },
                },
                None,
            );
            queue.submit([encoder.finish()]);
            let bytes = crate::test_support::read(&device, &queue, output.texture(), 8);
            [0, 2, 4].map(|at| crate::test_support::half(&bytes[at..at + 2]))
        });
        // The mirror's response scales every case alike; compare within it.
        let response = sky_only[1] / 2.;
        let expect = |case: [f32; 3], rgb: [f32; 3], what: &str| {
            assert!(
                case.iter()
                    .zip(rgb)
                    .all(|(c, e)| (c - e * response).abs() < 0.02 * response.max(1e-3)),
                "{what}: {case:?}, expected {rgb:?} x {response}"
            );
        };
        assert!(response > 0.2, "the mirror reflects the sky: {sky_only:?}");
        expect(core, red, "inside one probe's influence");
        expect(outside, [0., 2., 0.], "outside every influence");
        expect(pair, [0.5, 0., 0.5], "half way through two probes' blends");
        expect(alone, [0.5, 1., 0.], "half way through one probe's blend");
        expect(cores, [0.5, 0., 0.5], "inside two probes' cores");
        expect(compressed, red, "inside a BC6H probe");
        expect(last, red, "inside a full collection's last probe");
    }
}

// Tiled culling (probe_culling.wgsl) keeps in each 32x32 tile the probes whose
// influence reaches the geometry it sees, through a rotated perspective
// camera: a probe around the point the left tile sees stays there and not in
// the right tile, one hidden 10 m behind that point is culled, and one around
// the screen's centre stays in both. Culled after a resize, into the new
// tiles from the new depth.
#[test]
fn probe_tiles_keep_the_probes_whose_influence_reaches_their_geometry() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = [64, 32];
    let view =
        camera::rh::view::look_at_mat4(Vec3::new(3., 2., 5.), Vec3::new(-4., 1., -2.), Vec3::Y);
    let projection = crate::perspective(1., 2., 0.1);
    let device_depth = |distance: f32| {
        let p = projection * Vec4::new(0., 0., -distance, 1.);
        p.z / p.w
    };
    // The world point a pixel sees at a view depth.
    let seen = |pixel: [f32; 2], distance: f32| {
        let ndc = Vec3::new(
            pixel[0] / size[0] as f32 * 2. - 1.,
            1. - pixel[1] / size[1] as f32 * 2.,
            device_depth(distance),
        );
        (projection * view).inverse().project_point3(ndc)
    };
    let around = |center: Vec3, half: f32| crate::BakedSpecularProbe {
        world_to_local: Mat4::from_rotation_y(0.7) * Mat4::from_translation(-center),
        influence: crate::SpecularProbeBox {
            min: Vec3::splat(-half),
            max: Vec3::splat(half),
        },
        ..probe([1.; 3], Vec3::ZERO, Vec3::ONE, 0.)
    };
    let probes = crate::scene::probes::UploadedProbes::new(
        &device,
        &queue,
        &[
            around(seen([16., 16.], 10.), 1.),
            around(seen([16., 16.], 20.), 1.),
            around(seen([32., 16.], 10.), 8.),
        ],
    )
    .unwrap();
    let depth_target = |size: [u32; 2]| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("geometry ten metres deep"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default())
    };
    let cull = |source: &mut ReflectionSource, depth: &wgpu::TextureView| {
        let mut encoder = device.create_command_encoder(&Default::default());
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(device_depth(10.)),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        }));
        source.cull(
            &mut encoder,
            &device,
            &queue,
            depth,
            reflection_camera::Camera::new(view, projection),
            &probes.metadata,
            None,
        );
        encoder
    };
    // Culled first at another size, so a group kept from then binds other
    // tiles and depth.
    let mut source = ReflectionSource::new(&device, [32, 32], ENVIRONMENT);
    queue.submit([cull(&mut source, &depth_target([32, 32])).finish()]);
    source.resize(&device, size);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: source.tiles.size(),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = cull(&mut source, &depth_target(size));
    encoder.copy_buffer_to_buffer(&source.tiles, 0, &readback, 0, source.tiles.size());
    queue.submit([encoder.finish()]);
    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let tiles: Vec<u32> = bytemuck::cast_slice(&readback.slice(..).get_mapped_range()).to_vec();
    let buckets = PROBE_BUCKETS as usize;
    let mut expected = vec![0; 2 * buckets];
    expected[0] = 0b101;
    expected[buckets] = 0b100;
    assert_eq!(tiles, expected, "left then right tile buckets");
}

// A renderer builds source completion for its settings' screen-space method
// and ambient occlusion, so a first frame from a `perspective` camera keeps
// the pipeline it was created with.
#[test]
fn first_frame_keeps_the_source_completion_built_for_the_settings() {
    use crate::settings::{
        AmbientOcclusionQuality, Antialiasing, Bloom, ReflectionMethod, ScreenSpaceReflections,
        Settings,
    };
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = [64, 48];
    let cases = [
        (
            ScreenSpaceReflections::Off,
            ReflectionMethod::Crystal,
            AmbientOcclusionQuality::Off,
        ),
        (
            ScreenSpaceReflections::Half,
            ReflectionMethod::Crystal,
            AmbientOcclusionQuality::Off,
        ),
        (
            ScreenSpaceReflections::Full,
            ReflectionMethod::Velvet,
            AmbientOcclusionQuality::Medium,
        ),
        (
            ScreenSpaceReflections::Off,
            ReflectionMethod::Crystal,
            AmbientOcclusionQuality::Low,
        ),
    ];
    for (screen_space_reflections, reflection_method, ambient_occlusion) in cases {
        let settings = Settings {
            antialiasing: Antialiasing::Off,
            bloom: Bloom::Off,
            atmosphere: false,
            ambient_occlusion,
            screen_space_reflections,
            reflection_method,
            ..Settings::default()
        };
        let mut renderer = crate::Renderer::for_test(&device, &queue, size, &settings);
        let created = renderer
            .test_reflections()
            .source
            .completion
            .pipeline
            .clone();
        let mut scene = crate::Scene::new(&device, &queue);
        let input = crate::FrameInput::new(crate::Camera {
            view: Mat4::IDENTITY,
            projection: crate::perspective(1., size[0] as f32 / size[1] as f32, 0.1),
            eye: Vec3::ZERO,
        });
        let output = crate::view::targets::target(
            &device,
            "first frame",
            size,
            crate::shading::gbuffer::COLOR,
        );
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
        assert_eq!(
            renderer.test_reflections().source.completion.pipeline,
            created,
            "{screen_space_reflections:?} {reflection_method:?} with \
             {ambient_occlusion:?} ambient occlusion rebuilt source completion",
        );
    }
}
