//! Numerical-boundary GPU fixtures for the complete production Godot trace.
#![cfg(not(target_arch = "wasm32"))]
use sgl_3d::glam::camera;
use sgl_3d::glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;
const SIZE: u32 = 128;

#[test]
fn trace_numerical_boundaries() {
    let adapter =
        pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default()));
    let Ok(adapter) = adapter else {
        assert!(
            std::env::var_os("SGL_REQUIRE_GPU").is_none(),
            "GPU required"
        );
        eprintln!("skipping GPU regression: no adapter");
        return;
    };
    eprintln!("GPU: {:?}", adapter.get_info());
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
    // Read back intermediates immediately after the production statements.
    // No shader math is reimplemented. Negative step counts select a tap
    // before traversal; positive counts execute the complete main unchanged.
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("production Godot trace"),
        source: wgpu::ShaderSource::Wgsl(
            include_str!("../src/stages/reflections/velvet/godot_reflections_trace.wgsl")
                .replace("// Add a small bias towards", "if (params.num_steps == -1) { textureStore(output_color, pixel_pos, vec4<f32>(geom_normal, 1.0)); return; }\n// Add a small bias towards")
                .replace("let facing_camera =", "if (params.num_steps == -2) { textureStore(output_color, pixel_pos, vec4<f32>(screen_ray_dir, screen_end_pos.z - screen_pos.z)); return; }\nlet facing_camera =")
                .into(),
        ),
    });
    let entries: Vec<_> = (0..9)
        .map(|binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            count: None,
            ty: match binding {
                0..=2 => wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float {
                        filterable: binding == 0,
                    },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                3..=4 => wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: if binding == 3 {
                        wgpu::TextureFormat::Rgba16Float
                    } else {
                        wgpu::TextureFormat::R32Float
                    },
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                5..=6 => wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                8 => wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::R32Float,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                _ => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            },
        })
        .collect();
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &entries,
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let texture = |format, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let input_usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
    let source = texture(wgpu::TextureFormat::Rgba8Unorm, input_usage);
    let depth = texture(wgpu::TextureFormat::R32Float, input_usage);
    let normals = texture(wgpu::TextureFormat::Rgba32Float, input_usage);
    let output = texture(
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
    );
    let mip = texture(
        wgpu::TextureFormat::R32Float,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
    );
    let reprojection = texture(
        wgpu::TextureFormat::R32Float,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
    );
    let reprojection = reprojection.create_view(&Default::default());
    let views: Vec<_> = [&source, &depth, &normals, &output, &mip]
        .map(|t| t.create_view(&Default::default()))
        .into();
    let projection = Mat4::from_scale(Vec3::new(1., -1., 1.))
        * camera::rh::proj::directx::orthographic(-1., 1., -1., 1., 10., 0.1);
    let mut scene = Vec::from(projection.to_cols_array());
    scene.extend(projection.inverse().to_cols_array());
    scene.extend(Mat4::IDENTITY.to_cols_array());
    scene.extend([0.; 4]);
    let scene_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&scene),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let params = [
        SIZE,
        SIZE,
        1,
        128,
        0f32.to_bits(),
        0f32.to_bits(),
        0.5f32.to_bits(),
        1,
    ];
    let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&params),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
    let mut bindings: Vec<_> = views
        .iter()
        .enumerate()
        .map(|(binding, view)| wgpu::BindGroupEntry {
            binding: binding as u32,
            resource: wgpu::BindingResource::TextureView(view),
        })
        .collect();
    bindings.extend([
        wgpu::BindGroupEntry {
            binding: 5,
            resource: scene_buffer.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: 6,
            resource: params_buffer.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: 7,
            resource: wgpu::BindingResource::Sampler(&sampler),
        },
        wgpu::BindGroupEntry {
            binding: 8,
            resource: wgpu::BindingResource::TextureView(&reprojection),
        },
    ]);
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &bindings,
    });
    let upload = |t: &wgpu::Texture, bytes: &[u8], stride| {
        queue.write_texture(
            t.as_image_copy(),
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SIZE * stride),
                rows_per_image: Some(SIZE),
            },
            t.size(),
        )
    };
    upload(&source, &vec![255u8; (SIZE * SIZE * 4) as usize], 4);

    let mut results = Vec::new();
    for (name, case, steps) in [
        ("sky_normal", "sky", -1i32),
        ("parallel_direction", "parallel", -2),
        ("sky_trace", "sky", 128),
        ("parallel_trace", "parallel", 128),
        ("silhouette_hit", "silhouette", 128),
        ("ordinary_hit", "ordinary", 128),
    ] {
        let projection = Mat4::from_scale(Vec3::new(1., -1., 1.))
            * if case == "sky" || case == "silhouette" {
                sgl_3d::perspective(1., 1., 0.1)
            } else {
                camera::rh::proj::directx::orthographic(-1., 1., -1., 1., 10., 0.1)
            };
        let mut scene = Vec::from(projection.to_cols_array());
        scene.extend(projection.inverse().to_cols_array());
        scene.extend(Mat4::IDENTITY.to_cols_array());
        scene.extend([0.; 4]);
        queue.write_buffer(&scene_buffer, 0, bytemuck::cast_slice(&scene));
        let mut params = params;
        params[7] = u32::from(case == "parallel" || case == "ordinary");
        params[3] = steps as u32;
        queue.write_buffer(&params_buffer, 0, bytemuck::cast_slice(&params));
        let n = if case == "parallel" {
            Vec3::new(1., 0., 1.).normalize()
        } else if case == "sky" {
            Vec3::Z
        } else {
            Vec3::new(
                (std::f32::consts::PI / 8.).sin(),
                0.,
                (std::f32::consts::PI / 8.).cos(),
            )
        };
        let mut depths = Vec::new();
        let mut ns = Vec::new();
        for _y in 0..SIZE {
            for x in 0..SIZE {
                let target = (case == "silhouette" || case == "ordinary") && x >= 64;
                let receiver = case == "parallel" || case == "ordinary" || x == 32;
                depths.push(if target {
                    projection
                        .project_point3(Vec3::new(
                            0.,
                            0.,
                            if case == "silhouette" { -2.5 } else { -4. },
                        ))
                        .z
                } else if receiver {
                    projection.project_point3(Vec3::new(0., 0., -5.)).z
                } else {
                    0.
                });
                let n = if target { -Vec3::Z } else { n };
                ns.extend([(n.x + 1.) * 0.5, (n.y + 1.) * 0.5, (n.z + 1.) * 0.5, 0.3]);
            }
        }
        upload(&depth, bytemuck::cast_slice(&depths), 4);
        upload(&normals, bytemuck::cast_slice(&ns), 16);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (SIZE * SIZE * 12) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(SIZE / 8, SIZE / 8, 1);
        }
        encoder.copy_texture_to_buffer(
            output.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(SIZE * 8),
                    rows_per_image: Some(SIZE),
                },
            },
            output.size(),
        );
        encoder.copy_texture_to_buffer(
            mip.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: (SIZE * SIZE * 8) as u64,
                    bytes_per_row: Some(SIZE * 4),
                    rows_per_image: Some(SIZE),
                },
            },
            mip.size(),
        );
        queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let bytes = buffer.get_mapped_range(..).unwrap();
        let offset = ((64 * SIZE + 32) * 8) as usize;
        let rgba: [f32; 4] =
            std::array::from_fn(|c| half(&bytes[offset + c * 2..offset + c * 2 + 2]));
        let mip_offset = ((SIZE * SIZE * 8) + (64 * SIZE + 32) * 4) as usize;
        let mip_value = f32::from_le_bytes(bytes[mip_offset..mip_offset + 4].try_into().unwrap());
        eprintln!("{name}: rgba={rgba:?}, mip={mip_value}");
        results.push((name, rgba, mip_value));
    }
    for (case, rgba, mip) in results {
        if case == "sky_normal" {
            assert!(
                (rgba[2] - 1.).abs() < 0.001,
                "silhouette must retain its receiver normal: {rgba:?}"
            );
        }
        if case == "parallel_direction" || case == "parallel_trace" {
            assert_eq!(rgba, [0.; 4], "parallel ray is a clean miss");
            assert_eq!(mip, 0., "parallel ray has no blur footprint");
        }
        if case.ends_with("_hit") {
            assert!(
                rgba[3] > 0.5 && rgba[0] > 0.25,
                "{case} must retain valid reflection: {rgba:?}"
            );
        }
        assert!(
            rgba.iter().all(|v| v.is_finite()) && mip.is_finite(),
            "{case} must write finite radiance and mip: {rgba:?} {mip}"
        );
    }
}

fn half(bytes: &[u8]) -> f32 {
    let bits = u16::from_le_bytes([bytes[0], bytes[1]]);
    let sign = if bits & 0x8000 == 0 { 1. } else { -1. };
    let exponent = i32::from((bits >> 10) & 31);
    let fraction = f32::from(bits & 1023);
    sign * match exponent {
        0 => fraction * 2f32.powi(-24),
        31 if fraction == 0. => f32::INFINITY,
        31 => f32::NAN,
        _ => (1. + fraction / 1024.) * 2f32.powi(exponent - 15),
    }
}
