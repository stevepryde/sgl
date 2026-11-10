//! Exercises the production temporal pass: on a still view whose traced hits
//! move between neighbouring pixels every frame, as TAA's jitter moves hits on
//! thin bright geometry, and after a turn that puts the reflections' hits
//! behind the previous camera, and when the near plane changes.
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

/// Motion can be signed; the shared HDR fixture encoder accepts only
/// nonnegative values.
fn motion_half(value: f32) -> u16 {
    crate::test_support::to_half(value.abs()) | (u16::from(value.is_sign_negative()) << 15)
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
    let mapped = buffer.get_mapped_range(..).unwrap();
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
        previous_near: 0.1,
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
            previous_near: 0.1,
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

// The depth history holds raw reversed-Z device depth, near / distance, so it
// is linearised with the near plane that wrote it (#409). A still camera
// whose near plane moves from 0.5 to 0.1 m sees a still plane 5 m away, at
// depth 0.1 last frame and 0.02 now; read with this frame's near plane the
// history would put it at 1 m and disocclude every pixel. The history, a
// uniform 0.5 inside each neighbourhood's colour box, must carry over:
// accumulated, each pixel lies within a few hundredths of 0.5, never at this
// frame's hit or miss exactly.
#[test]
fn a_changed_near_plane_keeps_history() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let pass = Pass::new(&device, &queue);
    let view = glam::camera::rh::view::look_at_mat4(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y);
    let (near, previous_near) = (0.1, 0.5);
    let projection = crate::perspective(1.2, 1., near);
    let previous_projection = crate::perspective(1.2, 1., previous_near);
    let plane = Vec3::new(0., 0., -5.);
    let depth = projection.project_point3(plane).z;
    let previous_depth = previous_projection.project_point3(plane).z;
    let texels = (SIZE * SIZE) as usize;
    upload(
        &queue,
        &pass.depth,
        bytemuck::cast_slice(&vec![depth; texels]),
        4,
    );
    upload(
        &queue,
        &pass.reprojection,
        bytemuck::cast_slice(&vec![depth; texels]),
        4,
    );
    upload(
        &queue,
        &pass.depth_history[0],
        bytemuck::cast_slice(&vec![previous_depth; texels]),
        4,
    );
    upload(
        &queue,
        &pass.history[0],
        bytemuck::cast_slice(&[HALF, HALF, HALF, ONE].repeat(texels)),
        8,
    );
    let size = SIZE as f32;
    let output = pass.run(
        (&device, &queue),
        &hits(0),
        TemporalParams {
            inverse_view_projection: (projection * view).inverse().to_cols_array_2d(),
            previous_view_projection: (previous_projection * view).to_cols_array_2d(),
            size: [size, size, 1. / size, 1. / size],
            previous_near,
            flags: TEMPORAL_CONTINUES,
            padding: [0; 2],
        },
        1,
    );
    for (i, texel) in output.iter().enumerate() {
        assert!(
            (texel[0] - value(HALF)).abs() < 0.05,
            "pixel {i} dropped its history: {texel:?}"
        );
    }
}

