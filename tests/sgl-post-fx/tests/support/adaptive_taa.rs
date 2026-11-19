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
    /// A far surface at rest until frame 40, then the sky.
    RevealSky,
    /// The sky until frame 40, then a far surface at rest.
    CoverSky,
    Toggle,
}

/// The far surface of `RevealSky` and `CoverSky`, at view Z 90 under the
/// camera's 100 m far plane.
const FAR_SURFACE_Z: f32 = 90.;

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
        let occluded = match signal {
            Signal::Reveal | Signal::RevealSky => index < 40,
            Signal::CoverSky => index >= 40,
            _ => false,
        };
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
        // Every surface here keeps its view depth (the camera pans across
        // the plane, the occluder rests): depth motion zero and valid.
        // Frame 0, which only the placeholder reads, carries poison in Z:
        // the placeholder must initialize its own recovery hold rather than
        // copy the caller's channel, or the following frames would hold full
        // rejection for about 88 frames and fail the stabilization checks.
        let depth_motion = if index == 0 { 100. } else { 0. };
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
                depth_motion,
                1.,
            ]),
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        let sky = matches!(signal, Signal::RevealSky | Signal::CoverSky) && !occluded;
        let z = match signal {
            _ if !occluded => 2.,
            Signal::RevealSky | Signal::CoverSky => FAR_SURFACE_Z,
            _ => 0.4,
        };
        let clip_depth = if sky {
            curr.f_far_plane_depth
        } else {
            curr.m_proj[10] + curr.m_proj[14] / z
        };
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
            responsive_masks: &[],
            neighborhood_motion: None,
            taa_attribs: &TemporalAntiAliasingAttribs {
                temporal_stability_factor: 0.88,
                camera_motion: u32::from(TemporalAntiAliasing::camera_moved(
                    &curr,
                    &previous_camera,
                )),
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

// The revealed background supplies valid depth motion (zero: it rests), which
// places its previous surface at its own depth, not the occluder's that the
// previous depth buffer holds. Defect: trusting valid depth motion by
// skipping the depth test, as Filament skips it on object motion.
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

// #791: the sky is categorical in the adaptive depth test. The far surface
// lies at normalized depth 0.1 (100 - 90) / (90 * 99.9) = 1.1e-4, a ninth of
// the depth threshold 0.001, so its depth difference from the sky rejects
// only smoothstep(0, 0.001, 1.1e-4) = 0.034 of history. Defect: a far
// surface leaving the sky behind it, or appearing over it, with no motion
// and no camera change keeps most of the old colour. Oracle: the first
// changed frame's disocclusion is whole, so the resolve is that frame's own
// colour, within the stores the Reveal test allows.
#[test]
fn taa_adaptive_sky_and_a_far_surface_replacing_each_other_reject_history() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        for signal in [Signal::RevealSky, Signal::CoverSky] {
            let frames = sequence(&device, &queue, reversed, true, signal);
            let (output, current) = frames[40];
            assert!(
                (output - current).abs() < 0.002,
                "reversed={reversed}: the old colour stayed where the sky and a far surface swapped: {output}/{current}"
            );
        }
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
                // Poison in frame 0's Z, which only the placeholder reads.
                let depth_motion = if index == 0 { 100. } else { 0. };
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
                            depth_motion,
                            1.,
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
                    responsive_masks: &[],
                    neighborhood_motion: None,
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

#[derive(Clone, Copy, Debug)]
enum DepthMotion {
    /// The camera moves 0.5 forward a frame and the plane, which it follows,
    /// stays 2 ahead: zero screen and depth motion, while a static point 2
    /// ahead was 2.5 ahead of the previous camera.
    FollowedByCamera,
    /// A still camera, and a plane whose points move along their own view
    /// rays between 2 and 2.5 ahead: zero screen motion, depth motion ±0.5
    /// (exact in half precision).
    AlongViewRays,
}

/// The centre pixel's resolved colour, the colour drawn there, and the colour
/// drawn there the previous frame, per frame. Columns alternate between 0.8
/// and 0.05 and swap every frame, so each 3x3 neighbourhood holds both values
/// and the variance box does not clip a temporal blend away.
fn depth_motion_sequence(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    reversed: bool,
    case: DepthMotion,
) -> Vec<(f32, f32, f32)> {
    const FORWARD: f32 = 0.5;
    let mut context = PostFXContext::new(device, queue, Default::default());
    let mut taa = TemporalAntiAliasing::new(device);
    let mut previous_camera = jittered_camera(reversed, 0, [0.; 2], 0.);
    let mut previous_depth = None;
    let mut previous_z = 2.;
    let value = |x: u32, index: u32| {
        if (x + index).is_multiple_of(2) {
            0.8
        } else {
            0.05
        }
    };
    let mut samples = Vec::new();
    for index in 0u32..48 {
        let jitter = [halton(2, index % 16 + 1), halton(3, index % 16 + 1)];
        let mut curr = jittered_camera(reversed, index, jitter, 0.);
        let z = match case {
            DepthMotion::FollowedByCamera => {
                curr = moved(curr, [0., 0., FORWARD * index as f32], 0.);
                2.
            }
            DepthMotion::AlongViewRays => {
                if index.is_multiple_of(2) {
                    2.
                } else {
                    2.5
                }
            }
        };
        let colour: Vec<f32> = (0..SIZE[1])
            .flat_map(|_| (0..SIZE[0]).flat_map(move |x| [value(x, index); 4]))
            .collect();
        let color = texels(
            device,
            queue,
            wgpu::TextureFormat::Rgba16Float,
            &half(&colour),
        );
        // Previous minus current view depth, valid.
        let motion = texture(
            device,
            queue,
            wgpu::TextureFormat::Rgba32Float,
            bytemuck::cast_slice(&[0f32, 0., previous_z - z, 1.]),
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        let current_depth = depth(device, &mut encoder, curr.m_proj[10] + curr.m_proj[14] / z);
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
        taa.prepare_resources(device, &mut encoder, &context, ADAPTIVE, 0);
        context.execute(&mut RenderAttributes {
            device,
            queue,
            device_context: &mut encoder,
            curr_depth_buffer_srv: &current_depth,
            prev_depth_buffer_srv: previous_depth.as_ref().unwrap_or(&current_depth),
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
            responsive_masks: &[],
            neighborhood_motion: None,
            taa_attribs: &TemporalAntiAliasingAttribs {
                temporal_stability_factor: 0.88,
                camera_motion: u32::from(TemporalAntiAliasing::camera_moved(
                    &curr,
                    &previous_camera,
                )),
                ..Default::default()
            },
            accumulation_buffer_idx: 0,
            pass_timestamps: None,
        });
        queue.submit([encoder.finish()]);
        let frame = read_rgba16f(device, queue, taa.get_accumulated_frame_srv(false, 0));
        let (x, y) = (SIZE[0] / 2, SIZE[1] / 2);
        samples.push((
            frame[(y * SIZE[0] + x) as usize][0],
            value(x, index),
            value(x, index + 1),
        ));
        previous_camera = curr;
        previous_depth = Some(current_depth);
        previous_z = z;
    }
    samples
}

/// Defect: the adaptive depth test expecting the previous depth a static
/// point would have had, so a surface whose view depth changed differently
/// (one the camera follows, one moving along the view axis) is rejected as
/// disoccluded every frame and resolves to the current frame alone. Oracle:
/// TAA blends in x/(1+x) space, and with history weight h a two-valued
/// alternation settles h/(1+h) of the way to the other value: 0.468 at the
/// moving camera's full-strength 0.88, 0.49 at the still camera's relaxed
/// 0.964. A rejected history leaves 0.
#[test]
fn taa_adaptive_keeps_history_of_surfaces_moving_in_depth() {
    let Some((device, queue)) = device() else {
        return;
    };
    let sdr = |v: f32| v / (1. + v);
    for case in [DepthMotion::FollowedByCamera, DepthMotion::AlongViewRays] {
        for reversed in [false, true] {
            let frames = depth_motion_sequence(&device, &queue, reversed, case);
            for (index, &(output, current, other)) in frames.iter().enumerate().skip(24) {
                let share = (sdr(output) - sdr(current)) / (sdr(other) - sdr(current));
                assert!(
                    (0.4..0.6).contains(&share),
                    "{case:?}, reversed={reversed}, frame {index}: output {output} is {share} of \
                     the way from the current {current} to the previous {other}"
                );
            }
        }
    }
}

/// What a step sequence gives the resolve beside its colour.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Responsive {
    /// Nothing.
    None,
    /// One responsive mask holding this value everywhere.
    Mask(u8),
    /// Two masks, the first 0 and the second this value: the larger counts.
    SecondMask(u8),
    /// One mask holding this value everywhere before the step and 0 from
    /// it: the content that marked the pixel leaves as the value steps.
    MaskUntilStep(u8),
    /// A neighbourhood motion of this many pixels along X, in 32-pixel tiles,
    /// the pixel's own motion zero.
    Reach(f32),
    /// The pixel at the jittered edge of still content marked this value:
    /// the content wholly covers the pixel to its left, marked on every
    /// frame, and covers part of the pixel, whose sample hits it on the
    /// frames `edge_hit` names, when it shows `AFTER` and is marked too,
    /// and `BEFORE` and unmarked on the others. No step.
    JitteredEdge(u8),
}

/// The frames whose jittered sample hits the content beside a
/// `Responsive::JitteredEdge` pixel: 5 of every 16 phases, irregularly, as
/// a 16-phase sequence hits a pixel the content covers 5/16 of.
fn edge_hit(index: u32) -> bool {
    (index * 7) % 16 < 5
}

/// The checkerboard's radiances, and the centre pixel's before and after
/// its step. Around the centre the board's 3x3 neighbourhood spans both
/// board values, so its variance box (one standard deviation about the
/// mean at this height, DFX-14) holds every blend of the two centre
/// values: rectification leaves the history alone and only feedback acts.
const BOARD: [f32; 2] = [0.8, 0.05];
const BEFORE: f32 = 0.15;
const AFTER: f32 = 0.35;
const STEP_FRAME: u32 = 40;

/// The centre pixel's resolved value per frame, of a still, unjittered
/// camera on a plane at Z = 2 showing a checkerboard whose centre pixel
/// steps from `BEFORE` to `AFTER` at `STEP_FRAME`, with `responsive`.
fn step_sequence(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    reversed: bool,
    responsive: Responsive,
) -> Vec<f32> {
    use wgpu::util::DeviceExt;
    let mut context = PostFXContext::new(device, queue, Default::default());
    let mut taa = TemporalAntiAliasing::new(device);
    let camera = jittered_camera(reversed, 0, [0.; 2], 0.);
    let centre = [SIZE[0] / 2, SIZE[1] / 2];
    let r8 = |value: u8| texture(device, queue, wgpu::TextureFormat::R8Unorm, &[value]);
    let masks: Vec<wgpu::TextureView> = match responsive {
        Responsive::Mask(value) => vec![r8(value)],
        Responsive::SecondMask(value) => vec![r8(0), r8(value)],
        Responsive::MaskUntilStep(value) => vec![r8(value), r8(0)],
        Responsive::JitteredEdge(value) => {
            let marked = |pixels: &[[u32; 2]]| {
                let data: Vec<u8> = (0..SIZE[1])
                    .flat_map(|y| (0..SIZE[0]).map(move |x| [x, y]))
                    .map(|p| if pixels.contains(&p) { value } else { 0 })
                    .collect();
                texels(device, queue, wgpu::TextureFormat::R8Unorm, &data)
            };
            let left = [centre[0] - 1, centre[1]];
            vec![marked(&[left]), marked(&[left, centre])]
        }
        _ => Vec::new(),
    };
    let tiles = [SIZE[0].div_ceil(32), SIZE[1].div_ceil(32)];
    let reach = match responsive {
        Responsive::Reach(pixels) => Some(
            device
                .create_texture_with_data(
                    queue,
                    &wgpu::TextureDescriptor {
                        label: None,
                        size: wgpu::Extent3d {
                            width: tiles[0],
                            height: tiles[1],
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba32Float,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                    wgpu::util::TextureDataOrder::LayerMajor,
                    bytemuck::cast_slice(
                        &[[pixels, 0f32, 0., 0.]].repeat((tiles[0] * tiles[1]) as usize),
                    ),
                )
                .create_view(&Default::default()),
        ),
        _ => None,
    };
    let mask_views: Vec<&wgpu::TextureView> = masks.iter().collect();
    // The mask of each frame: before the step and from it.
    let until_step = |index: u32| -> Vec<&wgpu::TextureView> {
        match responsive {
            Responsive::MaskUntilStep(_) => vec![&masks[usize::from(index >= STEP_FRAME)]],
            Responsive::JitteredEdge(_) => vec![&masks[usize::from(edge_hit(index))]],
            _ => mask_views.clone(),
        }
    };
    let mut previous_depth = None;
    let mut out = Vec::new();
    for index in 0u32..STEP_FRAME + 24 {
        let mut colour = Vec::new();
        for y in 0..SIZE[1] {
            for x in 0..SIZE[0] {
                let value = if [x, y] == centre {
                    let after = match responsive {
                        Responsive::JitteredEdge(_) => edge_hit(index),
                        _ => index >= STEP_FRAME,
                    };
                    if after { AFTER } else { BEFORE }
                } else {
                    BOARD[((x + y) % 2) as usize]
                };
                colour.extend_from_slice(&[value, value, value, 1.]);
            }
        }
        let color = texels(
            device,
            queue,
            wgpu::TextureFormat::Rgba16Float,
            &half(&colour),
        );
        let motion = texture(
            device,
            queue,
            wgpu::TextureFormat::Rgba32Float,
            bytemuck::cast_slice(&[0f32, 0., 0., 1.]),
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        let current_depth = depth(
            device,
            &mut encoder,
            camera.m_proj[10] + camera.m_proj[14] / 2.,
        );
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
        taa.prepare_resources(device, &mut encoder, &context, ADAPTIVE, 0);
        context.execute(&mut RenderAttributes {
            device,
            queue,
            device_context: &mut encoder,
            curr_depth_buffer_srv: &current_depth,
            prev_depth_buffer_srv: previous_depth.as_ref().unwrap_or(&current_depth),
            curr_camera: Some(&camera),
            prev_camera: Some(&camera),
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
            responsive_masks: &until_step(index),
            neighborhood_motion: reach
                .as_ref()
                .map(|srv| temporal_anti_aliasing::NeighborhoodMotion { srv, tile_size: 32 }),
            taa_attribs: &TemporalAntiAliasingAttribs {
                temporal_stability_factor: 0.88,
                ..Default::default()
            },
            accumulation_buffer_idx: 0,
            pass_timestamps: None,
        });
        queue.submit([encoder.finish()]);
        let frame = read_rgba16f(device, queue, taa.get_accumulated_frame_srv(false, 0));
        out.push(frame[(centre[1] * SIZE[0] + centre[0]) as usize][0]);
        previous_depth = Some(current_depth);
    }
    out
}

/// The share of the step from `BEFORE` to `AFTER` each frame from the step
/// on has made, in the blend space x / (1 + x): the first after one
/// resolve of the new value.
fn step_shares(values: &[f32]) -> Vec<f32> {
    let sdr = |v: f32| v / (1. + v);
    values[STEP_FRAME as usize..]
        .iter()
        .map(|&v| (sdr(v) - sdr(BEFORE)) / (sdr(AFTER) - sdr(BEFORE)))
        .collect()
}

/// #795. Defect: content the caller marks as changing without a motion
/// vector (a responsive mask) is resolved at rest with the relaxed blend,
/// or the mask changes an unmarked pixel. Oracle, by hand from the spec's
/// blend (Frame item 11): at rest after the hold, with the history inside
/// the variance box, a pixel takes 1 - 0.88 x 0.3 = 0.036 of the current
/// frame unmarked, so n frames after a step it has made 1 - 0.964^n of it;
/// fully marked it takes 0.12, and has made 1 - 0.88^n, whichever of two
/// masks marks it; marked 0.5 (128 of 255), in proportion,
/// 0.12 (0.3 + 0.7 x 128 / 255). Each frame's store rounds to half
/// precision: a step of the value, about 0.002 of the step's share here,
/// over the frames. A mask of zero changes nothing: the same values as no
/// mask.
#[test]
fn taa_adaptive_a_marked_pixel_follows_a_step_at_full_feedback() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        let unmarked = step_sequence(&device, &queue, reversed, Responsive::None);
        for (n, share) in step_shares(&unmarked).into_iter().enumerate() {
            let expected = 1. - 0.964f32.powi(n as i32 + 1);
            assert!(
                (share - expected).abs() < 0.01,
                "reversed={reversed}: unmarked, {} resolves into the step it made {share}, not {expected}",
                n + 1
            );
        }
        assert_eq!(
            step_sequence(&device, &queue, reversed, Responsive::Mask(0)),
            unmarked,
            "reversed={reversed}: a zero mask changed the resolve"
        );
        for (responsive, mask) in [
            (Responsive::Mask(255), 1.),
            (Responsive::SecondMask(255), 1.),
            (Responsive::Mask(128), 128. / 255.),
        ] {
            let marked = step_sequence(&device, &queue, reversed, responsive);
            let kept: f32 = 1. - 0.12 * (0.3 + 0.7 * mask);
            for (n, share) in step_shares(&marked).into_iter().enumerate() {
                let expected = 1. - kept.powi(n as i32 + 1);
                assert!(
                    (share - expected).abs() < 0.01,
                    "reversed={reversed}, {responsive:?}: {} resolves into the step it made {share}, not {expected}",
                    n + 1
                );
            }
        }
    }
}

/// #802. Defect: a pixel whose own motion is zero but within reach of a
/// moving surface (its neighbourhood motion) is resolved at rest; or the
/// neighbourhood motion reprojects it, reading history from where the
/// surface was. Oracles: by hand, as the marked pixel above, a still pixel
/// whose neighbourhood moves 3 px a frame (above the 1 px threshold) has
/// made 1 - 0.88^n of a step n frames on; and equivalence: its values are
/// exactly a fully marked pixel's, whose reprojection is its own still
/// one. Had the 3 px reprojected it, it would read the board's history
/// three pixels away (0.05 or 0.8) instead.
#[test]
fn taa_adaptive_a_still_pixel_within_reach_of_motion_follows_a_step_at_full_feedback() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        let reach = step_sequence(&device, &queue, reversed, Responsive::Reach(3.));
        for (n, share) in step_shares(&reach).into_iter().enumerate() {
            let expected = 1. - 0.88f32.powi(n as i32 + 1);
            assert!(
                (share - expected).abs() < 0.01,
                "reversed={reversed}: within reach, {} resolves into the step it made {share}, not {expected}",
                n + 1
            );
        }
        assert_eq!(
            reach,
            step_sequence(&device, &queue, reversed, Responsive::Mask(255)),
            "reversed={reversed}: within reach, the resolve differs from a fully marked pixel's"
        );
    }
}

