//! Exercises the production temporal pass on a still view whose traced hits
//! move between neighbouring pixels every frame, as TAA's jitter moves hits on
//! thin bright geometry.
use super::*;

// 32 texels of RGBA16F fill one 256-byte copy row.
const SIZE: u32 = 32;
// Binary16 encodings of the traced (tone-mapped) values.
const BRIGHT: u16 = 0x3a66; // 0.7998
const DARK: u16 = 0x2a66; // 0.04999
const ONE: u16 = 0x3c00;
const HALF: u16 = 0x3800;
const ROUGHNESS: u16 = 0x3266; // 0.2

fn value(bits: u16) -> f32 {
    crate::test_support::half(&bits.to_le_bytes())
}

fn texture(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::Texture {
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
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn upload(queue: &wgpu::Queue, texture: &wgpu::Texture, bytes: &[u8], stride: u32) {
    queue.write_texture(
        texture.as_image_copy(),
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIZE * stride),
            rows_per_image: Some(SIZE),
        },
        texture.size(),
    );
}

fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<[f32; 4]> {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(SIZE * SIZE * 8),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SIZE * 8),
                rows_per_image: Some(SIZE),
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let mapped = buffer.get_mapped_range(..);
    mapped
        .chunks(8)
        .map(|texel| std::array::from_fn(|c| crate::test_support::half(&texel[c * 2..c * 2 + 2])))
        .collect()
}

/// Every 3x3 neighbourhood holds hits and misses; the hits shift each frame.
fn hits(frame: u32) -> Vec<u16> {
    (0..SIZE * SIZE)
        .flat_map(|i| {
            let (x, y) = (i % SIZE, i / SIZE);
            let bits = if (x + y + frame).is_multiple_of(3) {
                BRIGHT
            } else {
                DARK
            };
            [bits, bits, bits, ONE]
        })
        .collect()
}

#[test]
fn jittered_hits_settle_and_a_reset_passes_through() {
    let adapter =
        pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default()));
    let Ok(adapter) = adapter else {
        assert!(
            std::env::var_os("SGL_REQUIRE_GPU").is_none(),
            "GPU required"
        );
        eprintln!("skipping temporal GPU test: no adapter");
        return;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
    let pipeline = Velvet::new(&device);
    let hdr = wgpu::TextureFormat::Rgba16Float;
    let r32 = wgpu::TextureFormat::R32Float;
    let current = texture(&device, hdr);
    let output = texture(&device, hdr);
    let history = [texture(&device, hdr), texture(&device, hdr)];
    let depth_history = [texture(&device, r32), texture(&device, r32)];
    let depth = texture(&device, r32);
    let reprojection = texture(&device, r32);
    let motion = texture(&device, hdr);
    let normal_roughness = texture(&device, hdr);
    // A still receiver plane at one depth, traced (roughness below 0.7).
    let plane = vec![0.5f32; (SIZE * SIZE) as usize];
    upload(&queue, &depth, bytemuck::cast_slice(&plane), 4);
    upload(&queue, &reprojection, bytemuck::cast_slice(&plane), 4);
    upload(&queue, &motion, &vec![0; (SIZE * SIZE * 8) as usize], 8);
    let normals = [HALF, HALF, ONE, ROUGHNESS].repeat((SIZE * SIZE) as usize);
    upload(&queue, &normal_roughness, bytemuck::cast_slice(&normals), 8);
    let view = |t: &wgpu::Texture| t.create_view(&Default::default());
    let encode = |frame: u32, continues: bool| -> Vec<[f32; 4]> {
        upload(&queue, &current, bytemuck::cast_slice(&hits(frame)), 8);
        let size = SIZE as f32;
        queue.write_buffer(
            &pipeline.temporal_params,
            0,
            bytemuck::bytes_of(&TemporalParams {
                inverse_view_projection: Mat4::IDENTITY.to_cols_array_2d(),
                previous_view_projection: Mat4::IDENTITY.to_cols_array_2d(),
                size: [size, size, 1. / size, 1. / size],
                near: 0.1,
                flags: if continues { TEMPORAL_CONTINUES } else { 0 },
                padding: [0; 2],
            }),
        );
        let (write, read_from) = ((frame % 2) as usize, 1 - (frame % 2) as usize);
        let entries = [
            (0, view(&current)),
            (1, view(&history[read_from])),
            (2, view(&reprojection)),
            (3, view(&motion)),
            (4, view(&depth)),
            (5, view(&depth_history[read_from])),
            (6, view(&normal_roughness)),
            (9, view(&output)),
            (10, view(&history[write])),
            (11, view(&depth_history[write])),
        ];
        let mut bindings: Vec<_> = entries
            .iter()
            .map(|(binding, v)| wgpu::BindGroupEntry {
                binding: *binding,
                resource: wgpu::BindingResource::TextureView(v),
            })
            .collect();
        bindings.push(wgpu::BindGroupEntry {
            binding: 7,
            resource: wgpu::BindingResource::Sampler(&pipeline.linear),
        });
        bindings.push(wgpu::BindGroupEntry {
            binding: 8,
            resource: pipeline.temporal_params.as_entire_binding(),
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.temporal.get_bind_group_layout(0),
            entries: &bindings,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline.temporal);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(SIZE / 8, SIZE / 8, 1);
        }
        queue.submit([encoder.finish()]);
        read(&device, &queue, &output)
    };
    // Continuing history settles: a pixel's hit/miss toggling no longer
    // reaches the output frame to frame.
    let mut previous = encode(0, false);
    let mut last_change = 0f32;
    for frame in 1..90 {
        let result = encode(frame, true);
        last_change = result
            .iter()
            .zip(&previous)
            .map(|(a, b)| (a[0] - b[0]).abs())
            .fold(0., f32::max);
        for texel in &result {
            assert!(
                texel[0] >= value(DARK) - 0.01 && texel[0] <= value(BRIGHT) + 0.01,
                "accumulated hit {texel:?} left the traced range"
            );
        }
        previous = result;
    }
    eprintln!("largest per-frame change after settling: {last_change}");
    assert!(
        last_change < 0.1 * (value(BRIGHT) - value(DARK)),
        "hits still toggle frame to frame by {last_change}"
    );
    // A reset (a camera cut) returns this frame's hits, not the history.
    let reset = encode(90, false);
    let expected = hits(90);
    for (i, texel) in reset.iter().enumerate() {
        assert_eq!(
            texel[0],
            value(expected[i * 4]),
            "reset must pass through the traced hit at {i}"
        );
    }
}