// The bilinear fallback (#460) weights the four history texels around a UV,
// clamped to the grid, so within half a texel of the left edge all its weight
// lands on the edge column. Every pixel reprojects a quarter texel left, by
// motion and by hit alike, so the left column reads its history at a quarter
// texel. The receiver is at depth 0.3; the history's left column at 0.75 and
// the rest at 0.15 both disocclude it, the left column less, so the
// neighbour search keeps the reprojected UV and the fallback decides. Over
// the edge column alone it disoccludes and the left column keeps this
// frame's hits exactly; weighting the next column three quarters, as a
// truncated base texel does, blends a depth of exactly 0.3 and pulls the
// left column toward the history's uniform 0.5.
#[test]
fn the_left_edge_fallback_reads_the_edge_texel() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let pass = Pass::new(&device, &queue);
    let texels = (SIZE * SIZE) as usize;
    upload(
        &queue,
        &pass.depth,
        bytemuck::cast_slice(&vec![0.3f32; texels]),
        4,
    );
    let depth_history: Vec<f32> = (0..SIZE * SIZE)
        .map(|i| if i % SIZE == 0 { 0.75 } else { 0.15 })
        .collect();
    upload(
        &queue,
        &pass.depth_history[0],
        bytemuck::cast_slice(&depth_history),
        4,
    );
    upload(
        &queue,
        &pass.history[0],
        bytemuck::cast_slice(&[HALF, HALF, HALF, ONE].repeat(texels)),
        8,
    );
    // Motion (1/128, 0), a quarter texel: binary16 0x2000.
    upload(
        &queue,
        &pass.motion,
        bytemuck::cast_slice(&[0x2000u16, 0, 0, 0].repeat(texels)),
        8,
    );
    let size = SIZE as f32;
    // The same quarter texel by hit: a quarter of a clip-space texel (2/size).
    let previous_view_projection = Mat4::from_translation(Vec3::new(-0.5 / size, 0., 0.));
    let output = pass.run(
        (&device, &queue),
        &hits(0),
        TemporalParams {
            inverse_view_projection: Mat4::IDENTITY.to_cols_array_2d(),
            previous_view_projection: previous_view_projection.to_cols_array_2d(),
            size: [size, size, 1. / size, 1. / size],
            previous_near: 0.1,
            flags: TEMPORAL_CONTINUES,
            padding: [0; 2],
        },
        1,
    );
    let expected = hits(0);
    for y in 0..SIZE as usize {
        let i = y * SIZE as usize;
        assert_eq!(
            output[i][0],
            value(expected[i * 4]),
            "left-edge pixel {y} took history from the next column"
        );
    }
}

// Plausible defect: a clamp-to-edge sample outside the old viewport wins
// on colour, then the pass rejects it without trying the valid candidate.
// The oracle is a valid 0.5 history at the receiver's previous position:
// accumulation must pull a current hit/miss toward it. The viewport edge
// holds the current neighbourhood's mean (~0.3), so the invalid candidate
// wins the old unqualified luminance comparison in both directions.
#[test]
fn an_offscreen_candidate_cannot_discard_valid_history() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let pass = Pass::new(&device, &queue);
    let texels = (SIZE * SIZE) as usize;
    let projection = crate::perspective(std::f32::consts::FRAC_PI_2, 1., 0.1);
    let size = SIZE as f32;
    let target = (16 * SIZE + 24) as usize;
    let mean = crate::test_support::to_half((value(BRIGHT) + 2. * value(DARK)) / 3.);
    let history: Vec<u16> = (0..SIZE * SIZE)
        .flat_map(|p| {
            let x = p % SIZE;
            let color = if x == 0 || x == SIZE - 1 { mean } else { HALF };
            [color, color, color, ONE]
        })
        .collect();
    upload(&queue, &pass.history[0], bytemuck::cast_slice(&history), 8);
    for depth in [&pass.depth, &pass.depth_history[0]] {
        upload(
            &queue,
            depth,
            bytemuck::cast_slice(&vec![0.05f32; texels]),
            4,
        );
    }
    for offscreen_hit in [false, true] {
        let (previous, motion, hit_depth) = if offscreen_hit {
            // Receiver at z=-2 remains visible after a 1m lateral move and
            // zoom: its old UV is (17.5/32, 16.5/32). The virtual point at
            // z=-10 lies beyond the right edge (old UV x=1.146875).
            let zoom = crate::perspective(2. * (1f32 / 3.).atan(), 1., 0.1);
            let view = Mat4::from_translation(Vec3::new(-1., 0., 0.));
            (zoom * view, 0.21875, 0.01)
        } else {
            // Virtual hit stays at this pixel; motion points two screens
            // left, where the history sampler clamps to the left edge.
            (projection, 2., 0.05)
        };
        upload(
            &queue,
            &pass.motion,
            bytemuck::cast_slice(&[crate::test_support::to_half(motion), 0, 0, 0].repeat(texels)),
            8,
        );
        upload(
            &queue,
            &pass.reprojection,
            bytemuck::cast_slice(&vec![hit_depth; texels]),
            4,
        );
        let output = pass.run(
            (&device, &queue),
            &hits(0),
            TemporalParams {
                inverse_view_projection: projection.inverse().to_cols_array_2d(),
                previous_view_projection: previous.to_cols_array_2d(),
                size: [size, size, 1. / size, 1. / size],
                previous_near: 0.1,
                flags: TEMPORAL_CONTINUES,
                padding: [0; 2],
            },
            1,
        );
        assert!(
            (output[target][0] - value(HALF)).abs() < 0.05,
            "offscreen_hit={offscreen_hit}: valid history was discarded: {:?}",
            output[target]
        );
    }
    // Both candidates now lie a quarter texel beyond the right edge. At
    // row16 the edge's depth disagrees, while row15 agrees; an unguarded
    // vicinity search steps (-1,-1) back inside and incorrectly takes that
    // edge history. Neither original candidate was visible, so the only
    // valid result is the current sample, independent of that neighbour.
    let mut old_depth = vec![0.05f32; texels];
    old_depth[(16 * SIZE + SIZE - 1) as usize] = 0.025;
    upload(
        &queue,
        &pass.depth_history[0],
        bytemuck::cast_slice(&old_depth),
        4,
    );
    upload(
        &queue,
        &pass.reprojection,
        bytemuck::cast_slice(&vec![0.05f32; texels]),
        4,
    );
    upload(
        &queue,
        &pass.motion,
        bytemuck::cast_slice(&[motion_half(-0.2421875), 0, 0, 0].repeat(texels)),
        8,
    );
    let previous = Mat4::from_translation(Vec3::new(0.484375, 0., 0.)) * projection;
    let output = pass.run(
        (&device, &queue),
        &hits(0),
        TemporalParams {
            inverse_view_projection: projection.inverse().to_cols_array_2d(),
            previous_view_projection: previous.to_cols_array_2d(),
            size: [size, size, 1. / size, 1. / size],
            previous_near: 0.1,
            flags: TEMPORAL_CONTINUES,
            padding: [0; 2],
        },
        1,
    );
    assert_eq!(
        output[target][0],
        value(hits(0)[target * 4]),
        "two offscreen candidates admitted edge history"
    );
}

