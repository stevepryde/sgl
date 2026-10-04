//! Every feature permutation of each effect builds its pipelines, binds its
//! resources and executes on a real device (wgpu's validation panics on a
//! WGSL or binding error), and its output stays finite over frames.
#![cfg(not(target_arch = "wasm32"))]
use sgl_post_fx::post_fx_context::{self, FrameDesc, PostFXContext, RenderAttributes};
use sgl_post_fx::screen_space_reflection::{self, FeatureFlags, ScreenSpaceReflection};
use sgl_post_fx::temporal_anti_aliasing::{self, TemporalAntiAliasing};
use sgl_post_fx::{CameraAttribs, ScreenSpaceReflectionAttribs, TemporalAntiAliasingAttribs};

const SIZE: [u32; 2] = [64, 48];

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let required = std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    match pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default())) {
        Ok(adapter) => {
            Some(pollster::block_on(adapter.request_device(&Default::default())).unwrap())
        }
        Err(error) => {
            assert!(
                !required,
                "SGL_REQUIRE_GPU is set but no adapter exists: {error}"
            );
            None
        }
    }
}

fn texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    texel: &[u8],
) -> wgpu::TextureView {
    let data: Vec<u8> = texel
        .iter()
        .copied()
        .cycle()
        .take(texel.len() * (SIZE[0] * SIZE[1]) as usize)
        .collect();
    texels(device, queue, format, &data)
}

/// A texture of `data`, row-major texels.
fn texels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    data: &[u8],
) -> wgpu::TextureView {
    use wgpu::util::DeviceExt;
    device
        .create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: SIZE[0],
                    height: SIZE[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            data,
        )
        .create_view(&Default::default())
}

/// A depth buffer cleared to `depth`: depth formats accept no texel uploads.
fn depth(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    depth: f32,
) -> wgpu::TextureView {
    let view = device
        .create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: SIZE[0],
                height: SIZE[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(depth),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        ..Default::default()
    });
    view
}

fn half(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|v| {
            // f32 to IEEE binary16 for the values used here (normal range).
            let bits = v.to_bits();
            let sign = ((bits >> 16) & 0x8000) as u16;
            let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
            let mantissa = ((bits >> 13) & 0x3ff) as u16;
            let h = if *v == 0.0 {
                sign
            } else {
                sign | ((exponent as u16) << 10) | mantissa
            };
            h.to_le_bytes()
        })
        .collect()
}

/// A camera 2 m from a plane it faces, reversed or conventional depth.
fn camera(reversed: bool, frame_index: u32) -> CameraAttribs {
    let (near, far) = (0.1f32, 100.0f32);
    let aspect = SIZE[0] as f32 / SIZE[1] as f32;
    let y = 1.0 / (0.5f32).tan();
    // Left-handed perspective, column-vector convention, columns listed.
    let (m22, m32) = if reversed {
        (near / (near - far), -far * near / (near - far))
    } else {
        (far / (far - near), -far * near / (far - near))
    };
    let proj = [
        y / aspect,
        0.,
        0.,
        0.,
        0.,
        y,
        0.,
        0.,
        0.,
        0.,
        m22,
        1.,
        0.,
        0.,
        m32,
        0.,
    ];
    let identity = [
        1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
    ];
    let inverse = |m: [f32; 16]| {
        // Inverse of the perspective above.
        let mut inv = [0.0f32; 16];
        inv[0] = 1.0 / m[0];
        inv[5] = 1.0 / m[5];
        inv[11] = 1.0 / m[14];
        inv[14] = 1.0;
        inv[15] = -m[10] / m[14];
        inv
    };
    let mut attribs = CameraAttribs {
        f4_viewport_size: [
            SIZE[0] as f32,
            SIZE[1] as f32,
            1.0 / SIZE[0] as f32,
            1.0 / SIZE[1] as f32,
        ],
        ui_frame_index: frame_index,
        m_view: identity,
        m_proj: proj,
        m_view_proj: proj,
        m_view_inv: identity,
        m_proj_inv: inverse(proj),
        m_view_proj_inv: inverse(proj),
        ..Default::default()
    };
    if reversed {
        attribs.set_clip_planes(far, near);
    } else {
        attribs.set_clip_planes(near, far);
    }
    attribs
}

