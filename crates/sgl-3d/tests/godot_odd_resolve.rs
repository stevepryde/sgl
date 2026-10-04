//! Real half-resolution resolve dispatch: bilateral metadata selects a red
//! surface. Green belongs to a different depth and must never leak into it.
#![cfg(not(target_arch = "wasm32"))]
#[test]
fn half_resolution_color_matches_metadata_at_odd_even_and_minimum_sizes() {
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
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("production Godot resolve"),
        source: wgpu::ShaderSource::Wgsl(
            include_str!("../src/stages/reflections/velvet/godot_reflections_resolve.wgsl").into(),
        ),
    });
    let entries: Vec<_> = (0..8)
        .map(|binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            count: None,
            ty: match binding {
                0..=5 => wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float {
                        filterable: binding == 4,
                    },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                6 => wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba16Float,
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
        entry_point: Some("resolve_half"),
        compilation_options: Default::default(),
        cache: None,
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let mut results = Vec::new();
    for full in [
        [8u32, 8],
        [9, 8],
        [8, 9],
        [9, 9],
        [1, 1],
        [1, 8],
        [8, 1],
        [3, 3],
    ] {
        let half_size = full.map(|v| (v / 2).max(1));
        let selected = half_size.map(|v| (v - 1).min(2));
        let pixel = [0, 1].map(|i| (2 * selected[i] + 1).min(full[i] - 1));
        let texture = |size: [u32; 2], format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
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
        let full_depth = texture(full, wgpu::TextureFormat::R32Float, input_usage);
        let full_normal = texture(full, wgpu::TextureFormat::Rgba32Float, input_usage);
        let half_depth = texture(half_size, wgpu::TextureFormat::R32Float, input_usage);
        let half_normal = texture(half_size, wgpu::TextureFormat::Rgba32Float, input_usage);
        let color = texture(half_size, wgpu::TextureFormat::Rgba8Unorm, input_usage);
        let mip = texture(half_size, wgpu::TextureFormat::R32Float, input_usage);
        let output = texture(
            full,
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        );
        let upload = |t: &wgpu::Texture, bytes: &[u8], stride| {
            queue.write_texture(
                t.as_image_copy(),
                bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(t.width() * stride),
                    rows_per_image: Some(t.height()),
                },
                t.size(),
            )
        };
        let full_count = (full[0] * full[1]) as usize;
        let half_count = (half_size[0] * half_size[1]) as usize;
        upload(
            &full_depth,
            bytemuck::cast_slice(&vec![0.75f32; full_count]),
            4,
        );
        upload(
            &full_normal,
            bytemuck::cast_slice(&vec![[0.5f32, 0.5, 1., 0.]; full_count]),
            16,
        );
        upload(
            &half_normal,
            bytemuck::cast_slice(&vec![[0.5f32, 0.5, 1., 0.]; half_count]),
            16,
        );
        upload(&mip, bytemuck::cast_slice(&vec![0f32; half_count]), 4);
        let mut depths = vec![0.25f32; half_count];
        let mut colors = vec![[0u8, 64, 0, 255]; half_count];
        let index = (selected[1] * half_size[0] + selected[0]) as usize;
        depths[index] = 0.75;
        colors[index] = [64, 0, 0, 255];
        upload(&half_depth, bytemuck::cast_slice(&depths), 4);
        upload(&color, bytemuck::cast_slice(&colors), 4);
        let views = [
            &full_depth,
            &full_normal,
            &half_depth,
            &half_normal,
            &color,
            &mip,
            &output,
        ]
        .map(|t| t.create_view(&Default::default()));
        let mut bindings: Vec<_> = views
            .iter()
            .enumerate()
            .map(|(binding, view)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: wgpu::BindingResource::TextureView(view),
            })
            .collect();
        bindings.push(wgpu::BindGroupEntry {
            binding: 7,
            resource: wgpu::BindingResource::Sampler(&sampler),
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &bindings,
        });
        let row = (full[0] * 8).div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * full[1]) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(full[0].div_ceil(8), full[1].div_ceil(8), 1);
        }
        encoder.copy_texture_to_buffer(
            output.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(full[1]),
                },
            },
            output.size(),
        );
        queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let bytes = buffer.get_mapped_range(..);
        let offset = (pixel[1] * row + pixel[0] * 8) as usize;
        let rgba: [f32; 4] =
            std::array::from_fn(|c| half(&bytes[offset + c * 2..offset + c * 2 + 2]));
        eprintln!(
            "full={full:?}, traced={half_size:?}, selected={selected:?}, output={pixel:?}: {rgba:?}"
        );
        results.push((full, rgba));
    }
    for (size, rgba) in results {
        assert!(
            rgba.iter().all(|v| v.is_finite()),
            "{size:?}: finite resolve: {rgba:?}"
        );
        assert!(
            rgba[0] > 0.25 && rgba[3] > 0.99,
            "{size:?}: selected red surface remains visible: {rgba:?}"
        );
        assert!(
            rgba[1].abs() < 0.001 && rgba[2].abs() < 0.001,
            "{size:?}: different-depth green surface leaked into red: {rgba:?}"
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
