//! Real GPU rasterization compared with an independently supersampled reference.
//! This catches reversed neighborhood directions and broken search/area sampling
//! that shader validation alone cannot detect. No application startup gate.
use super::*;
use crate::test_support::{half, hdr_texture, read};

/// The fixture's format, the RGBA16F SMAA reads and writes in post.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

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
        format: FORMAT,
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

// Defect: replacing the colour detector's centre-to-second-neighbour contrast
// with the luma detector's adjacent-pixel contrast reverses these decisions.
// The oracle is hand arithmetic from SMAAColorEdgeDetectionPS (iryoku/smaa
// 27fad0b, SMAA.hlsl lines 791-805), not a CPU copy of the shader:
//
// second neighbour / neighbour / centre, in sRGB:
// 0.125 / 0.5  / 0.75: edge 0.25, largest contrast 0.625 > 2*0.25 => suppress.
// 0.125 / 0.75 / 0.5:  edge 0.25, largest contrast 0.375 < 2*0.25 => retain.
// Adjacent-pixel second contrasts would be 0.375 and 0.625 respectively,
// giving the opposite answers. Both edges exceed every preset's threshold;
// the inequalities have enough margin for RGBA16F quantization.
// Shader compilation cannot establish these sampled-pixel decisions.
#[test]
fn color_local_contrast_matches_reference_pixel_pairs() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = [8, 8];
    let mut smaa = Smaa::new(&device, &queue, size[0], size[1], FORMAT, QUALITIES[0]).unwrap();
    let output = image_target(&device, size).create_view(&Default::default());
    for quality in QUALITIES {
        smaa.set_quality(&device, quality);
        for axis in 0..2 {
            // A contrast in any RGB channel is an edge in colour detection;
            // luma detection would lose especially the blue-only contrast.
            for channel in 0..3 {
                for (neighbour, centre, retained) in [(0.5, 0.75, false), (0.75, 0.5, true)] {
                    let mut texels = Vec::new();
                    for y in 0..size[1] {
                        for x in 0..size[0] {
                            let level = match [x, y][axis] {
                                2 => 0.125,
                                3 => neighbour,
                                _ => centre,
                            };
                            let mut pixel = [0., 0., 0., 1.];
                            pixel[channel] = crate::shading::srgb::to_linear(level);
                            texels.push(pixel);
                        }
                    }
                    let input = hdr_texture(&device, &queue, size, &texels);
                    let mut encoder = device.create_command_encoder(&Default::default());
                    smaa.encode(&device, &mut encoder, &input, &output, None);
                    queue.submit([encoder.finish()]);
                    let edges = read(&device, &queue, smaa.edges.texture(), 8);
                    // Probe (4,4): immediate neighbours in the other axis,
                    // and right/bottom, have the centre's colour.
                    let offset = ((4 * size[0] + 4) * 8) as usize;
                    let observed = [half(&edges[offset..]), half(&edges[offset + 2..])];
                    let mut expected = [0., 0.];
                    expected[axis] = if retained { 1. } else { 0. };
                    assert_eq!(
                        observed, expected,
                        "{quality:?}, axis {axis}, RGB channel {channel}, neighbour {neighbour}, centre {centre}: colour-reference local contrast"
                    );
                }
            }
        }
    }
}

/// The triangle's level on black for an edge of contrast 32/255, about
/// 0.125, in the sRGB encoding SMAA detects edges on: between Medium's
/// threshold (0.1) and Low's (0.15).
fn faint() -> f32 {
    crate::shading::srgb::to_linear(32. / 255.)
}

/// Absolute coverage error of a triangle's slanted edge on black, raw and
/// through SMAA, against an independent reference, away from the viewport
/// boundary.
struct Coverage {
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
    let raster = pipeline(device, &shader, "white", "fullscreen", FORMAT, &[]);
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
    // Each texel's red channel, in 8-bit code values.
    let red = |texture: &wgpu::Texture| -> Vec<f64> {
        read(device, queue, texture, 8)
            .chunks_exact(8)
            .map(|texel| f64::from(half(texel)) * 255.)
            .collect()
    };
    let raw = red(&input);
    let aa = red(&output);
    let high = red(&supersampled);
    let (mut raw_error, mut smaa_error) = (0.0f64, 0.0f64);
    for y in 0..size[1] as usize {
        for x in 0..size[0] as usize {
            let mut total = 0.;
            for sy in 0..16 {
                for sx in 0..16 {
                    total += high[(y * 16 + sy) * size[0] as usize * 16 + x * 16 + sx];
                }
            }
            let value = total / 256.0;
            let i = y * size[0] as usize + x;
            // Exclude the viewport boundary, which has no outside image.
            if x > 1 && y > 1 && x + 2 < size[0] as usize && y + 2 < size[1] as usize {
                raw_error += (raw[i] - value).abs();
                smaa_error += (aa[i] - value).abs();
            }
        }
    }
    Coverage {
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
    let mut smaa = Smaa::new(&device, &queue, 1, 1, FORMAT, SmaaQuality::default()).unwrap();
    let faint_edge = Edge {
        level: faint(),
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