fn finite_output(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) -> bool {
    let texture = view.texture();
    let stride = (SIZE[0] * 8).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(stride * SIZE[1]),
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
                bytes_per_row: Some(stride),
                rows_per_image: None,
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    buffer.map_async(wgpu::MapMode::Read, .., |r| r.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let bytes = buffer.get_mapped_range(..);
    bytes.chunks_exact(2).all(|h| {
        let bits = u16::from_le_bytes([h[0], h[1]]);
        bits & 0x7c00 != 0x7c00
    })
}

#[test]
fn every_feature_permutation_executes() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        for flags in [
            FeatureFlags::NONE,
            FeatureFlags::PREVIOUS_FRAME,
            FeatureFlags::HALF_RESOLUTION,
            FeatureFlags::PREVIOUS_FRAME | FeatureFlags::HALF_RESOLUTION,
        ] {
            let mut context = PostFXContext::new(&device, &queue, Default::default());
            let mut ssr = ScreenSpaceReflection::new(&device);
            let color = texture(
                &device,
                &queue,
                wgpu::TextureFormat::Rgba16Float,
                &half(&[4.0, 2.0, 1.0, 1.0]),
            );
            let normal = texture(
                &device,
                &queue,
                wgpu::TextureFormat::Rgba16Float,
                &half(&[0.0, 0.6, -0.8, 0.0]),
            );
            let material = texture(&device, &queue, wgpu::TextureFormat::R8Unorm, &[40]);
            let motion = texture(
                &device,
                &queue,
                wgpu::TextureFormat::Rg16Float,
                &half(&[0.01, 0.0]),
            );
            let context_flags = if reversed {
                post_fx_context::FeatureFlags::REVERSED_DEPTH
            } else {
                post_fx_context::FeatureFlags::NONE
            };
            let mut encoder = device.create_command_encoder(&Default::default());
            let plane = if reversed { 0.05 } else { 0.95 };
            let current_depth = depth(&device, &mut encoder, plane);
            let previous_depth = depth(&device, &mut encoder, plane);
            for index in 0..3 {
                context.prepare_resources(
                    &device,
                    &FrameDesc {
                        index,
                        width: SIZE[0],
                        height: SIZE[1],
                        output_width: SIZE[0],
                        output_height: SIZE[1],
                    },
                    context_flags,
                );
                ssr.prepare_resources(&device, &mut encoder, &mut context, flags);
                let curr = camera(reversed, index);
                let prev = camera(reversed, index.saturating_sub(1));
                context.execute(&mut RenderAttributes {
                    device: &device,
                    queue: &queue,
                    device_context: &mut encoder,
                    curr_depth_buffer_srv: &current_depth,
                    prev_depth_buffer_srv: &previous_depth,
                    motion_vectors_srv: &motion,
                    curr_camera: Some(&curr),
                    prev_camera: Some(&prev),
                    camera_attribs_cb: None,
                    pass_timestamps: None,
                });
                let status = ssr.execute(&mut screen_space_reflection::RenderAttributes {
                    device: &device,
                    queue: &queue,
                    device_context: &mut encoder,
                    post_fx_context: &mut context,
                    color_buffer_srv: &color,
                    depth_buffer_srv: &current_depth,
                    normal_buffer_srv: &normal,
                    material_buffer_srv: &material,
                    motion_vectors_srv: &motion,
                    ssr_attribs: &ScreenSpaceReflectionAttribs::default(),
                    pass_timestamps: None,
                    reset_accumulation: false,
                    frame_time: 1.0 / 60.0,
                });
                assert_eq!(
                    status,
                    screen_space_reflection::PostFxExecutionStatus::Ready
                );
                queue.submit([std::mem::replace(
                    &mut encoder,
                    device.create_command_encoder(&Default::default()),
                )
                .finish()]);
            }
            assert!(
                finite_output(&device, &queue, ssr.get_ssr_radiance_srv()),
                "non-finite output, reversed {reversed}, flags {flags:?}"
            );
        }
    }
}

