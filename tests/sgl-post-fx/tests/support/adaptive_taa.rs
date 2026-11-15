//! Independent signals: a projected periodic surface, a moving camera which
//! stops, and an occluder revealing a different-depth surface. The current
//! sample positions and camera matrices carry the same real Halton jitter.
use super::*;

const ADAPTIVE: temporal_anti_aliasing::FeatureFlags =
    temporal_anti_aliasing::FeatureFlags::MOTION_ADAPTIVE_HISTORY;

fn jittered_camera(reversed: bool, index: u32, jitter: [f32; 2], pan: f32) -> CameraAttribs {
    let mut c = camera(reversed, index);
    let j = [
        2. * jitter[0] / SIZE[0] as f32,
        -2. * jitter[1] / SIZE[1] as f32,
    ];
    c.f2_jitter = j;
    c.m_proj[8] = j[0];
    c.m_proj[9] = j[1];
    c.m_proj_inv[12] = -j[0] / c.m_proj[0];
    c.m_proj_inv[13] = -j[1] / c.m_proj[5];
    c.m_view_proj = c.m_proj;
    c.m_view_proj_inv = c.m_proj_inv;
    // Pan measured in pixels on the plane at view Z=2, independent of
    // renderer implementation. The projection above maps that plane's
    // world x into screen pixels by width * sx / (2 * z).
    let camera_x = pan * 4. / (SIZE[0] as f32 * c.m_proj[0]);
    c.m_view[12] = -camera_x;
    c.m_view_inv[12] = camera_x;
    c.f4_position[0] = camera_x;
    c.m_view_proj[12] = -c.m_proj[0] * camera_x;
    for column in 0..4 {
        c.m_view_proj_inv[column * 4] += camera_x * c.m_proj_inv[column * 4 + 3];
    }
    c
}

fn halton(base: u32, mut i: u32) -> f32 {
    let mut f = 1.;
    let mut value = 0.;
    while i > 0 {
        f /= base as f32;
        value += f * (i % base) as f32;
        i /= base;
    }
    value - 0.5
}

#[derive(Clone, Copy)]
enum Signal {
    Periodic,
    TranslatedPeriodic,
    MotionResidue,
    Border,
    CameraStop,
    Reveal,
    Toggle,
}