/// #795. Defect: a pixel whose content has just left it relaxes at once, or
/// holds only in proportion to the mask it had, so dim content leaves a
/// trail. Oracle, by hand from the spec's rule (Frame item 11): a marked
/// pixel arms the twelve-frame hold in full, and once it is no longer
/// marked the hold's remaining frames are at full strength, whatever its
/// mask was; with the history inside the variance box only feedback acts,
/// 0.12 of the current frame, so n frames after content marked 0.5 left as
/// the value stepped the pixel has made 1 - 0.88^n of the step, as a fully
/// marked one (judged over six frames, well within the hold).
#[test]
fn taa_adaptive_a_pixel_marked_content_left_holds_at_full_strength() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        let left = step_sequence(&device, &queue, reversed, Responsive::MaskUntilStep(128));
        for (n, share) in step_shares(&left).into_iter().enumerate().take(6) {
            let expected = 1. - 0.88f32.powi(n as i32 + 1);
            assert!(
                (share - expected).abs() < 0.01,
                "reversed={reversed}: {} resolves after the content left it made {share}, not {expected}",
                n + 1
            );
        }
    }
}

/// #802. Defect: a neighbourhood motion slower than the motion threshold
/// (a pixel a frame, the radius the resolve's own closest-motion search
/// covers) changes the resolve, costing slowly deforming surfaces' surroundings
/// their resting stability. Oracle, equivalence (Frame item 11: slower
/// motion is the search's): a neighbourhood moving 0.5 px a frame gives
/// exactly the values of no neighbourhood motion.
#[test]
fn taa_adaptive_a_neighbourhood_slower_than_the_threshold_changes_nothing() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        assert_eq!(
            step_sequence(&device, &queue, reversed, Responsive::Reach(0.5)),
            step_sequence(&device, &queue, reversed, Responsive::None),
            "reversed={reversed}"
        );
    }
}

