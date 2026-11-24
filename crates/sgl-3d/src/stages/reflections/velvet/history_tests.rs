//! Velvet's production temporal pass and the history it keeps: at half
//! resolution, through the downsample, at the silhouette of a surface the
//! camera follows over static background; and for pixels that leave the
//! traced set and return.
use super::*;
use glam::Vec3;
use wgpu::util::DeviceExt;

const FULL: u32 = 64;
const HALF: u32 = FULL / 2;
const NEAR: f32 = 0.1;
/// The craft's view depth, constant: the camera follows it.
const CRAFT: f32 = 8.;
/// The background's view depth now; the camera travelled `TRAVEL` towards
/// it, so it was `BACKGROUND + TRAVEL` from the previous camera.
const BACKGROUND: f32 = 40.;
const TRAVEL: f32 = 2.;
/// Full-resolution columns below this are the craft. Odd, so the block of
/// half-resolution column `SILHOUETTE / 2` holds a craft pixel and a
/// background pixel in each row.
const SILHOUETTE: u32 = 33;
/// The traced radiance and the uniform history.
const BRIGHT: f32 = 0.8;
const DARK: f32 = 0.05;
const HISTORY: f32 = 0.5;

fn texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: u32,
    format: wgpu::TextureFormat,
    data: &[u8],
) -> wgpu::Texture {
    device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        data,
    )
}

fn floats(values: impl IntoIterator<Item = f32>) -> Vec<u8> {
    values.into_iter().flat_map(f32::to_le_bytes).collect()
}

fn halves(values: impl IntoIterator<Item = f32>) -> Vec<u8> {
    values
        .into_iter()
        .flat_map(|v| crate::test_support::to_half(v).to_le_bytes())
        .collect()
}