#[test]
fn every_taa_feature_permutation_executes() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        for bits in 0..8 {
            let flags = temporal_anti_aliasing::FeatureFlags(bits);
            let mut context = PostFXContext::new(&device, &queue, Default::default());
            let mut taa = TemporalAntiAliasing::new(&device);
            // A constant image: the clip direction is zero in every channel.
            let color = texture(
                &device,
                &queue,
                wgpu::TextureFormat::Rgba16Float,
                &half(&[4.0, 2.0, 1.0, 1.0]),
            );
            let motion = texture(
                &device,
                &queue,
                wgpu::TextureFormat::Rg16Float,
                &half(&[0.01, 0.0]),
            );
            let context_flags = if reversed {
                post_fx_context::FeatureFlags::REVERSED_DEPTH
            } else {
                post_fx_context::FeatureFlags::NONE
            };
            let mut encoder = device.create_command_encoder(&Default::default());
            let plane = if reversed { 0.05 } else { 0.95 };
            let current_depth = depth(&device, &mut encoder, plane);
            let previous_depth = depth(&device, &mut encoder, plane);
            for index in 0..4 {
                context.prepare_resources(
                    &device,
                    &FrameDesc {
                        index,
                        width: SIZE[0],
                        height: SIZE[1],
                        output_width: SIZE[0],
                        output_height: SIZE[1],
                    },
                    context_flags,
                );
                taa.prepare_resources(&device, &mut encoder, &context, flags, 0);
                let curr = camera(reversed, index);
                let prev = camera(reversed, index.saturating_sub(1));
                context.execute(&mut RenderAttributes {
                    device: &device,
                    queue: &queue,
                    device_context: &mut encoder,
                    curr_depth_buffer_srv: &current_depth,
                    prev_depth_buffer_srv: &previous_depth,
                    motion_vectors_srv: &motion,
                    curr_camera: Some(&curr),
                    prev_camera: Some(&prev),
                    camera_attribs_cb: None,
                    pass_timestamps: None,
                });
                let status = taa.execute(&mut temporal_anti_aliasing::RenderAttributes {
                    device: &device,
                    queue: &queue,
                    device_context: &mut encoder,
                    post_fx_context: &mut context,
                    color_buffer_srv: &color,
                    taa_attribs: &TemporalAntiAliasingAttribs::default(),
                    accumulation_buffer_idx: 0,
                    pass_timestamps: None,
                });
                // Upstream copies the input until a frame's resources were
                // prepared with the pipeline already created.
                if index > 0 {
                    assert_eq!(
                        status,
                        screen_space_reflection::PostFxExecutionStatus::Ready
                    );
                }
                queue.submit([std::mem::replace(
                    &mut encoder,
                    device.create_command_encoder(&Default::default()),
                )
                .finish()]);
            }
            assert!(
                finite_output(&device, &queue, taa.get_accumulated_frame_srv(false, 0)),
                "non-finite output, reversed {reversed}, flags {flags:?}"
            );
        }
    }
}

