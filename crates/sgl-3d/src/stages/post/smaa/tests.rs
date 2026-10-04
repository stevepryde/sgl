//! Real GPU rasterization compared with an independently supersampled reference.
//! This catches reversed neighborhood directions and broken search/area sampling
//! that shader validation alone cannot detect. No application startup gate.
use super::*;

pub(crate) fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let size = texture.size();
    let row = (size.width * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("SMAA GPU evidence"),
        size: u64::from(row * size.height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(size.height),
            },
        },
        size,
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |result| {
        tx.send(result).unwrap();
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().unwrap();
    buffer
        .get_mapped_range(..)
        .chunks(row as usize)
        .flat_map(|line| line[..size.width as usize * 4].iter().copied())
        .collect()
}

fn image_target(device: &wgpu::Device, size: [u32; 2]) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("SMAA diagonal fixture"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// SMAA's presets, as `Settings::smaa_quality` names them.
const QUALITIES: [SmaaQuality; 4] = [
    SmaaQuality::Low,
    SmaaQuality::Medium,
    SmaaQuality::High,
    SmaaQuality::Ultra,
];

/// The triangle's level on black for an edge of contrast 32/255, about
/// 0.125: between Medium's threshold (0.1) and Low's (0.15).
const FAINT: f32 = 32. / 255.;

/// A triangle's slanted edge on black, raw and through SMAA, and its
/// coverage by an independent reference.
struct Coverage {
    raw: Vec<u8>,
    smaa: Vec<u8>,
    reference: Vec<u8>,
    /// Absolute coverage error of the raw and SMAA images against the
    /// reference, away from the viewport boundary.
    raw_error: f64,
    smaa_error: f64,
}

/// A triangle on black filling the frame's left side up to a slanted edge,
/// which `coverage` measures.
#[derive(Clone, Copy)]
struct Edge {
    /// The triangle's level, 1 white.
    level: f32,
    /// The edge is exactly 45° in pixels, a quarter pixel right of the
    /// frame's corner, rather than about 46°.
    exact: bool,
    /// Mirrored top to bottom, so the edge falls to the right rather than
    /// rising.
    falling: bool,
}

impl Edge {
    const WHITE: Self = Self {
        level: 1.,
        exact: false,
        falling: false,
    };
}

/// `edge` at `size` through `smaa`. Hardware rasterization supplies input
/// and reference; the reference is rendered at 16x each dimension, then
/// averaged without using SMAA logic.
fn coverage(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    smaa: &mut Smaa,
    size: [u32; 2],
    edge: Edge,
) -> Coverage {
    let Edge {
        level,
        exact,
        falling,
    } = edge;
    let mirror: f32 = if falling { -1. } else { 1. };
    // The apex's clip x, and a horizontal shift.
    let (apex, shift): (f32, f32) = if exact {
        let pixel = 2. / size[0] as f32;
        (-1. + pixel * size[1] as f32, pixel / 4.)
    } else {
        (0.43, 0.)
    };
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("independent diagonal geometry"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                r#"
                @vertex fn fullscreen(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> {{
                    let p=array<vec2<f32>,3>(vec2<f32>(-1.0,-1.0),vec2<f32>(-1.0,1.0),vec2<f32>({apex:?},1.0));
                    return vec4<f32>((p[i]+vec2<f32>({shift:?},0.0))*vec2<f32>(1.0,{mirror:?}),0.0,1.0);
                }}
                @fragment fn white()->@location(0) vec4<f32> {{ return vec4<f32>({level:?}); }}
            "#
            )
            .into(),
        ),
    });
    let raster = pipeline(
        device,
        &shader,
        "white",
        "fullscreen",
        wgpu::TextureFormat::Rgba8Unorm,
        &[],
    );
    let render = |target: &wgpu::Texture| {
        let view = target.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("reference triangle"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&raster);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
    };
    smaa.resize(device, size[0], size[1]);
    let input = image_target(device, size);
    let output = image_target(device, size);
    let supersampled = image_target(device, [size[0] * 16, size[1] * 16]);
    render(&input);
    render(&supersampled);
    let mut encoder = device.create_command_encoder(&Default::default());
    smaa.encode(
        device,
        &mut encoder,
        &input.create_view(&Default::default()),
        &output.create_view(&Default::default()),
        None,
    );
    queue.submit([encoder.finish()]);
    let raw = read(device, queue, &input);
    let aa = read(device, queue, &output);
    let high = read(device, queue, &supersampled);
    let mut reference = vec![0u8; raw.len()];
    let (mut raw_error, mut smaa_error) = (0.0f64, 0.0f64);
    for y in 0..size[1] as usize {
        for x in 0..size[0] as usize {
            let mut total = 0u32;
            for sy in 0..16 {
                for sx in 0..16 {
                    total +=
                        u32::from(high[((y * 16 + sy) * size[0] as usize * 16 + x * 16 + sx) * 4]);
                }
            }
            let value = f64::from(total) / 256.0;
            let i = (y * size[0] as usize + x) * 4;
            reference[i..i + 3].fill(value.round() as u8);
            reference[i + 3] = 255;
            // Exclude the viewport boundary, which has no outside image.
            if x > 1 && y > 1 && x + 2 < size[0] as usize && y + 2 < size[1] as usize {
                raw_error += (f64::from(raw[i]) - value).abs();
                smaa_error += (f64::from(aa[i]) - value).abs();
            }
        }
    }
    Coverage {
        raw,
        smaa: aa,
        reference,
        raw_error,
        smaa_error,
    }
}

