//! A ray that runs out of traversal lookups reports a miss (AR-12), however
//! near a surface it ends; one that finishes within its budget keeps the
//! visible plane's radiance.
#![cfg(not(target_arch = "wasm32"))]
use sgl_post_fx::{CameraAttribs, ScreenSpaceReflectionAttribs};
use wgpu::util::DeviceExt;

#[test]
fn rays_that_run_out_of_lookups_miss() {
    let adapter =
        match pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default())) {
            Ok(adapter) => adapter,
            Err(error) => {
                assert!(
                    !std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0"),
                    "GPU required: {error}"
                );
                eprintln!("skipping GPU test: {error}");
                return;
            }
        };
    eprintln!("adapter: {:?}", adapter.get_info());
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
    let mut source = sgl_post_fx::shaders::shader_source(
        "SSR_ComputeIntersection.fx",
        &[
            ("SUPPORTED_SHADER_SRV", "1"),
            ("SSR_OPTION_INVERTED_DEPTH", "0"),
            ("SSR_OPTION_PREVIOUS_FRAME", "0"),
            ("SSR_OPTION_HALF_RESOLUTION", "0"),
        ],
    );
    source.push_str(r#"
@group(0) @binding(8) var<storage, read_write> results: array<vec4<f32>>;
@compute @workgroup_size(1)
fn regression(@builtin(global_invocation_id) id: vec3<u32>) {
    let origin = vec3<f32>(select(0.25, 0.999, id.x == 5u), 0.5, 0.5);
    let direction = vec3<f32>(0.5, 0.01, 0.05);
    var valid = false;
    let hit = HierarchicalRaymarch(origin, direction, vec2<f32>(64.0), 0, id.x, &valid);
    let confidence = ValidateHit(hit, origin.xy, normalize(direction), vec2<f32>(64.0), g_SSRAttribs.DepthBufferThickness);
    results[id.x * 2u] = vec4<f32>(hit, select(0.0, 1.0, valid));
    let accepted = select(0.0, confidence, valid);
    let radiance = LoadRadiance(vec2<i32>(hit.xy * 64.0));
    results[id.x * 2u + 1u] = vec4<f32>(radiance * accepted, accepted);
}
"#);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("production SSR budget regression"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    // Left-handed perspective: view z = 1 / (1 - depth).
    let camera = CameraAttribs {
        m_proj: [
            1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 1., 0., 0., -1., 0.,
        ],
        ..Default::default()
    };
    let attrs = ScreenSpaceReflectionAttribs::default();
    let uniform = |data: &[u8]| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: data,
            usage: wgpu::BufferUsages::UNIFORM,
        })
    };
    let camera = uniform(bytemuck::bytes_of(&camera));
    let attrs = uniform(bytemuck::bytes_of(&attrs));
    let texture = |format, channels: usize, pixel: &[f32], levels: u32| {
        let data: Vec<f32> = (0..levels)
            .flat_map(|mip| {
                pixel
                    .iter()
                    .copied()
                    .cycle()
                    .take(channels * (64usize >> mip).pow(2))
            })
            .collect();
        device
            .create_texture_with_data(
                &queue,
                &wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d {
                        width: 64,
                        height: 64,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: levels,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                bytemuck::cast_slice(&data),
            )
            .create_view(&Default::default())
    };
    // At x=18.5 pixels the ray meets this plane. One step crosses the next
    // pixel boundary; two steps reach the plane at mip 1; three confirm mip 0.
    let surface_depth = 0.50390625f32;
    let depth = texture(wgpu::TextureFormat::R32Float, 1, &[surface_depth], 7);
    let normal = texture(wgpu::TextureFormat::Rgba32Float, 4, &[0., 0., -1., 0.], 1);
    // Only the independently calculated intersection pixel emits red. Adjacent
    // pixels are green, so a wrong endpoint cannot satisfy the radiance check.
    let radiance_data: Vec<f32> = (0..64 * 64)
        .flat_map(|i| {
            if i % 64 == 18 {
                [1., 0., 0., 1.]
            } else {
                [0., 1., 0., 1.]
            }
        })
        .collect();
    let radiance = device
        .create_texture_with_data(
            &queue,
            &wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: 64,
                    height: 64,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&radiance_data),
        )
        .create_view(&Default::default());
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 6 * 2 * 16,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let entries = [0, 1, 2, 3, 7, 8].map(|binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        count: None,
        ty: match binding {
            0 | 1 => wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            8 => wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            _ => wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
        },
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &entries,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            }),
        ),
        module: &module,
        entry_point: Some("regression"),
        compilation_options: Default::default(),
        cache: None,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: camera.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: attrs.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&radiance),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&normal),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: wgpu::BindingResource::TextureView(&depth),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: output.as_entire_binding(),
            },
        ],
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: output.size(),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(6, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, output.size());
    queue.submit([encoder.finish()]);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let mapped = readback.slice(..).get_mapped_range().unwrap();
    let result: &[[f32; 4]] = bytemuck::cast_slice(&mapped);
    eprintln!("budget rows (endpoint xyz, accepted; premultiplied radiance): {result:?}");
    // Two lookups end the ray on the plane, but at mip 1, unconfirmed.
    for budget in 0..=2 {
        assert_eq!(
            result[budget * 2 + 1],
            [0.; 4],
            "ray out of lookups accepted at budget {budget}"
        );
    }
    for budget in 3..=4 {
        let rgba = result[budget * 2 + 1];
        assert!(
            rgba[0] > 0.9 && rgba[1] == 0. && rgba[2] == 0. && rgba[3] > 0.9,
            "visible red plane lost at budget {budget}: {rgba:?}"
        );
    }
    assert_eq!(result[11], [0.; 4], "offscreen ray supplied radiance");
}