/// Texels of an RGBA16F view (normal and zero halves only).
fn read_rgba16f(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    view: &wgpu::TextureView,
) -> Vec<[f32; 4]> {
    let texture = view.texture();
    let stride = (SIZE[0] * 8).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(stride * SIZE[1]),
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
                bytes_per_row: Some(stride),
                rows_per_image: None,
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    buffer.map_async(wgpu::MapMode::Read, .., |r| r.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let bytes = buffer.get_mapped_range(..);
    let half = |h: u16| {
        let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
        let exponent = i32::from((h >> 10) & 0x1f);
        let mantissa = f32::from(h & 0x3ff);
        if exponent == 0 {
            sign * mantissa * 2f32.powi(-24)
        } else {
            sign * (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15)
        }
    };
    (0..SIZE[1])
        .flat_map(|y| (0..SIZE[0]).map(move |x| (y * stride + x * 8) as usize))
        .map(|at| {
            std::array::from_fn(|c| {
                half(u16::from_le_bytes([
                    bytes[at + 2 * c],
                    bytes[at + 2 * c + 1],
                ]))
            })
        })
        .collect()
}

// PROVENANCE.md DFX-14: history survives fast motion that is consistent
// between frames (upstream rejects any pixel moving faster than
// 1/TAA_MOTION_VECTOR_DIFF_FACTOR of the viewport per frame) and is rejected
// where the motion changes. The accumulated weight is the output's alpha:
// 0.5 for a pixel without history (`ResetAccumulation`'s value, and
// `ComputeCorrectedAlpha(0)`).
#[test]
fn taa_rejects_history_by_motion_change_not_speed() {
    let Some((device, queue)) = device() else {
        return;
    };
    // 0.04 NDC across 64 pixels: 1.28 pixels per frame, over five times the
    // upstream speed limit.
    let motions = |alternate: bool| {
        [0.04f32, if alternate { -0.04 } else { 0.04 }].map(|x| {
            texture(
                &device,
                &queue,
                wgpu::TextureFormat::Rg16Float,
                &half(&[x, 0.0]),
            )
        })
    };
    let alpha = |alternate: bool| {
        let mut context = PostFXContext::new(&device, &queue, Default::default());
        let mut taa = TemporalAntiAliasing::new(&device);
        let color = texture(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba16Float,
            &half(&[4.0, 2.0, 1.0, 1.0]),
        );
        let motion = motions(alternate);
        let mut encoder = device.create_command_encoder(&Default::default());
        let current_depth = depth(&device, &mut encoder, 0.05);
        let previous_depth = depth(&device, &mut encoder, 0.05);
        let camera = camera(true, 0);
        for index in 0..8u32 {
            context.prepare_resources(
                &device,
                &FrameDesc {
                    index,
                    width: SIZE[0],
                    height: SIZE[1],
                    output_width: SIZE[0],
                    output_height: SIZE[1],
                },
                post_fx_context::FeatureFlags::REVERSED_DEPTH,
            );
            taa.prepare_resources(
                &device,
                &mut encoder,
                &context,
                temporal_anti_aliasing::FeatureFlags::NONE,
                0,
            );
            context.execute(&mut RenderAttributes {
                device: &device,
                queue: &queue,
                device_context: &mut encoder,
                curr_depth_buffer_srv: &current_depth,
                prev_depth_buffer_srv: &previous_depth,
                motion_vectors_srv: &motion[index as usize % 2],
                curr_camera: Some(&camera),
                prev_camera: Some(&camera),
                camera_attribs_cb: None,
                pass_timestamps: None,
            });
            taa.execute(&mut temporal_anti_aliasing::RenderAttributes {
                device: &device,
                queue: &queue,
                device_context: &mut encoder,
                post_fx_context: &mut context,
                color_buffer_srv: &color,
                taa_attribs: &TemporalAntiAliasingAttribs::default(),
                accumulation_buffer_idx: 0,
                pass_timestamps: None,
            });
            queue.submit([std::mem::replace(
                &mut encoder,
                device.create_command_encoder(&Default::default()),
            )
            .finish()]);
        }
        let texels = read_rgba16f(&device, &queue, taa.get_accumulated_frame_srv(false, 0));
        texels[(SIZE[1] / 2 * SIZE[0] + SIZE[0] / 2) as usize][3]
    };
    let consistent = alpha(false);
    assert!(
        consistent > 0.5,
        "consistent fast motion lost its history (weight {consistent})"
    );
    let changing = alpha(true);
    assert_eq!(changing, 0.5, "changing motion kept its history");
}

/// A depth buffer of per-texel `values`, row-major, written by a full-screen
/// pass: depth formats accept no texel uploads.
fn depth_values(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    values: &[f32],
) -> wgpu::TextureView {
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let source = texels(device, queue, wgpu::TextureFormat::R32Float, &bytes);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(
            "@group(0) @binding(0) var values: texture_2d<f32>;
             @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
                 let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
                 return vec4<f32>(p * 4.0 - 1.0, 0.0, 1.0);
             }
             @fragment fn fs(@builtin(position) p: vec4<f32>) -> @builtin(frag_depth) f32 {
                 return textureLoad(values, vec2<i32>(p.xy), 0).x;
             }"
            .into(),
        ),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }],
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            }),
        ),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[],
        }),
        multiview_mask: None,
        cache: None,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&source),
        }],
    });
    let view = depth(device, encoder, 1.0);
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        ..Default::default()
    });
    pass.set_pipeline(&pipeline);
    pass.set_bind_group(0, &group, &[]);
    pass.draw(0..3, 0..1);
    drop(pass);
    view
}