// The stripes have a one-pixel period and 35% coverage on the plane at Z=2.
// A whole 3x3 current neighborhood can therefore miss their bright faces
// together. Point sampling exactly reproduces the reported aliasing boundary;
// constant depth and camera-consistent zero motion rule out lighting/network
// changes. The independent spatial coverage mean is .35*.22 + .65*.05.
fn sequence(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    reversed: bool,
    adaptive: bool,
    signal: Signal,
) -> Vec<(f32, f32)> {
    let mut context = PostFXContext::new(device, queue, Default::default());
    let mut taa = TemporalAntiAliasing::new(device);
    let mut previous_camera = jittered_camera(reversed, 0, [0.; 2], 0.);
    let mut previous_depth = None;
    let mut previous_pan = 0.;
    let mut samples = Vec::new();
    for index in 0u32..96 {
        let jitter = [halton(2, index % 16 + 1), halton(3, index % 16 + 1)];
        let pan = if matches!(signal, Signal::CameraStop) && (24..32).contains(&index) {
            (index - 23) as f32 * 2.
        } else if matches!(signal, Signal::CameraStop) && index >= 32 {
            16.
        } else {
            0.
        };
        let mut curr = jittered_camera(reversed, index, jitter, pan);
        if matches!(signal, Signal::TranslatedPeriodic) {
            // A translated stationary camera must not become camera motion
            // when jitter is cancelled after a rounded VP multiplication.
            let world = [42.4976, -0.3901, 16.926];
            for row in 0..4 {
                curr.m_view_proj[12 + row] = curr.m_proj[12 + row]
                    - curr.m_proj[row] * world[0]
                    - curr.m_proj[4 + row] * world[1]
                    - curr.m_proj[8 + row] * world[2];
            }
            for (axis, coordinate) in world.into_iter().enumerate() {
                curr.m_view[12 + axis] = -coordinate;
                curr.m_view_inv[12 + axis] = coordinate;
                curr.f4_position[axis] = coordinate;
                for column in 0..4 {
                    curr.m_view_proj_inv[column * 4 + axis] = curr.m_proj_inv[column * 4 + axis]
                        + coordinate * curr.m_proj_inv[column * 4 + 3];
                }
            }
        }
        let occluded = matches!(signal, Signal::Reveal) && index < 40;
        let color_value = if occluded {
            0.8
        } else if (0.5 - jitter[0] + pan).rem_euclid(1.) < 0.35 {
            0.22
        } else {
            0.05
        };
        let color = texture(
            device,
            queue,
            wgpu::TextureFormat::Rgba16Float,
            &half(&[color_value, color_value, color_value, 1.]),
        );
        // Poison unused input B/A. The placeholder must initialize its own
        // recovery history rather than copying arbitrary caller channels.
        let motion = texture(
            device,
            queue,
            wgpu::TextureFormat::Rgba32Float,
            bytemuck::cast_slice(&[
                -2. * (pan - previous_pan
                    + if matches!(signal, Signal::MotionResidue) {
                        0.000043
                    } else {
                        0.
                    })
                    / SIZE[0] as f32,
                0.,
                100.,
                0.75,
            ]),
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        let z = if occluded { 0.4 } else { 2. };
        let clip_depth = curr.m_proj[10] + curr.m_proj[14] / z;
        let current_depth = depth(device, &mut encoder, clip_depth);
        let flags = if adaptive && !(matches!(signal, Signal::Toggle) && (32..48).contains(&index))
        {
            ADAPTIVE
        } else {
            temporal_anti_aliasing::FeatureFlags::NONE
        };
        context.prepare_resources(
            device,
            &FrameDesc {
                index,
                width: SIZE[0],
                height: SIZE[1],
                output_width: SIZE[0],
                output_height: SIZE[1],
            },
            if reversed {
                post_fx_context::FeatureFlags::REVERSED_DEPTH
            } else {
                post_fx_context::FeatureFlags::NONE
            },
        );
        taa.prepare_resources(device, &mut encoder, &context, flags, 0);
        let prev_depth = previous_depth.as_ref().unwrap_or(&current_depth);
        context.execute(&mut RenderAttributes {
            device,
            queue,
            device_context: &mut encoder,
            curr_depth_buffer_srv: &current_depth,
            prev_depth_buffer_srv: prev_depth,
            curr_camera: Some(&curr),
            prev_camera: Some(&previous_camera),
            camera_attribs_cb: None,
            pass_timestamps: None,
        });
        taa.execute(&mut temporal_anti_aliasing::RenderAttributes {
            device,
            queue,
            device_context: &mut encoder,
            post_fx_context: &mut context,
            color_buffer_srv: &color,
            depth_buffer_srv: &current_depth,
            motion_vectors_srv: &motion,
            taa_attribs: &TemporalAntiAliasingAttribs {
                temporal_stability_factor: 0.88,
                ..Default::default()
            },
            accumulation_buffer_idx: 0,
            pass_timestamps: None,
        });
        queue.submit([encoder.finish()]);
        let frame = read_rgba16f(device, queue, taa.get_accumulated_frame_srv(false, 0));
        let pixel = if matches!(signal, Signal::Border) {
            0
        } else {
            (SIZE[0] * SIZE[1] / 2 + SIZE[0] / 2) as usize
        };
        let output = frame[pixel][0];
        assert!(
            output.is_finite() && (0.0..=1.).contains(&output),
            "frame {index}: {output}"
        );
        samples.push((output, color_value));
        previous_camera = curr;
        previous_depth = Some(current_depth);
        previous_pan = pan;
    }
    samples
}

fn variance(samples: &[(f32, f32)]) -> f32 {
    let mean = samples.iter().map(|s| s.0).sum::<f32>() / samples.len() as f32;
    samples.iter().map(|s| (s.0 - mean).powi(2)).sum::<f32>() / samples.len() as f32
}

#[test]
fn taa_adaptive_retains_periodic_subpixel_geometry_without_disabling_rejection() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        let legacy = sequence(&device, &queue, reversed, false, Signal::Periodic);
        let adaptive = sequence(&device, &queue, reversed, true, Signal::Periodic);
        let a = &adaptive[64..96];
        let mean = a.iter().map(|s| s.0).sum::<f32>() / a.len() as f32;
        assert!(
            variance(a) < variance(&legacy[64..96]) * 0.25,
            "reversed={reversed}: legacy variance {}, adaptive {}",
            variance(&legacy[64..96]),
            variance(a)
        );
        assert!((mean - 0.1095).abs() < 0.035, "coverage lost: {mean}");
    }
}

