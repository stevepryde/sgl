//! Real Godot trace dispatch on visible geometry with grazing mapped normals.
#![cfg(not(target_arch = "wasm32"))]
use sgl_3d::glam::camera;
use sgl_3d::glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;
const SIZE: u32 = 128;

#[test]
fn grazing_mapped_normals_keep_distant_visible_targets() {
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
        label: Some("production Godot trace"),
        source: wgpu::ShaderSource::Wgsl(
            include_str!("../src/stages/reflections/velvet/godot_reflections_trace.wgsl").into(),
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
        wgpu::TextureUsages::STORAGE_BINDING,
    );
    let reprojection = texture(
        wgpu::TextureFormat::R32Float,
        wgpu::TextureUsages::STORAGE_BINDING,
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
        usage: wgpu::BufferUsages::UNIFORM,
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
        usage: wgpu::BufferUsages::UNIFORM,
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
    let source_pixels: Vec<u8> = (0..SIZE * SIZE)
        .flat_map(|i| {
            if i % SIZE >= 64 {
                [255, 0, 0, 255]
            } else {
                [0, 255, 0, 255]
            }
        })
        .collect();
    upload(&source, &source_pixels, 4);

    // A camera-visible receiver slopes down to the right by 67.5 degrees.
    // Its geometric and shading normals agree, so a -Z camera ray reflects
    // equally along +X and -Z without an origin bias or second bounce.
    // The target is a visible horizontal plane below it (+Z geometric normal).
    // Its mapped normal stays in the camera-facing hemisphere but tips past
    // the reflected ray's tangent: rejecting it would erase real target radiance.
    let normal = Vec3::new(
        (3. * std::f32::consts::PI / 8.).sin(),
        0.,
        (3. * std::f32::consts::PI / 8.).cos(),
    );
    let mut results = Vec::new();
    let distance = 32u32;
    for mapped in [false, true] {
        let target_x = 32 + distance;
        let target_z = -5. - distance as f32 * 2. / SIZE as f32;
        let mut depths = Vec::new();
        let mut ns = Vec::new();
        for _y in 0..SIZE {
            for x in 0..SIZE {
                let receiver_z = -5. - (x as f32 - 32.) * 2. / SIZE as f32 * (normal.x / normal.z);
                let target = x >= target_x;
                let z = if target { target_z } else { receiver_z };
                depths.push(projection.project_point3(Vec3::new(0., 0., z)).z);
                let n = if target {
                    if mapped { normal } else { Vec3::Z }
                } else {
                    normal
                };
                ns.extend([(n.x + 1.) * 0.5, (n.y + 1.) * 0.5, (n.z + 1.) * 0.5, 0.]);
            }
        }
        upload(&depth, bytemuck::cast_slice(&depths), 4);
        upload(&normals, bytemuck::cast_slice(&ns), 16);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (SIZE * SIZE * 8) as u64,
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
        queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let bytes = buffer.get_mapped_range(..);
        let offset = ((64 * SIZE + 32) * 8) as usize;
        let rgba: [f32; 4] =
            std::array::from_fn(|c| half(&bytes[offset + c * 2..offset + c * 2 + 2]));
        eprintln!("distance={distance}px mapped={mapped}: rgba={rgba:?}");
        results.push((mapped, rgba));
    }
    for (mapped, rgba) in results {
        // The trace stores Godot's luminance tone map of the unit red target
        // (1 / (1 + 0.2126)); a lost target is zero.
        assert!(
            rgba[0] > 0.5 && rgba[1] < 0.01 && rgba[2] < 0.01 && rgba[3] > 0.9,
            "visible red target lost, mapped={mapped}: {rgba:?}"
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