// Plausible defect: disocclusion compares distances measured from different
// camera positions. The independent scene is a stationary plane at world
// z=-10, previously 10m away and now 8m after a 2m dolly. Its old depth is
// 10m everywhere, so searching neighbours cannot repair comparison with
// 8m (exp(-2/8) < 0.9). Correct reprojection preserves its uniform 0.5
// history. Different near planes and jitter ensure the depth conversion
// follows each camera's actual projection rather than an assumed matrix.
#[test]
fn a_camera_dolly_keeps_a_stationary_planes_history() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let pass = Pass::new(&device, &queue);
    let texels = (SIZE * SIZE) as usize;
    let size = SIZE as f32;
    let projection = crate::perspective(1.2, 1., 0.1);
    let old_projection = crate::perspective(1.2, 1., 0.3);
    let view = Mat4::from_translation(Vec3::new(0., 0., 2.));
    let current =
        Mat4::from_translation(Vec3::new(0.25 / size, -0.5 / size, 0.)) * projection * view;
    let previous = Mat4::from_translation(Vec3::new(-0.5 / size, 0.25 / size, 0.)) * old_projection;
    let depth = current.project_point3(Vec3::new(0., 0., -10.)).z;
    let old_depth = previous.project_point3(Vec3::new(0., 0., -10.)).z;
    for texture in [&pass.depth, &pass.reprojection] {
        upload(
            &queue,
            texture,
            bytemuck::cast_slice(&vec![depth; texels]),
            4,
        );
    }
    upload(
        &queue,
        &pass.depth_history[0],
        bytemuck::cast_slice(&vec![old_depth; texels]),
        4,
    );
    upload(
        &queue,
        &pass.history[0],
        bytemuck::cast_slice(&[HALF, HALF, HALF, ONE].repeat(texels)),
        8,
    );
    // The public motion contract is unjittered current minus previous UV.
    let motion: Vec<u16> = (0..SIZE * SIZE)
        .flat_map(|i| {
            let uv = glam::Vec2::new((i % SIZE) as f32 + 0.5, (i / SIZE) as f32 + 0.5) / size;
            let point =
                current
                    .inverse()
                    .project_point3(Vec3::new(uv.x * 2. - 1., 1. - uv.y * 2., depth));
            let now = (projection * view).project_point3(point);
            let old = old_projection.project_point3(point);
            let delta = (now - old) * Vec3::new(0.5, -0.5, 0.);
            [motion_half(delta.x), motion_half(delta.y), 0, 0]
        })
        .collect();
    upload(&queue, &pass.motion, bytemuck::cast_slice(&motion), 8);
    let output = pass.run(
        (&device, &queue),
        &hits(0),
        TemporalParams {
            inverse_view_projection: current.inverse().to_cols_array_2d(),
            previous_view_projection: previous.to_cols_array_2d(),
            size: [size, size, 1. / size, 1. / size],
            previous_near: 0.3,
            flags: TEMPORAL_CONTINUES,
            padding: [0; 2],
        },
        1,
    );
    for y in 2..SIZE - 2 {
        for x in 2..SIZE - 2 {
            let texel = output[(y * SIZE + x) as usize];
            assert!(
                (texel[0] - value(HALF)).abs() < 0.05,
                "stationary plane lost history at ({x},{y}): {texel:?}"
            );
        }
    }
}