#[test]
fn taa_adaptive_camera_stop_holds_recovery_then_stabilizes() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        let frames = sequence(&device, &queue, reversed, true, Signal::CameraStop);
        // Recovery must continue after motion ends, even though every current
        // velocity is now zero. The reference hold ramps rather than plateaus.
        let settled = sequence(&device, &queue, reversed, true, Signal::Periodic);
        let error = |f: &[(f32, f32)]| f.iter().map(|(a, b)| (a - b).abs()).sum::<f32>();
        assert!(
            error(&frames[32..40]) < error(&settled[32..40]) * 0.7,
            "camera stop did not restore rejection: recovering {}, settled {}",
            error(&frames[32..40]),
            error(&settled[32..40])
        );
        assert!(variance(&frames[64..96]) < variance(&frames[32..40]) * 0.25);
    }
}

#[test]
fn taa_adaptive_depth_disocclusion_rearms_zero_motion_background() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        let frames = sequence(&device, &queue, reversed, true, Signal::Reveal);
        let (output, current) = frames[40];
        assert!(
            (output - current).abs() < 0.002,
            "occluder ghost on first revealed background: {output}/{current}"
        );
        let settled = sequence(&device, &queue, reversed, true, Signal::Periodic);
        let error = |f: &[(f32, f32)]| f.iter().map(|(a, b)| (a - b).abs()).sum::<f32>();
        assert!(
            error(&frames[40..48]) < error(&settled[40..48]) * 0.7,
            "departure did not hold rejection: recovering {}, settled {}",
            error(&frames[40..48]),
            error(&settled[40..48])
        );
        assert!(variance(&frames[64..96]) < variance(&frames[40..48]) * 0.25);
    }
}

#[test]
fn taa_adaptive_format_switch_resets_at_same_extent_in_both_directions() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        let frames = sequence(&device, &queue, reversed, true, Signal::Toggle);
        for index in [32, 48] {
            let (output, current) = frames[index];
            assert!(
                (output - current).abs() < 0.002,
                "feature change retained incompatible history: {output}/{current}"
            );
        }
        assert!(variance(&frames[64..96]) < variance(&frames[32..48]) * 0.25);
    }
}

#[test]
fn taa_adaptive_translated_camera_does_not_mistake_jitter_for_motion() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        let origin = sequence(&device, &queue, reversed, true, Signal::Periodic);
        let translated = sequence(&device, &queue, reversed, true, Signal::TranslatedPeriodic);
        assert!(
            variance(&translated[64..96]) < variance(&origin[64..96]) * 1.2,
            "translated-camera variance {} vs origin {}",
            variance(&translated[64..96]),
            variance(&origin[64..96])
        );
    }
}

#[test]
fn taa_adaptive_static_projection_residue_is_not_surface_motion() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        let origin = sequence(&device, &queue, reversed, true, Signal::Periodic);
        let residue = sequence(&device, &queue, reversed, true, Signal::MotionResidue);
        assert!(
            variance(&residue[64..96]) < variance(&origin[64..96]) * 1.2,
            "captured 0.000043-pixel projection residue prevented stabilization"
        );
    }
}

