//! Exercises the production temporal pass: on a still view whose traced hits
//! move between neighbouring pixels every frame, as TAA's jitter moves hits on
//! thin bright geometry, and after a turn that puts the reflections' hits
//! behind the previous camera.
use super::*;
use glam::Vec3;

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

/// The production temporal pass over a still receiver plane at depth 0.5,
/// traced (roughness 0.2), with no motion and its two history slots.
struct Pass {
    pipeline: Velvet,
    current: wgpu::Texture,
    output: wgpu::Texture,
    history: [wgpu::Texture; 2],
    depth_history: [wgpu::Texture; 2],
    depth: wgpu::Texture,
    reprojection: wgpu::Texture,
    motion: wgpu::Texture,
    normal_roughness: wgpu::Texture,
}

impl Pass {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let hdr = wgpu::TextureFormat::Rgba16Float;
        let r32 = wgpu::TextureFormat::R32Float;
        let pass = Self {
            pipeline: Velvet::new(device),
            current: texture(device, hdr),
            output: texture(device, hdr),
            history: [texture(device, hdr), texture(device, hdr)],
            depth_history: [texture(device, r32), texture(device, r32)],
            depth: texture(device, r32),
            reprojection: texture(device, r32),
            motion: texture(device, hdr),
            normal_roughness: texture(device, hdr),
        };
        upload(queue, &pass.depth, bytemuck::cast_slice(&PLANE), 4);
        upload(queue, &pass.reprojection, bytemuck::cast_slice(&PLANE), 4);
        upload(queue, &pass.motion, &[0; (SIZE * SIZE * 8) as usize], 8);
        let normals = [HALF, HALF, ONE, ROUGHNESS].repeat((SIZE * SIZE) as usize);
        upload(
            queue,
            &pass.normal_roughness,
            bytemuck::cast_slice(&normals),
            8,
        );
        pass
    }

    /// One frame over `current`, RGBA16F texels, reading the history in slot
    /// `1 - write` and writing slot `write`: the frame's output.
    fn run(
        &self,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
        current: &[u16],
        params: TemporalParams,
        write: usize,
    ) -> Vec<[f32; 4]> {
        upload(queue, &self.current, bytemuck::cast_slice(current), 8);
        queue.write_buffer(
            &self.pipeline.temporal_params,
            0,
            bytemuck::bytes_of(&params),
        );
        let view = |t: &wgpu::Texture| t.create_view(&Default::default());
        let entries = [
            (0, view(&self.current)),
            (1, view(&self.history[1 - write])),
            (2, view(&self.reprojection)),
            (3, view(&self.motion)),
            (4, view(&self.depth)),
            (5, view(&self.depth_history[1 - write])),
            (6, view(&self.normal_roughness)),
            (9, view(&self.output)),
            (10, view(&self.history[write])),
            (11, view(&self.depth_history[write])),
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
            resource: wgpu::BindingResource::Sampler(&self.pipeline.linear),
        });
        bindings.push(wgpu::BindGroupEntry {
            binding: 8,
            resource: self.pipeline.temporal_params.as_entire_binding(),
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.pipeline.temporal.get_bind_group_layout(0),
            entries: &bindings,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline.temporal);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(SIZE / 8, SIZE / 8, 1);
        }
        queue.submit([encoder.finish()]);
        read(device, queue, &self.output)
    }
}

/// The receiver plane's device depth at every texel.
const PLANE: [f32; (SIZE * SIZE) as usize] = [0.5; (SIZE * SIZE) as usize];

/// A still camera whose history `continues`.
fn still(continues: bool) -> TemporalParams {
    let size = SIZE as f32;
    TemporalParams {
        inverse_view_projection: Mat4::IDENTITY.to_cols_array_2d(),
        previous_view_projection: Mat4::IDENTITY.to_cols_array_2d(),
        size: [size, size, 1. / size, 1. / size],
        near: 0.1,
        flags: if continues { TEMPORAL_CONTINUES } else { 0 },
        padding: [0; 2],
    }
}

#[test]
fn jittered_hits_settle_and_a_reset_passes_through() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let pass = Pass::new(&device, &queue);
    let encode = |frame: u32, continues: bool| {
        pass.run(
            (&device, &queue),
            &hits(frame),
            still(continues),
            (frame % 2) as usize,
        )
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

// A reflection's virtual hit point on or behind the previous camera's plane
// was nowhere on its screen. After a half turn every hit point ahead of the
// camera, on the plane 5 m deep (view z = -5), lies behind the previous one,
// where dividing by its negative clip w would mirror it onto the screen; the
// surface's motion is two screens long, as for a surface behind the previous
// camera. With no valid history the pass must return this frame's hits
// exactly. History taken from the mirrored position, a uniform 1 at the
// plane's depth, would pull every pixel toward 1, inside its neighbourhood's
// colour box.
#[test]
fn hits_behind_the_previous_camera_take_no_history() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let pass = Pass::new(&device, &queue);
    let projection = crate::perspective(1.2, 1., 0.1);
    let look = |target| glam::camera::rh::view::look_at_mat4(Vec3::ZERO, target, Vec3::Y);
    let inverse_view_projection = (projection * look(Vec3::NEG_Z)).inverse();
    let previous_view_projection = projection * look(Vec3::Z);
    let hit_depth = projection.project_point3(Vec3::new(0., 0., -5.)).z;
    let texels = (SIZE * SIZE) as usize;
    upload(
        &queue,
        &pass.reprojection,
        bytemuck::cast_slice(&vec![hit_depth; texels]),
        4,
    );
    // Motion (2, 0): two screens.
    upload(
        &queue,
        &pass.motion,
        bytemuck::cast_slice(&[0x4000u16, 0, 0, 0].repeat(texels)),
        8,
    );
    upload(
        &queue,
        &pass.history[0],
        bytemuck::cast_slice(&[ONE; 4].repeat(texels)),
        8,
    );
    upload(
        &queue,
        &pass.depth_history[0],
        bytemuck::cast_slice(&PLANE),
        4,
    );
    let size = SIZE as f32;
    let output = pass.run(
        (&device, &queue),
        &hits(0),
        TemporalParams {
            inverse_view_projection: inverse_view_projection.to_cols_array_2d(),
            previous_view_projection: previous_view_projection.to_cols_array_2d(),
            size: [size, size, 1. / size, 1. / size],
            near: 0.1,
            flags: TEMPORAL_CONTINUES,
            padding: [0; 2],
        },
        1,
    );
    let expected = hits(0);
    for (i, texel) in output.iter().enumerate() {
        assert_eq!(
            texel[0],
            value(expected[i * 4]),
            "pixel {i} took history from behind the previous camera"
        );
    }
}