/// The texels of an RGBA16F `texture` of `HALF` a side.
fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<[f32; 4]> {
    // HALF texels of 8 bytes fill one 256-byte copy row.
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(HALF * HALF * 8),
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
                bytes_per_row: Some(HALF * 8),
                rows_per_image: Some(HALF),
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

/// The traced radiance at half-resolution texel `(x, y)`: every 3x3
/// neighbourhood holds three bright and six dark texels, a colour box
/// (mean ± 2σ, about 0.3 ± 0.71) that admits the history.
fn traced(x: u32, y: u32) -> f32 {
    if (x + y).is_multiple_of(3) {
        BRIGHT
    } else {
        DARK
    }
}

// Plausible defect: at half resolution the receiver's depth and
// normal-roughness come from the nearest pixel of each 2x2 block (Godot's
// downsample) but its motion from another of its pixels, so where a block
// straddles two surfaces the depth test compares one surface's depth with
// the other's depth motion. A craft 8 m ahead fills the full-resolution
// columns below 33; the camera follows it, so it has neither screen nor
// depth motion. Static background 40 m away fills the rest; the camera
// travelled 2 m towards it, which the background's motion carries (its
// depth motion +2 m). The history is uniform and its depth history is each
// texel's surface as the previous camera saw it: the craft at 8 m, the
// background at 42 m, and the craft in the straddling column, its nearer
// surface. Every texel keeps its history: its output is
// 0.05 × traced + 0.95 × the history. The straddling column, read with the
// background's depth motion, expects 10 m where its history holds 8 m
// (exp(-0.2) < 0.9) and its neighbours 8 m or 42 m, and keeps none.
#[test]
fn a_straddling_block_takes_depth_and_motion_from_one_pixel() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let velvet = Velvet::new(&crate::view::compile::Creator::now(&device)).now();
    let projection = crate::perspective(1.2, 1., NEAR);
    // This camera stands TRAVEL further along -z than the previous one,
    // whose view is the world.
    let view = Mat4::from_translation(Vec3::new(0., 0., TRAVEL));
    let view_projection = projection * view;
    let device_depth = |z: f32| projection.project_point3(Vec3::new(0., 0., -z)).z;
    let full = |x: u32, y: u32| {
        let craft = x < SILHOUETTE;
        let depth = device_depth(if craft { CRAFT } else { BACKGROUND });
        let motion = if craft {
            [0., 0., 0., 1.]
        } else {
            // The static point seen here, on the previous screen.
            let uv = glam::Vec2::new(x as f32 + 0.5, y as f32 + 0.5) / FULL as f32;
            let ndc = Vec3::new(uv.x * 2. - 1., 1. - uv.y * 2., depth);
            let world = view_projection.inverse().project_point3(ndc);
            let old = projection.project_point3(world);
            let old_uv = glam::Vec2::new(old.x * 0.5 + 0.5, 0.5 - old.y * 0.5);
            let moved = uv - old_uv;
            [moved.x, moved.y, -world.z - BACKGROUND, 1.]
        };
        (depth, motion)
    };
    let grid = |size: u32| (0..size).flat_map(move |y| (0..size).map(move |x| (x, y)));
    let r32 = wgpu::TextureFormat::R32Float;
    let rgba32 = wgpu::TextureFormat::Rgba32Float;
    let rgba16 = wgpu::TextureFormat::Rgba16Float;
    let full_depth = texture(
        &device,
        &queue,
        FULL,
        r32,
        &floats(grid(FULL).map(|(x, y)| full(x, y).0)),
    );
    let full_motion = texture(
        &device,
        &queue,
        FULL,
        rgba32,
        &floats(grid(FULL).flat_map(|(x, y)| full(x, y).1)),
    );
    let full_normal_roughness = texture(
        &device,
        &queue,
        FULL,
        rgba32,
        &floats([0., 0., 1., 0.2].repeat((FULL * FULL) as usize)),
    );
    let empty = |format: wgpu::TextureFormat, texel: usize| {
        texture(
            &device,
            &queue,
            HALF,
            format,
            &vec![0; texel * (HALF * HALF) as usize],
        )
    };
    let depth = empty(r32, 4);
    let normal_roughness = empty(rgba16, 8);
    let motion = empty(rgba16, 8);
    let current = texture(
        &device,
        &queue,
        HALF,
        rgba16,
        &halves(grid(HALF).flat_map(|(x, y)| [traced(x, y); 3].into_iter().chain([1.]))),
    );
    let history = texture(
        &device,
        &queue,
        HALF,
        rgba16,
        &halves([HISTORY; 4].repeat((HALF * HALF) as usize)),
    );
    // The previous camera saw the craft 8 m and the background 42 m away;
    // the straddling block kept its nearer pixel, the craft.
    let depth_history = texture(
        &device,
        &queue,
        HALF,
        r32,
        &floats(grid(HALF).map(|(x, _)| {
            device_depth(if 2 * x < SILHOUETTE {
                CRAFT
            } else {
                BACKGROUND + TRAVEL
            })
        })),
    );
    let output = empty(rgba16, 8);
    let history_output = empty(rgba16, 8);
    let depth_output = empty(r32, 4);
    let size = HALF as f32;
    queue.write_buffer(
        &velvet.temporal_params,
        0,
        bytemuck::bytes_of(&TemporalParams {
            inverse_view_projection: view_projection.inverse().to_cols_array_2d(),
            previous_view_projection: projection.to_cols_array_2d(),
            size: [size, size, 1. / size, 1. / size],
            previous_near: NEAR,
            near: NEAR,
            flags: TEMPORAL_CONTINUES,
            padding: 0,
        }),
    );
    let view = |t: &wgpu::Texture| t.create_view(&Default::default());
    let group = |pipeline: &wgpu::ComputePipeline, entries: &[(u32, wgpu::BindingResource)]| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &entries
                .iter()
                .map(|(binding, resource)| wgpu::BindGroupEntry {
                    binding: *binding,
                    resource: resource.clone(),
                })
                .collect::<Vec<_>>(),
        })
    };
    let texture_view = wgpu::BindingResource::TextureView;
    let views = [
        &full_depth,
        &full_normal_roughness,
        &depth,
        &normal_roughness,
        &full_motion,
        &motion,
        &current,
        &history,
        &depth_history,
        &output,
        &history_output,
        &depth_output,
    ]
    .map(view);
    let [
        full_depth,
        full_normal_roughness,
        depth,
        normal_roughness,
        full_motion,
        motion,
        current,
        history,
        depth_history,
        output_view,
        history_output,
        depth_output,
    ] = &views;
    let downsample = group(
        &velvet.downsample,
        &[
            (0, texture_view(full_depth)),
            (1, texture_view(full_normal_roughness)),
            (2, texture_view(depth)),
            (3, texture_view(normal_roughness)),
            (4, texture_view(full_motion)),
            (5, texture_view(motion)),
        ],
    );
    // The receiver's reflected point is the receiver itself: the hit
    // reprojection is the camera's.
    let temporal = group(
        &velvet.temporal,
        &[
            (0, texture_view(current)),
            (1, texture_view(history)),
            (2, texture_view(depth)),
            (3, texture_view(motion)),
            (4, texture_view(depth)),
            (5, texture_view(depth_history)),
            (6, texture_view(normal_roughness)),
            (7, wgpu::BindingResource::Sampler(&velvet.linear)),
            (8, velvet.temporal_params.as_entire_binding()),
            (9, texture_view(output_view)),
            (10, texture_view(history_output)),
            (11, texture_view(depth_output)),
        ],
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    for (pipeline, group) in [
        (&velvet.downsample, &downsample),
        (&velvet.temporal, &temporal),
    ] {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, group, &[]);
        pass.dispatch_workgroups(HALF / 8, HALF / 8, 1);
    }
    queue.submit([encoder.finish()]);
    let result = read(&device, &queue, &output);
    let mut lost = Vec::new();
    for (x, y) in grid(HALF) {
        let expected = 0.05 * traced(x, y) + 0.95 * HISTORY;
        let got = result[(y * HALF + x) as usize][0];
        // Binary16 steps here are under 0.0005; keeping no history gives
        // the traced value, at least 0.42 away.
        if (got - expected).abs() > 0.01 {
            lost.push((x, y, got));
        }
    }
    assert!(
        lost.is_empty(),
        "{} texels lost their history, first (x, y, output): {:?}",
        lost.len(),
        lost.first()
    );
}

