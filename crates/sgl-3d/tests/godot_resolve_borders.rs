//! The native GPU sampler is the oracle for texel-center neighborhoods and
//! weights. A second entry point exercises production metadata sampling at
//! out-of-range coordinates; no CPU copy of the bilateral filter is used.
#![cfg(not(target_arch = "wasm32"))]
#[test]
fn resolve_borders_match_hardware_filtering_and_clamp_metadata() {
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
            (include_str!("../src/stages/reflections/velvet/godot_reflections_resolve.wgsl")
                .to_owned()
                + PROBES)
                .into(),
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
    let pipelines = ["resolve_half", "hardware_reference", "edge_metadata"].map(|entry| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        })
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let mut results = Vec::new();
    for full in [[8u32, 8], [9, 8], [8, 9], [9, 9]] {
        let half_size = [4, 4];
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
        let depths = vec![0.75f32; half_count];
        // Distinct confidence at every texel; RGB is zero so tone-map inversion
        // cannot change the native sampler comparison.
        let colors: Vec<_> = (0..half_count)
            .map(|i| [0u8, 0, 0, 16 + i as u8 * 11])
            .collect();
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
        let mut frames = Vec::new();
        for (pass_index, pipeline) in pipelines.iter().enumerate() {
            if pass_index == 2 {
                // Distinct edge metadata exposes a color/metadata coordinate
                // mismatch even when the wrong metadata texel is in bounds.
                let depths: Vec<_> = (0..half_count).map(|i| 0.125f32 + i as f32 / 32.).collect();
                upload(&half_depth, bytemuck::cast_slice(&depths), 4);
            }
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
                pass.set_pipeline(pipeline);
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
            let alpha: Vec<_> = (0..full[1])
                .flat_map(|y| (0..full[0]).map(move |x| (y * row + x * 8 + 6) as usize))
                .map(|offset| half(&bytes[offset..offset + 2]))
                .collect();
            assert!(
                alpha.iter().all(|value| value.is_finite()),
                "non-finite GPU output"
            );
            frames.push(alpha);
        }
        let worst = frames[0]
            .iter()
            .zip(&frames[1])
            .map(|(actual, expected)| (actual - expected).abs())
            .fold(0f32, f32::max);
        // Left, right, top, bottom, then four corners. The authored edge
        // confidence values are read directly, not evaluated by a test filter.
        let expected_indices = [4usize, 7, 1, 13, 0, 3, 12, 15];
        let metadata_error = expected_indices
            .iter()
            .enumerate()
            .map(|(i, &texel)| (frames[2][i] - colors[texel][3] as f32 / 255.).abs())
            .fold(0f32, f32::max);
        eprintln!(
            "{full:?}: maximum native sampler difference={worst}; edge metadata difference={metadata_error}; corner alpha={} expected={}",
            frames[0][0], frames[1][0]
        );
        results.push((full, worst, metadata_error));
    }
    for (size, difference, metadata_error) in results {
        assert!(
            difference < 0.001,
            "{size:?}: resolve differs from GPU texel-center filtering by {difference}"
        );
        assert!(
            metadata_error < 0.001,
            "{size:?}: color and metadata disagree at borders by {metadata_error}"
        );
    }
}

// These probes append entry points, without changing the production source.
// The oracle delegates interpolation and addressing to GPU texture hardware.
const PROBES: &str = r#"
@compute @workgroup_size(8, 8, 1)
fn hardware_reference(@builtin(global_invocation_id) id: vec3<u32>) {
    if (any(id.xy >= textureDimensions(output_color))) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / (2.0 * vec2<f32>(textureDimensions(source_color)));
    textureStore(output_color, vec2<i32>(id.xy), textureSampleLevel(source_color, linear_sampler, uv, 0.0));
}
@compute @workgroup_size(8, 8, 1)
fn edge_metadata(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= 8u || id.y != 0u) { return; }
    let positions = array<vec2<i32>, 8>(vec2<i32>(-1, 1), vec2<i32>(4, 1), vec2<i32>(1, -1), vec2<i32>(1, 4), vec2<i32>(-1, -1), vec2<i32>(4, -1), vec2<i32>(-1, 4), vec2<i32>(4, 4));
    var color: vec4<f32>;
    var weight: f32;
    let depths = array<f32, 8>(0.25, 0.34375, 0.15625, 0.53125, 0.125, 0.21875, 0.5, 0.59375);
    get_sample(depths[id.x], vec3<f32>(0.0, 0.0, 1.0), 0.0, positions[id.x], &color, &weight);
    textureStore(output_color, vec2<i32>(id.xy), color * weight);
}
"#;

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