#[test]
fn taa_adaptive_border_depth_alignment_keeps_valid_stable_history() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        let interior = sequence(&device, &queue, reversed, true, Signal::Periodic);
        let border = sequence(&device, &queue, reversed, true, Signal::Border);
        assert!(
            variance(&border[64..96]) < variance(&interior[64..96]) * 1.2,
            "jitter-aligned depth tap off the raw grid flashed a valid border history pixel"
        );
    }
}
// A resting thin foreground strip can vacate a pixel with zero current AND
// previous local velocity. Its old neighborhood still contains background,
// so the two-sided depth test alone is not evidence that history is safe.
#[test]
fn taa_adaptive_resting_thin_occluder_departure_does_not_leave_a_ghost() {
    use wgpu::util::DeviceExt;
    let Some((device, queue)) = device() else {
        return;
    };
    for report_motion in [false, true] {
        for reversed in [false, true] {
            let mut context = PostFXContext::new(&device, &queue, Default::default());
            let mut taa = TemporalAntiAliasing::new(&device);
            let c = camera(reversed, 0);
            let front = c.m_proj[10] + c.m_proj[14] / 0.4;
            let back = c.m_proj[10] + c.m_proj[14] / 2.;
            let shader=device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label:Some("thin occluder geometry"),
            source:wgpu::ShaderSource::Wgsl("override depth:f32=0.5; @vertex fn vs(@location(0) xy:vec2<f32>)->@builtin(position) vec4<f32>{return vec4(xy,depth,1.);} @fragment fn fs(){}".into()),
        });
            let constants = [("depth", f64::from(front))];
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("thin occluder depth"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &constants,
                        ..Default::default()
                    },
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: 8,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        }],
                    })],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[],
                }),
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
            let mut prev_camera = jittered_camera(reversed, 0, [0.; 2], 0.);
            let mut prev_depth = None;
            for index in 0u32..40 {
                let jitter = [halton(2, index % 16 + 1), halton(3, index % 16 + 1)];
                let left = if index < 32 { 24.25 } else { 32.25 };
                let moved = index == 32;
                let lo = left + jitter[0];
                let hi = lo + 2.;
                let mut color = Vec::new();
                let mut motion = Vec::new();
                for _y in 0..SIZE[1] {
                    for x in 0..SIZE[0] {
                        let covered = (x as f32 + 0.5) >= lo && (x as f32 + 0.5) < hi;
                        let value = if covered { 0.8 } else { 0.05 };
                        color.extend_from_slice(&[value, value, value, 1.]);
                        motion.extend_from_slice(&[
                            if covered && moved {
                                16. / SIZE[0] as f32
                            } else {
                                0.
                            },
                            0.,
                            100.,
                            0.75,
                        ]);
                    }
                }
                let color = texels(
                    &device,
                    &queue,
                    wgpu::TextureFormat::Rgba16Float,
                    &half(&color),
                );
                let motion = texels(
                    &device,
                    &queue,
                    wgpu::TextureFormat::Rgba32Float,
                    bytemuck::cast_slice(&motion),
                );
                let mut encoder = device.create_command_encoder(&Default::default());
                let current_depth = depth(&device, &mut encoder, back);
                let x0 = 2. * lo / SIZE[0] as f32 - 1.;
                let x1 = 2. * hi / SIZE[0] as f32 - 1.;
                let vertices = [
                    [x0, -1.],
                    [x1, -1.],
                    [x0, 1.],
                    [x0, 1.],
                    [x1, -1.],
                    [x1, 1.],
                ];
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("resting thin strip"),
                        color_attachments: &[],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &current_depth,
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            }),
                            stencil_ops: None,
                        }),
                        ..Default::default()
                    });
                    pass.set_pipeline(&pipeline);
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.draw(0..6, 0..1);
                }
                let curr = jittered_camera(reversed, index, jitter, 0.);
                context.prepare_resources(
                    &device,
                    &FrameDesc {
                        index,
                        width: SIZE[0],
                        height: SIZE[1],
                        output_width: SIZE[0],
                        output_height: SIZE[1],
                    },
                    if reversed {
                        post_fx_context::FeatureFlags::REVERSED_DEPTH
                    } else {
                        post_fx_context::FeatureFlags::NONE
                    },
                );
                taa.prepare_resources(&device, &mut encoder, &context, ADAPTIVE, 0);
                context.execute(&mut RenderAttributes {
                    device: &device,
                    queue: &queue,
                    device_context: &mut encoder,
                    curr_depth_buffer_srv: &current_depth,
                    prev_depth_buffer_srv: prev_depth.as_ref().unwrap_or(&current_depth),
                    curr_camera: Some(&curr),
                    prev_camera: Some(&prev_camera),
                    camera_attribs_cb: None,
                    pass_timestamps: None,
                });
                taa.execute(&mut temporal_anti_aliasing::RenderAttributes {
                    device: &device,
                    queue: &queue,
                    device_context: &mut encoder,
                    post_fx_context: &mut context,
                    color_buffer_srv: &color,
                    depth_buffer_srv: &current_depth,
                    motion_vectors_srv: &motion,
                    taa_attribs: &TemporalAntiAliasingAttribs {
                        temporal_stability_factor: 0.88,
                        rigid_object_motion: u32::from(report_motion && moved),
                        ..Default::default()
                    },
                    accumulation_buffer_idx: 0,
                    pass_timestamps: None,
                });
                queue.submit([encoder.finish()]);
                let output = read_rgba16f(&device, &queue, taa.get_accumulated_frame_srv(false, 0));
                if index >= 32 && report_motion {
                    let red = output[(24 * SIZE[0] + 25) as usize][0];
                    assert!(
                        (red - 0.05).abs() < 0.02,
                        "resting strip left background ghost: reversed={reversed}, frame={index}, red={red}"
                    );
                }
                if index == 32 && !report_motion {
                    let red = output[(24 * SIZE[0] + 25) as usize][0];
                    assert!(
                        red > 0.5,
                        "negative control no longer exercises missing rigid-motion evidence: {red}"
                    );
                }
                prev_depth = Some(current_depth);
                prev_camera = curr;
            }
        }
    }
}