/// Three frames of the production temporal pass at full resolution over a
/// still plane, whose first frame resets to a uniform `before`; in the
/// second the right half (columns 16 and on) is rough enough to be untraced
/// (Godot's trace writes it 0), and in the third all of it is traced again.
/// Returns the third frame's output.
fn returning(device: &wgpu::Device, queue: &wgpu::Queue, before: f32) -> Vec<[f32; 4]> {
    let velvet = Velvet::new(&crate::view::compile::Creator::now(device)).now();
    let projection = crate::perspective(1.2, 1., NEAR);
    let plane = projection.project_point3(Vec3::new(0., 0., -5.)).z;
    let grid = || (0..HALF).flat_map(|y| (0..HALF).map(move |x| (x, y)));
    let r32 = wgpu::TextureFormat::R32Float;
    let rgba16 = wgpu::TextureFormat::Rgba16Float;
    let depth = texture(device, queue, HALF, r32, &floats(grid().map(|_| plane)));
    let motion = texture(
        device,
        queue,
        HALF,
        rgba16,
        &halves(grid().flat_map(|_| [0., 0., 0., 1.])),
    );
    let empty = |format: wgpu::TextureFormat, texel: usize| {
        texture(
            device,
            queue,
            HALF,
            format,
            &vec![0; texel * (HALF * HALF) as usize],
        )
    };
    let history = [empty(rgba16, 8), empty(rgba16, 8)];
    let depth_history = [empty(r32, 4), empty(r32, 4)];
    let output = empty(rgba16, 8);
    let size = HALF as f32;
    let view = |t: &wgpu::Texture| t.create_view(&Default::default());
    for frame in 0..3u32 {
        let untraced = |x: u32| frame == 1 && x >= HALF / 2;
        let current = texture(
            device,
            queue,
            HALF,
            rgba16,
            &halves(grid().flat_map(|(x, y)| {
                let value = match frame {
                    0 => before,
                    _ if untraced(x) => 0.,
                    _ => traced(x, y),
                };
                [value, value, value, 1.]
            })),
        );
        let normal_roughness = texture(
            device,
            queue,
            HALF,
            rgba16,
            &halves(grid().flat_map(|(x, _)| [0., 0., 1., if untraced(x) { 0.8 } else { 0.2 }])),
        );
        queue.write_buffer(
            &velvet.temporal_params,
            0,
            bytemuck::bytes_of(&TemporalParams {
                inverse_view_projection: projection.inverse().to_cols_array_2d(),
                previous_view_projection: projection.to_cols_array_2d(),
                size: [size, size, 1. / size, 1. / size],
                previous_near: NEAR,
                near: NEAR,
                flags: if frame == 0 { 0 } else { TEMPORAL_CONTINUES },
                padding: 0,
            }),
        );
        let write = (frame % 2) as usize;
        let views = [
            &current,
            &history[1 - write],
            &depth,
            &motion,
            &depth_history[1 - write],
            &normal_roughness,
            &output,
            &history[write],
            &depth_history[write],
        ]
        .map(view);
        let entries: Vec<_> = [0, 1, 2, 3, 5, 6, 9, 10, 11]
            .into_iter()
            .zip(&views)
            .map(|(binding, view)| wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::TextureView(view),
            })
            .chain([
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&views[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::Sampler(&velvet.linear),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: velvet.temporal_params.as_entire_binding(),
                },
            ])
            .collect();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &velvet.temporal.get_bind_group_layout(0),
            entries: &entries,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&velvet.temporal);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(HALF / 8, HALF / 8, 1);
        }
        queue.submit([encoder.finish()]);
    }
    read(device, queue, &output)
}

// Plausible defect: a pixel that Godot's trace skips (roughness at or above
// the cutoff) keeps the history it had when it was last traced, so when it
// returns it reprojects radiance from before it left, as DiligentFX's
// temporal pass kept it (PROVENANCE.md DFX-51). Two runs differ only in the
// history their first frame stores, bright or dark; then the right half is
// untraced for a frame and traced again. Where it returns, the runs' outputs
// must be equal (every read of history there lies within the right half,
// from column 17 on). The left half, traced throughout, must differ, or the
// comparison proves nothing.
#[test]
fn pixels_returning_to_the_traced_set_keep_no_old_radiance() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let bright = returning(&device, &queue, BRIGHT);
    let dark = returning(&device, &queue, DARK);
    let differs = |columns: std::ops::Range<u32>| {
        (0..HALF)
            .flat_map(|y| columns.clone().map(move |x| (y * HALF + x) as usize))
            .filter(|&i| bright[i] != dark[i])
            .count()
    };
    assert_eq!(
        differs(HALF / 2 + 1..HALF),
        0,
        "returning pixels kept radiance from before they left"
    );
    assert!(
        differs(0..HALF / 2) > 0,
        "history made no difference where it was kept"
    );
}