// Plausible defect: Velvet adds a world-space hit length directly to view
// Z, placing off-axis virtual points too far away. Exercise the production
// trace's reflected_depth; the independent oracle reconstructs its point
// through the camera and measures the added distance in 3D. A 10m hit must
// extend the view ray by 10m at the centre and edges alike, and a miss must
// remain on its receiver. World resolve exercises the same contract in its
// own production pass (world/tests.rs).
#[test]
fn virtual_points_extend_the_normalised_view_ray() {
    use wgpu::util::DeviceExt;
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let reflection = Velvet::new(&device);
    let points = [
        [0f32, 0., -10., 10.],
        [10., 0., -10., 10.],
        [-12., 7., -8., 25.],
    ];
    let inputs = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("virtual point receivers"),
        contents: bytemuck::cast_slice(&points),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("virtual point depths"),
        size: (points.len() * 8) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    static OBSERVATION: Module = Module {
        name: "velvet_virtual_points_test",
        source: "@group(0) @binding(9) var<storage,read> points:array<vec4<f32>>;\n@group(0) @binding(10) var<storage,read_write> depths:array<vec2<f32>>;\n@compute @workgroup_size(1) fn observe(@builtin(global_invocation_id) id:vec3<u32>) {\n let p=points[id.x];\n depths[id.x]=vec2(reflected_depth(p.xyz,p.w),reflected_depth(p.xyz,0.));\n}\n",
        deps: &[&TRACE],
    };
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Velvet virtual points"),
        source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&OBSERVATION]).into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Velvet virtual points"),
        layout: None,
        module: &shader,
        entry_point: Some("observe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 5,
                resource: reflection.scene_data.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 9,
                resource: inputs.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 10,
                resource: output.as_entire_binding(),
            },
        ],
    });
    for near in [0.1, 0.5] {
        let projection = crate::perspective(1.2, 1., near);
        queue.write_buffer(
            &reflection.scene_data,
            0,
            bytemuck::bytes_of(&SceneData {
                projection: projection.to_cols_array_2d(),
                inv_projection: projection.inverse().to_cols_array_2d(),
                reprojection: Mat4::IDENTITY.to_cols_array_2d(),
                eye_offset: [0.; 4],
            }),
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(points.len() as u32, 1, 1);
        }
        queue.submit([encoder.finish()]);
        let depths = crate::test_support::read_words(&device, &queue, &output);
        for (point, depth) in points.iter().zip(depths.chunks_exact(2)) {
            let receiver = Vec3::new(point[0], point[1], point[2]);
            let screen = projection.project_point3(receiver);
            for (word, hit_distance) in depth.iter().zip([point[3], 0.]) {
                let virtual_point = projection.inverse().project_point3(Vec3::new(
                    screen.x,
                    screen.y,
                    f32::from_bits(*word),
                ));
                let measured = virtual_point.distance(receiver);
                assert!(
                    (measured - hit_distance).abs() < 1e-4,
                    "near={near}, receiver={receiver:?}: virtual ray extends {measured}m, expected {hit_distance}m"
                );
            }
        }
    }
}