/// Inverse of a 4x4 matrix (Gauss-Jordan with partial pivoting).
fn inverse(m: [f32; 16]) -> [f32; 16] {
    let mut a: Vec<[f64; 8]> = (0..4)
        .map(|r| {
            std::array::from_fn(|c| {
                if c < 4 {
                    f64::from(m[r * 4 + c])
                } else {
                    f64::from(u8::from(c - 4 == r))
                }
            })
        })
        .collect();
    for c in 0..4 {
        let pivot = (c..4)
            .max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))
            .unwrap();
        a.swap(c, pivot);
        let p = a[c][c];
        a[c].iter_mut().for_each(|v| *v /= p);
        for r in (0..4).filter(|&r| r != c) {
            let f = a[r][c];
            let row = a[c];
            a[r].iter_mut().zip(row).for_each(|(v, s)| *v -= f * s);
        }
    }
    std::array::from_fn(|i| a[i / 4][4 + i % 4] as f32)
}

// PROVENANCE.md DFX-15: under a projection with off-centre terms (TAA
// jitter) each ray reads the texel its reflection hits. A mirror floor at
// y=-1, seen from the origin looking along +z, reflects a wall at z=8. Each
// texel's colour is its own coordinates, so the output names the texel each
// floor pixel's ray read; the expected texel is the mirror image projected
// with the jittered projection. Upstream reconstructs view space without the
// off-centre terms, which bends every screen-space ray by the jitter scaled
// by the ray's length (several texels here).
#[test]
fn ssr_hits_the_mirror_image_under_a_jittered_projection() {
    let Some((device, queue)) = device() else {
        return;
    };
    const WALL: f64 = 8.0;
    let [width, height] = SIZE.map(f64::from);
    let jitter = [6.0 / width, -4.0 / height];
    let mut curr = camera(false, 0);
    curr.m_proj[8] = jitter[0] as f32;
    curr.m_proj[9] = jitter[1] as f32;
    curr.m_view_proj = curr.m_proj;
    curr.m_proj_inv = inverse(curr.m_proj);
    curr.m_view_proj_inv = curr.m_proj_inv;
    curr.f2_jitter = jitter.map(|j| j as f32);
    let p = curr.m_proj.map(f64::from);
    let (a, b, m22, m32) = (p[0], p[5], p[10], p[14]);
    // The surface seen through each pixel centre: floor or wall.
    let pixels: Vec<_> = (0..SIZE[1])
        .flat_map(|y| (0..SIZE[0]).map(move |x| (x, y)))
        .map(|(x, y)| {
            let ndc = [
                (f64::from(x) + 0.5) / width * 2.0 - 1.0,
                1.0 - (f64::from(y) + 0.5) / height * 2.0,
            ];
            let ray = [(ndc[0] - jitter[0]) / a, (ndc[1] - jitter[1]) / b, 1.0];
            let floor_z = if ray[1] < 0.0 {
                -1.0 / ray[1]
            } else {
                f64::INFINITY
            };
            let z = floor_z.min(WALL);
            ((x, y), ray.map(|r| r * z), floor_z < WALL)
        })
        .collect();
    let depth_of = |z: f64| ((z * m22 + m32) / z) as f32;
    let mut normals = Vec::new();
    let mut roughness = Vec::new();
    let mut color = Vec::new();
    let mut depths = Vec::new();
    for &((x, y), point, floor) in &pixels {
        normals.extend(half(if floor {
            &[0.0, 1.0, 0.0, 0.0]
        } else {
            &[0.0, 0.0, -1.0, 0.0]
        }));
        roughness.push(if floor { 0 } else { 255 });
        color.extend(half(&[x as f32, y as f32, 0.0, 1.0]));
        depths.push(depth_of(point[2]));
    }
    let mut encoder = device.create_command_encoder(&Default::default());
    let current_depth = depth_values(&device, &queue, &mut encoder, &depths);
    let previous_depth = depth(&device, &mut encoder, 1.0);
    let normal = texels(&device, &queue, wgpu::TextureFormat::Rgba16Float, &normals);
    let material = texels(&device, &queue, wgpu::TextureFormat::R8Unorm, &roughness);
    let color = texels(&device, &queue, wgpu::TextureFormat::Rgba16Float, &color);
    let motion = texture(
        &device,
        &queue,
        wgpu::TextureFormat::Rg16Float,
        &half(&[0.0, 0.0]),
    );
    // No fade-in: one execution, whose transition alpha would otherwise be
    // 0 (DFX-24), as SGL3D configures the context.
    let mut context = PostFXContext::new(
        &device,
        &queue,
        post_fx_context::CreateInfo {
            transition_duration: 0.0,
            ..Default::default()
        },
    );
    let mut ssr = ScreenSpaceReflection::new(&device);
    context.prepare_resources(
        &device,
        &FrameDesc {
            index: 0,
            width: SIZE[0],
            height: SIZE[1],
            output_width: SIZE[0],
            output_height: SIZE[1],
        },
        post_fx_context::FeatureFlags::NONE,
    );
    ssr.prepare_resources(&device, &mut encoder, &mut context, FeatureFlags::NONE);
    context.execute(&mut RenderAttributes {
        device: &device,
        queue: &queue,
        device_context: &mut encoder,
        curr_depth_buffer_srv: &current_depth,
        prev_depth_buffer_srv: &previous_depth,
        motion_vectors_srv: &motion,
        curr_camera: Some(&curr),
        prev_camera: Some(&curr),
        camera_attribs_cb: None,
        pass_timestamps: None,
    });
    ssr.execute(&mut screen_space_reflection::RenderAttributes {
        device: &device,
        queue: &queue,
        device_context: &mut encoder,
        post_fx_context: &mut context,
        color_buffer_srv: &color,
        depth_buffer_srv: &current_depth,
        normal_buffer_srv: &normal,
        material_buffer_srv: &material,
        motion_vectors_srv: &motion,
        ssr_attribs: &ScreenSpaceReflectionAttribs::default(),
        pass_timestamps: None,
        reset_accumulation: true,
        frame_time: 1.0 / 60.0,
    });
    queue.submit([encoder.finish()]);
    let output = read_rgba16f(&device, &queue, ssr.get_ssr_radiance_srv());
    let mut checked = 0;
    for (&(_, point, floor), read) in pixels.iter().zip(&output) {
        if !floor || read[3] == 0.0 {
            continue;
        }
        // The mirror ray from the floor point to the wall, projected.
        let length = point.iter().map(|v| v * v).sum::<f64>().sqrt();
        let mirror = [point[0] / length, -point[1] / length, point[2] / length];
        let s = (WALL - point[2]) / mirror[2];
        let hit: [f64; 3] = std::array::from_fn(|i| point[i] + s * mirror[i]);
        let expected = [
            ((hit[0] * a / hit[2] + jitter[0]) + 1.0) / 2.0 * width,
            (1.0 - (hit[1] * b / hit[2] + jitter[1])) / 2.0 * height,
        ]
        .map(f64::floor);
        // Radiance is premultiplied by confidence (DFX-17).
        let texel = [read[0] / read[3], read[1] / read[3]].map(f64::from);
        let error = [texel[0] - expected[0], texel[1] - expected[1]];
        assert!(
            error.iter().all(|e| e.abs() <= 1.0),
            "floor point {point:?} read texel {texel:?}, its mirror image is texel {expected:?}"
        );
        checked += 1;
    }
    assert!(
        checked > 200,
        "only {checked} floor pixels reflected the wall"
    );
}
