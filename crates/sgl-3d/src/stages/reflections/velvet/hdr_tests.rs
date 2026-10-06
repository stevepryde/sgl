//! Exercises the production trace, RGBA16F storage, roughness mip filter and
//! both resolve pipelines. GPU readback is linear f32, so Inf cannot be hidden
//! by presentation or a normalized output format.
use super::*;
use glam::camera;
use wgpu::util::DeviceExt;
const FULL: [u32; 2] = [128, 128];

fn texture(
    device: &wgpu::Device,
    size: [u32; 2],
    format: wgpu::TextureFormat,
    levels: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}
fn upload(queue: &wgpu::Queue, texture: &wgpu::Texture, data: &[u8], stride: u32) {
    queue.write_texture(
        texture.as_image_copy(),
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(texture.width() * stride),
            rows_per_image: Some(texture.height()),
        },
        texture.size(),
    );
}
fn dispatch(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    entries: &[(u32, wgpu::BindingResource<'_>)],
    size: [u32; 2],
) {
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &entries
            .iter()
            .map(|(binding, resource)| wgpu::BindGroupEntry {
                binding: *binding,
                resource: resource.clone(),
            })
            .collect::<Vec<_>>(),
    });
    let mut pass = encoder.begin_compute_pass(&Default::default());
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, &group, &[]);
    pass.dispatch_workgroups(size[0].div_ceil(8), size[1].div_ceil(8), 1);
}
fn read(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &wgpu::TextureView,
    size: [u32; 2],
) -> Vec<[f32; 4]> {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("HDR readback"),
        source: wgpu::ShaderSource::Wgsl(
            r#"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> pixels: array<vec4<f32>>;
@compute @workgroup_size(8,8) fn main(@builtin(global_invocation_id) id:vec3<u32>) {
 let size=textureDimensions(source); if any(id.xy>=size) {return;}
 pixels[id.y*size.x+id.x]=textureLoad(source,vec2<i32>(id.xy),0);
}"#
            .into(),
        ),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let buffer_size = u64::from(size[0] * size[1] * 16);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: buffer_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let mapped = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    dispatch(
        device,
        &mut encoder,
        &pipeline,
        &[
            (0, wgpu::BindingResource::TextureView(source)),
            (1, buffer.as_entire_binding()),
        ],
        size,
    );
    encoder.copy_buffer_to_buffer(&buffer, 0, &mapped, 0, buffer_size);
    queue.submit([encoder.finish()]);
    mapped.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    bytemuck::cast_slice(&mapped.get_mapped_range(..).unwrap()).to_vec()
}