/// B-15 (2026-11-18 history audit, O-2). Defect: a pixel at the jittered
/// edge of still marked content, which its own sample hits on some phases
/// only, is unmarked on the others and runs at full strength through the
/// mask hold, so it alternates between its mask's strength and full and
/// never relaxes, though its content has not changed. Rule (T-25): the
/// resolve takes the masks' largest value over the pixel's 3x3
/// neighbourhood, as AMD's FSR dilates them, so content wholly covering a
/// neighbour marks the pixel on every phase. Oracle, by hand from the
/// spec's blend: with the history inside the variance box (the board's,
/// as `taa_adaptive_a_marked_pixel_follows_a_step_at_full_feedback`), only
/// feedback acts, so once confidence has reached its cap each resolve moves
/// the pixel 0.12 (0.3 + 0.7 x 128 / 255) of the way from its last output
/// to the current sample in the blend space x / (1 + x), whatever that
/// sample is, within the half-float rounding of the two stores (the
/// current colour and the output). Full strength would move it 0.12 of the
/// way: 0.0030 off the model here without the rule.
#[test]
fn taa_adaptive_a_pixel_at_the_jittered_edge_of_still_marked_content_keeps_its_strength() {
    let Some((device, queue)) = device() else {
        return;
    };
    let sdr = |v: f32| v / (1. + v);
    let weight = 0.12 * (0.3 + 0.7 * 128. / 255.);
    for reversed in [false, true] {
        let values = step_sequence(&device, &queue, reversed, Responsive::JitteredEdge(128));
        // The reset's hold ends at frame 12, and confidence climbs from 0.893
        // to its cap over the 18 frames after it. The largest excess over
        // the rounding, with its frame, distance and rounding.
        let mut worst = (f32::NEG_INFINITY, 0, 0., 0.);
        for index in 36..values.len() {
            let current = if edge_hit(index as u32) {
                AFTER
            } else {
                BEFORE
            };
            let last = sdr(values[index - 1]);
            let expected = last + weight * (sdr(current) - last);
            let off = (sdr(values[index]) - expected).abs();
            let rounding = 2. * values[index].max(current) / 1024.;
            if off - rounding > worst.0 {
                worst = (off - rounding, index, off, rounding);
            }
        }
        let (excess, frame, off, rounding) = worst;
        eprintln!(
            "reversed={reversed}: largest distance from the mask's feedback {off} at frame {frame} (rounding {rounding})"
        );
        assert!(
            excess <= 0.,
            "reversed={reversed}, frame {frame}: {off} off the mask's feedback in the blend \
             space, above the rounding {rounding}"
        );
    }
}