// Defects: a preset's pipeline constants that do not reach the shader, so
// every preset runs as SMAA was built; a broken preset path (diagonal or
// corner detection, a search length) that leaves edges aliased, blends the
// wrong way or blends where no edge is; and a threshold that is not the
// preset's. The oracle is each triangle's independently supersampled
// coverage. Each preset, switched to on the same SMAA as a game switches it,
// must bring the white triangle's slanted edge closer to it, and High and
// Ultra, whose diagonal detection exists for such edges, closer than Medium.
// An edge of contrast between Medium's and Low's thresholds is one Low must
// leave exactly as rasterized and Medium must smooth. SMAA.hlsl searches a
// rising diagonal (SMAASearchDiag1) and a falling one (SMAASearchDiag2) to
// the same length and reads both from the same diagonal areas, so High and
// Ultra must bring an exact 45° edge and its mirror image within a quarter
// of each other's error: a lost line end on one side (Bevy's `d.z`) breaks
// that.
#[test]
fn every_preset_approaches_supersampled_rasterization() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut smaa = Smaa::new(
        &device,
        &queue,
        1,
        1,
        wgpu::TextureFormat::Rgba8Unorm,
        SmaaQuality::default(),
    )
    .unwrap();
    let faint_edge = Edge {
        level: FAINT,
        ..Edge::WHITE
    };
    for size in [[128, 96], [192, 128]] {
        let mut white = Vec::new();
        let mut faint = Vec::new();
        for quality in QUALITIES {
            smaa.set_quality(&device, quality);
            for (edge, errors) in [(Edge::WHITE, &mut white), (faint_edge, &mut faint)] {
                let c = coverage(&device, &queue, &mut smaa, size, edge);
                eprintln!(
                    "{quality:?} {size:?} level {}: raw absolute coverage error={:.2}; SMAA={:.2}",
                    edge.level, c.raw_error, c.smaa_error
                );
                errors.push((quality, c.raw_error, c.smaa_error));
            }
            if matches!(quality, SmaaQuality::High | SmaaQuality::Ultra) {
                let [rising, falling] = [false, true].map(|falling| {
                    let edge = Edge {
                        exact: true,
                        falling,
                        ..Edge::WHITE
                    };
                    coverage(&device, &queue, &mut smaa, size, edge).smaa_error
                });
                eprintln!("{quality:?} {size:?} 45°: rising {rising:.2}; falling {falling:.2}");
                assert!(
                    rising.max(falling) < 1.25 * rising.min(falling),
                    "{quality:?} {size:?}: a 45° edge rising ({rising}) and falling ({falling}) must come out alike"
                );
            }
        }
        let error = |errors: &[(SmaaQuality, f64, f64)], quality| {
            let &(_, raw, smaa) = errors.iter().find(|(q, ..)| *q == quality).unwrap();
            (raw, smaa)
        };
        for &(quality, raw, smaa) in &white {
            assert!(
                smaa < raw * 0.9,
                "{quality:?} {size:?}: SMAA must improve diagonal coverage against independently rasterized reference"
            );
        }
        let (_, medium) = error(&white, SmaaQuality::Medium);
        for quality in [SmaaQuality::High, SmaaQuality::Ultra] {
            let (_, diagonal) = error(&white, quality);
            assert!(
                diagonal < medium,
                "{quality:?} {size:?}: {diagonal} must be closer to the reference than Medium's {medium}"
            );
        }
        let (raw, low) = error(&faint, SmaaQuality::Low);
        assert_eq!(
            low, raw,
            "{size:?}: Low must leave a 0.125 edge as rasterized"
        );
        let (raw, medium) = error(&faint, SmaaQuality::Medium);
        assert!(
            medium < raw * 0.9,
            "{size:?}: Medium must smooth a 0.125 edge: {medium} against raw {raw}"
        );
    }
}

#[test]
#[ignore = "requires a real GPU; writes review images under .cache/smaa-qa"]
fn diagonal_edges_approach_supersampled_rasterization() {
    pollster::block_on(async {
        let adapter = wgpu::Instance::default()
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .unwrap();
        eprintln!("SMAA QA device: {:?}", adapter.get_info());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .unwrap();
        let capture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".cache/smaa-qa");
        std::fs::create_dir_all(&capture).unwrap();
        for quality in QUALITIES {
            let mut smaa = Smaa::new(
                &device,
                &queue,
                1,
                1,
                wgpu::TextureFormat::Rgba8Unorm,
                quality,
            )
            .unwrap();
            for size in [[128, 96], [192, 128]] {
                let c = coverage(&device, &queue, &mut smaa, size, Edge::WHITE);
                eprintln!(
                    "{quality:?} {size:?}: raw absolute coverage error={:.2}; SMAA={:.2}",
                    c.raw_error, c.smaa_error
                );
                for (name, pixels) in [
                    ("raw", &c.raw),
                    ("smaa", &c.smaa),
                    ("reference", &c.reference),
                ] {
                    image::save_buffer(
                        capture.join(format!("{}-{quality:?}-{name}.png", size[0])),
                        pixels,
                        size[0],
                        size[1],
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                }
                assert!(
                    c.smaa_error < c.raw_error * 0.9,
                    "SMAA must improve diagonal coverage against independently rasterized reference"
                );
            }
        }
    });
}