#[test]
fn finite_hdr_survives_trace_filter_storage_and_both_resolves() {
    let adapter =
        pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default()));
    let Ok(adapter) = adapter else {
        assert!(
            std::env::var_os("SGL_REQUIRE_GPU").is_none(),
            "GPU required"
        );
        eprintln!("skipping HDR GPU test: no adapter");
        return;
    };
    eprintln!("GPU: {:?}", adapter.get_info());
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
    let pipeline = Velvet::new(&device);
    let projection = Mat4::from_scale(glam::Vec3::new(1., -1., 1.))
        * camera::rh::proj::directx::orthographic(-1., 1., -1., 1., 10., 0.1);
    queue.write_buffer(
        &pipeline.scene_data,
        0,
        bytemuck::bytes_of(&SceneData {
            projection: projection.to_cols_array_2d(),
            inv_projection: projection.inverse().to_cols_array_2d(),
            reprojection: Mat4::IDENTITY.to_cols_array_2d(),
            eye_offset: [0.; 4],
        }),
    );
    let mut results = Vec::new();
    let mut filter_results = Vec::new();
    for half in [false, true] {
        let view = wgpu::BindingResource::TextureView;
        let size = FULL.map(|v| if half { v / 2 } else { v });
        let levels = size[0].ilog2() + 1;
        let full_depth = texture(&device, FULL, wgpu::TextureFormat::R32Float, 1);
        let full_normal = texture(&device, FULL, wgpu::TextureFormat::Rgba16Float, 1);
        let depth = texture(&device, size, wgpu::TextureFormat::R32Float, levels);
        let depth_views = targets::mip_views(&depth, levels);
        let depth_view = depth.create_view(&Default::default());
        let normal = texture(&device, size, wgpu::TextureFormat::Rgba16Float, 1);
        let normal_view = normal.create_view(&Default::default());
        let radiance = texture(&device, FULL, wgpu::TextureFormat::Rgba16Float, 1);
        let source = radiance.create_view(&Default::default());
        let ssr = texture(&device, size, wgpu::TextureFormat::Rgba16Float, levels);
        let ssr_views = targets::mip_views(&ssr, levels);
        let ssr_view = ssr.create_view(&Default::default());
        let mip = texture(&device, size, wgpu::TextureFormat::R32Float, 1)
            .create_view(&Default::default());
        let reprojection = texture(&device, size, wgpu::TextureFormat::R32Float, 1)
            .create_view(&Default::default());
        let output = texture(&device, FULL, wgpu::TextureFormat::Rgba16Float, 1)
            .create_view(&Default::default());
        let full_depth_view = full_depth.create_view(&Default::default());
        let full_normal_view = full_normal.create_view(&Default::default());
        // A tilted receiver reflects a nearer opaque target. The same geometry
        // is supplied at full size and downsampled by the actual half pass.
        let make_depth = |size: [u32; 2]| -> Vec<f32> {
            (0..size[1])
                .flat_map(|_| {
                    (0..size[0]).map(|x| {
                        projection
                            .project_point3(glam::Vec3::new(
                                0.,
                                0.,
                                if x < size[0] / 2 { -5. } else { -4. },
                            ))
                            .z
                    })
                })
                .collect()
        };
        upload(
            &queue,
            &full_depth,
            bytemuck::cast_slice(&make_depth(FULL)),
            4,
        );
        upload(&queue, &depth, bytemuck::cast_slice(&make_depth(size)), 4);
        queue.write_buffer(
            &pipeline.trace_params,
            0,
            bytemuck::bytes_of(&TraceParams {
                screen_size: size.map(|v| v as i32),
                mipmaps: levels as i32,
                num_steps: 128,
                distance_fade: 0.,
                curve_fade_in: 0.,
                depth_tolerance: 0.5,
                orthogonal: 1,
            }),
        );
        // IEEE binary16 encodings of authored linear HDR radiance.
        for (brightness, bits, roughness, striped) in [
            ([1f32; 3], [0x3c00u16; 3], 0., false),
            ([4.; 3], [0x4400; 3], 0., false),
            ([32.; 3], [0x5000; 3], 0., false),
            ([4096.; 3], [0x6c00; 3], 0., false),
            ([65504.; 3], [0x7bff; 3], 0., false),
            ([1.; 3], [0x3c00; 3], 0.5, false),
            ([4.; 3], [0x4400; 3], 0.5, false),
            ([32.; 3], [0x5000; 3], 0.5, false),
            ([4096.; 3], [0x6c00; 3], 0.5, false),
            ([65504.; 3], [0x7bff; 3], 0.5, false),
            ([65504., 4096., 32.], [0x7bff, 0x6c00, 0x5000], 0., false),
            ([65504., 4096., 32.], [0x7bff, 0x6c00, 0x5000], 0.5, false),
            ([4.; 3], [0x4400; 3], 0.5, true),
        ] {
            let make_normals = |size: [u32; 2]| -> Vec<[u16; 4]> {
                (0..size[1])
                    .flat_map(|_| {
                        (0..size[0]).map(|x| {
                            if x < size[0] / 2 {
                                [
                                    0x3a00,
                                    0x3800,
                                    0x3c00,
                                    if roughness == 0. { 0 } else { 0x3800 },
                                ]
                            } else {
                                [0x3800, 0x3800, 0, 0]
                            }
                        })
                    })
                    .collect()
            };
            upload(
                &queue,
                &full_normal,
                bytemuck::cast_slice(&make_normals(FULL)),
                8,
            );
            upload(
                &queue,
                &normal,
                bytemuck::cast_slice(&make_normals(size)),
                8,
            );
            let colors: Vec<[u16; 4]> = (0..FULL[1])
                .flat_map(|y| {
                    (0..FULL[0]).map(move |_| {
                        let rgb = if striped && (y / 2) % 2 == 0 {
                            [0x3c00; 3]
                        } else {
                            bits
                        };
                        [rgb[0], rgb[1], rgb[2], 0x3c00]
                    })
                })
                .collect();
            upload(&queue, &radiance, bytemuck::cast_slice(&colors), 8);
            let mut encoder = device.create_command_encoder(&Default::default());
            if half {
                dispatch(
                    &device,
                    &mut encoder,
                    &pipeline.downsample,
                    &[
                        (0, view(&full_depth_view)),
                        (1, view(&full_normal_view)),
                        (2, view(&depth_views[0])),
                        (3, view(&normal_view)),
                    ],
                    size,
                );
            }
            for m in 1..levels {
                dispatch(
                    &device,
                    &mut encoder,
                    &pipeline.hiz,
                    &[
                        (0, view(&depth_views[m as usize - 1])),
                        (1, view(&depth_views[m as usize])),
                    ],
                    size.map(|v| (v >> m).max(1)),
                );
            }
            dispatch(
                &device,
                &mut encoder,
                &pipeline.trace,
                &[
                    (0, view(&source)),
                    (1, view(&depth_view)),
                    (2, view(&normal_view)),
                    (3, view(&ssr_views[0])),
                    (4, view(&mip)),
                    (5, pipeline.scene_data.as_entire_binding()),
                    (6, pipeline.trace_params.as_entire_binding()),
                    (7, wgpu::BindingResource::Sampler(&pipeline.linear)),
                    (8, view(&reprojection)),
                ],
                size,
            );
            for m in 1..levels {
                let filtered_size = size.map(|v| (v >> m).max(1));
                let parameters = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: bytemuck::bytes_of(&FilterParams {
                        screen_size: filtered_size.map(|v| v as i32),
                        mip_level: m,
                        pad: 0,
                    }),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                dispatch(
                    &device,
                    &mut encoder,
                    &pipeline.filter,
                    &[
                        (0, view(&ssr_views[m as usize - 1])),
                        (1, view(&ssr_views[m as usize])),
                        (2, parameters.as_entire_binding()),
                        (3, wgpu::BindingResource::Sampler(&pipeline.linear)),
                    ],
                    filtered_size,
                );
            }
            let mut entries = vec![
                (4, view(&ssr_view)),
                (5, view(&mip)),
                (6, view(&output)),
                (7, wgpu::BindingResource::Sampler(&pipeline.linear)),
            ];
            if half {
                entries.extend([
                    (0, view(&full_depth_view)),
                    (1, view(&full_normal_view)),
                    (2, view(&depth_views[0])),
                    (3, view(&normal_view)),
                ]);
            }
            dispatch(
                &device,
                &mut encoder,
                if half {
                    &pipeline.resolve_half
                } else {
                    &pipeline.resolve_full
                },
                &entries,
                FULL,
            );
            queue.submit([encoder.finish()]);
            let resolved = read(&device, &queue, &output, FULL);
            let traced = read(&device, &queue, &ssr_views[0], size);
            let filtered = read(&device, &queue, &ssr_views[1], size.map(|v| v / 2));
            let center = (FULL[1] / 2 * FULL[0] + FULL[0] / 4) as usize;
            let nonfinite = resolved
                .iter()
                .chain(&traced)
                .chain(&filtered)
                .flatten()
                .filter(|v| !v.is_finite())
                .count();
            let hit_count = resolved.iter().filter(|c| c[3] > 0.1).count();
            let energy_error = resolved
                .iter()
                .filter(|c| c[3] > 0.1)
                .flat_map(|c| {
                    (0..3).map(move |channel| {
                        (c[channel] - brightness[channel] * c[3]).abs() / brightness[channel]
                    })
                })
                .fold(0f32, f32::max);
            let hit_luminance = resolved
                .iter()
                .filter(|c| c[3] > 0.1)
                .map(|c| (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]) / c[3])
                .sum::<f32>()
                / hit_count.max(1) as f32;
            eprintln!(
                "half={half} HDR={brightness:?} roughness={roughness} striped={striped}: resolved={:?} trace={:?} nonfinite={nonfinite} hits={hit_count} energy_error={energy_error} luminance={hit_luminance}",
                resolved[center],
                traced[(size[1] / 2 * size[0] + size[0] / 4) as usize]
            );
            if striped {
                let column = |pixels: &[[f32; 4]], size: [u32; 2]| -> Vec<f32> {
                    (size[1] / 4..size[1] * 3 / 4)
                        .map(|y| {
                            let c = pixels[(y * size[0] + size[0] / 4) as usize];
                            c[0] / c[3]
                        })
                        .collect()
                };
                filter_results.push((
                    half,
                    column(&traced, size),
                    column(&filtered, size.map(|v| v / 2)),
                ));
            } else {
                results.push((
                    half,
                    brightness,
                    roughness,
                    nonfinite,
                    hit_count,
                    energy_error,
                    hit_luminance,
                ));
            }
        }
    }
    // Godot filters and resolves luminance-tone-mapped radiance, so blurred
    // edges are not energy preserving by design. A mirror hit's tone map and
    // inverse round-trip within one binary16 step (2^-11 below 1.0) of the
    // stored value t = L / (1 + L), which the inverse scales by (1 + L)^2.
    let ordinary = |brightness: [f32; 3]| brightness.iter().all(|&v| v <= 32.);
    for &(half, brightness, roughness, nonfinite, hits, error, luminance) in &results {
        assert_eq!(
            nonfinite, 0,
            "half={half} HDR={brightness:?} roughness={roughness}: finite HDR must remain finite"
        );
        assert!(hits > 0, "fixture must exercise real hits");
        if roughness == 0. && ordinary(brightness) {
            let bound = (1. + brightness[0]).powi(2) / brightness[0] / 2048.;
            assert!(
                error <= bound,
                "half={half} HDR={brightness:?} roughness={roughness}: HDR energy error {error} above {bound}"
            );
        }
        // A brighter source never reflects darker than an ordinary one.
        if !ordinary(brightness) {
            let reference = results
                .iter()
                .find(|r| r.0 == half && r.1 == [32.; 3] && r.2 == roughness)
                .unwrap()
                .6;
            assert!(
                luminance >= reference,
                "half={half} HDR={brightness:?} roughness={roughness}: luminance {luminance} below the 32 source's {reference}"
            );
        }
    }
    for (half, raw, filtered) in filter_results {
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
        let spread = |v: &[f32]| {
            v.iter().copied().fold(f32::NEG_INFINITY, f32::max)
                - v.iter().copied().fold(f32::INFINITY, f32::min)
        };
        eprintln!(
            "half={half} stripe raw mean={} spread={}, filtered mean={} spread={}",
            mean(&raw),
            spread(&raw),
            mean(&filtered),
            spread(&filtered)
        );
        // Both mips hold tone-mapped radiance; the ratio needs raw contrast.
        assert!(
            spread(&filtered) < spread(&raw) * 0.5,
            "roughness filtering must reduce high-frequency contrast"
        );
    }
}
