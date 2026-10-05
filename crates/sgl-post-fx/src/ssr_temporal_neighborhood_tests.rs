//! #314: production reconstruction/temporal passes must not treat skipped
//! pixels from an older frame as current evidence. Ray radiance is supplied
//! at the reconstruction boundary, independent of traversal and hit geometry.
use super::*;
use crate::{CameraAttribs, post_fx_context::FrameDesc};
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

/// `camera` turned half around its vertical axis, its view x and z negated:
/// what was ahead of it is behind.
fn turned_around(camera: CameraAttribs) -> CameraAttribs {
    let turn = [
        -1., 0., 0., 0., 0., 1., 0., 0., 0., 0., -1., 0., 0., 0., 0., 1.,
    ];
    // Column-major, columns listed: view-projection P V negates P's columns
    // 0 and 2, its inverse V P^-1 the inverse's rows 0 and 2.
    let columns =
        |m: [f32; 16]| std::array::from_fn(|i| if matches!(i / 4, 0 | 2) { -m[i] } else { m[i] });
    let rows =
        |m: [f32; 16]| std::array::from_fn(|i| if matches!(i % 4, 0 | 2) { -m[i] } else { m[i] });
    CameraAttribs {
        m_view: turn,
        m_view_inv: turn,
        m_view_proj: columns(camera.m_view_proj),
        m_view_proj_inv: rows(camera.m_view_proj_inv),
        ..camera
    }
}

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

#[test]
fn departed_reflections_cannot_support_current_dark_history() {
    let Some((device, queue)) = device() else {
        return;
    };
    let normal = texture(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba16Float,
        &half(&[0.0, 0.0, -1.0, 0.0]),
    );
    // One-pixel horizontal surface reprojection. Constant depth and bright
    // history permit valid history; this must not become a history-reset fix.
    let motion = texture(
        &device,
        &queue,
        wgpu::TextureFormat::Rg16Float,
        &half(&[2.0 / 64.0, 0.0]),
    );
    let mut failures = Vec::new();
    for background in [false, true] {
        for flags in [FeatureFlags::NONE, FeatureFlags::HALF_RESOLUTION] {
            let mut ssr = ScreenSpaceReflection::new(&device);
            let mut context = PostFXContext::new(&device, &queue, Default::default());
            let mut previous_depth = {
                let mut encoder = device.create_command_encoder(&Default::default());
                let result = depth(&device, &mut encoder, 0.95);
                queue.submit([encoder.finish()]);
                result
            };
            // Start dark, establish a bright silhouette, retain valid moving
            // history, then remove the silhouette twice at different borders.
            for index in 0..7 {
                let boundary = if index == 6 { 24 } else { 32 };
                let departed = matches!(index, 0 | 4 | 6);
                let bright = matches!(index, 1 | 2 | 5);
                let supported_history = index == 3;
                let mut encoder = device.create_command_encoder(&Default::default());
                let scene_depth = depth_values(
                    &device,
                    &queue,
                    &mut encoder,
                    &(0..48)
                        .flat_map(|_| {
                            (0..64).map(|x| {
                                if background && departed && x >= boundary {
                                    1.0
                                } else {
                                    0.95
                                }
                            })
                        })
                        .collect::<Vec<_>>(),
                );
                let material = texels(
                    &device,
                    &queue,
                    wgpu::TextureFormat::R8Unorm,
                    &(0..48)
                        .flat_map(|_| {
                            (0..64).map(|x| {
                                if !background && departed && x >= boundary {
                                    230
                                } else {
                                    0
                                }
                            })
                        })
                        .collect::<Vec<_>>(),
                );
                context.prepare_resources(
                    &device,
                    &FrameDesc {
                        index,
                        width: 64,
                        height: 48,
                        output_width: 64,
                        output_height: 48,
                    },
                    post_fx_context::FeatureFlags::NONE,
                );
                ssr.prepare_resources(&device, &mut encoder, &mut context, flags);
                ssr.prepare_shaders_and_pso(&device);
                let current_camera = camera(false, index);
                let previous_camera = camera(false, index.saturating_sub(1));
                context.execute(&mut post_fx_context::RenderAttributes {
                    device: &device,
                    queue: &queue,
                    device_context: &mut encoder,
                    curr_depth_buffer_srv: &scene_depth,
                    prev_depth_buffer_srv: &previous_depth,
                    curr_camera: Some(&current_camera),
                    prev_camera: Some(&previous_camera),
                    camera_attribs_cb: None,
                    pass_timestamps: None,
                });
                // Intersections at this boundary are exact black/white samples.
                // Zero ray length makes reconstruction's minimum weight apply;
                // no duplicated BRDF, clamp, reprojection, or variance math.
                let ray_width = if flags == FeatureFlags::NONE { 64 } else { 32 };
                let ray_height = if flags == FeatureFlags::NONE { 48 } else { 24 };
                let input = (0..ray_height)
                    .flat_map(|_| {
                        (0..ray_width).flat_map(|x| {
                            let value =
                                if bright || (supported_history && x != (31 * ray_width / 64)) {
                                    1.0
                                } else {
                                    0.0
                                };
                            half(&[value, value, value, 1.0])
                        })
                    })
                    .collect::<Vec<_>>();
                use wgpu::util::DeviceExt;
                ssr.resources.as_mut().unwrap().radiance = device
                    .create_texture_with_data(
                        &queue,
                        &wgpu::TextureDescriptor {
                            label: None,
                            size: wgpu::Extent3d {
                                width: ray_width,
                                height: ray_height,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: wgpu::TextureFormat::Rgba16Float,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING,
                            view_formats: &[],
                        },
                        wgpu::util::TextureDataOrder::LayerMajor,
                        &input,
                    )
                    .create_view(&Default::default());
                let attribs = ScreenSpaceReflectionAttribs {
                    temporal_radiance_stability_factor: 0.95,
                    ..Default::default()
                };
                let mut render = RenderAttributes {
                    device: &device,
                    queue: &queue,
                    device_context: &mut encoder,
                    post_fx_context: &mut context,
                    color_buffer_srv: &normal,
                    depth_buffer_srv: &scene_depth,
                    normal_buffer_srv: &normal,
                    material_buffer_srv: &material,
                    motion_vectors_srv: &motion,
                    ssr_attribs: &attribs,
                    pass_timestamps: None,
                    reset_accumulation: false,
                    frame_time: 1.0 / 60.0,
                };
                ssr.update_constant_buffer(&render, false);
                ssr.compute_stencil_mask_and_extract_roughness(&mut render);
                ssr.compute_downsampled_stencil_mask(&mut render);
                ssr.classify_denoiser_tiles(&mut render);
                ssr.compute_spatial_reconstruction(&mut render);
                ssr.compute_temporal_accumulation(&mut render);
                queue.submit([encoder.finish()]);
                let resolved = read_rgba16f(&device, &queue, &ssr.resources().resolved_radiance);
                let temporal = read_rgba16f(
                    &device,
                    &queue,
                    &ssr.resources().radiance_history[(index & 1) as usize],
                );
                let edge = (24 * 64 + boundary - 1) as usize;
                println!(
                    "background={background} {flags:?} frame={index}: active resolved={} inactive neighbor={} temporal edge={}",
                    resolved[edge][0],
                    resolved[edge + 1][0],
                    temporal[edge][0]
                );
                if departed && temporal[edge][0].abs() > 0.001 {
                    failures.push(format!(
                        "{background} {flags:?} frame{index}: dark edge retains {}",
                        temporal[edge][0]
                    ));
                }
                // Sample inside the black ray's footprint. A half-resolution
                // ray covers two full-size pixels; its center differs from the
                // full-resolution silhouette boundary checked above.
                let history_pixel = if flags == FeatureFlags::HALF_RESOLUTION {
                    24 * 64 + 30
                } else {
                    edge
                };
                if supported_history
                    && temporal[history_pixel][0] <= resolved[history_pixel][0] + 0.01
                {
                    failures.push(format!(
                        "valid moving history lost: resolved={} temporal={}",
                        resolved[history_pixel][0], temporal[history_pixel][0]
                    ));
                }
                previous_depth = scene_depth;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Texels of an R16Float view (normal and zero halves only).
fn read_r16f(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) -> Vec<f32> {
    let texture = view.texture();
    let stride = (SIZE[0] * 2).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
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
    (0..SIZE[1])
        .flat_map(|y| (0..SIZE[0]).map(move |x| (y * stride + x * 2) as usize))
        .map(|at| {
            let h = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
            let exponent = i32::from((h >> 10) & 0x1f);
            let mantissa = f32::from(h & 0x3ff);
            if exponent == 0 {
                mantissa * 2f32.powi(-24)
            } else {
                (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15)
            }
        })
        .collect()
}

// PROVENANCE.md DFX-25: the resolved depth the temporal pass reprojects by is
// the depth of the reflection's virtual point, the hit distance beyond the
// surface along the view ray (AMD's FFX_DNSR_Reflections_GetHitPositionReprojection).
// The oracle builds that point with vector maths and projects it through the
// camera's matrix; upstream's formula, which took the camera-to-surface
// distance plus the hit distance as camera Z, lands about 30 % further at the
// 41° off-axis pixel checked here.
#[test]
fn resolved_depth_is_the_virtual_point_along_the_view_ray() {
    let Some((device, queue)) = device() else {
        return;
    };
    // A plane facing the camera 2 m away, reversed depth (R16Float keeps its
    // precision near 0), perceptual roughness 0.1, and every ray 3 m long.
    let (plane_z, hit_distance) = (2.0f32, 3.0f32);
    let camera = camera(true, 0);
    let proj = camera.m_proj;
    // Column-vector convention, columns listed: clip = proj * (v, 1).
    let project = |v: [f32; 3]| -> f32 {
        let clip = |row: usize| {
            proj[row] * v[0] + proj[4 + row] * v[1] + proj[8 + row] * v[2] + proj[12 + row]
        };
        clip(2) / clip(3)
    };
    let pixel = [62u32, 46u32];
    // The pixel centre's view ray, through UV (x + 0.5) / size, NDC y up.
    let ndc = [
        2.0 * (pixel[0] as f32 + 0.5) / SIZE[0] as f32 - 1.0,
        1.0 - 2.0 * (pixel[1] as f32 + 0.5) / SIZE[1] as f32,
    ];
    let surface = [
        ndc[0] * plane_z / proj[0],
        ndc[1] * plane_z / proj[5],
        plane_z,
    ];
    let distance = surface.iter().map(|c| c * c).sum::<f32>().sqrt();
    let virtual_point = surface.map(|c| c + hit_distance * c / distance);
    let expected = project(virtual_point);
    assert!(
        (plane_z / distance).acos().to_degrees() > 38.0,
        "the checked pixel must be well off the view axis"
    );
    // The mirror direction at that pixel off the plane (normal -z), so the
    // reconstruction's BRDF weight is well above its 1e-6 floor.
    let mirror = [surface[0], surface[1], -surface[2]].map(|c| c / distance);
    let normal = texture(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba16Float,
        &half(&[0.0, 0.0, -1.0, 0.0]),
    );
    let motion = texture(
        &device,
        &queue,
        wgpu::TextureFormat::Rg16Float,
        &half(&[0.0, 0.0]),
    );
    let material = texture(&device, &queue, wgpu::TextureFormat::R8Unorm, &[26]);
    let mut ssr = ScreenSpaceReflection::new(&device);
    let mut context = PostFXContext::new(&device, &queue, Default::default());
    let mut encoder = device.create_command_encoder(&Default::default());
    let scene_depth = depth(&device, &mut encoder, project(surface));
    context.prepare_resources(
        &device,
        &FrameDesc {
            index: 0,
            width: SIZE[0],
            height: SIZE[1],
            output_width: SIZE[0],
            output_height: SIZE[1],
        },
        post_fx_context::FeatureFlags::REVERSED_DEPTH,
    );
    ssr.prepare_resources(&device, &mut encoder, &mut context, FeatureFlags::NONE);
    ssr.prepare_shaders_and_pso(&device);
    context.execute(&mut post_fx_context::RenderAttributes {
        device: &device,
        queue: &queue,
        device_context: &mut encoder,
        curr_depth_buffer_srv: &scene_depth,
        prev_depth_buffer_srv: &scene_depth,
        curr_camera: Some(&camera),
        prev_camera: Some(&camera),
        camera_attribs_cb: None,
        pass_timestamps: None,
    });
    // The rays' direction scaled by their length, PDF 1, at the
    // reconstruction boundary.
    ssr.resources.as_mut().unwrap().ray_direction_pdf = texture(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba16Float,
        &half(&[
            mirror[0] * hit_distance,
            mirror[1] * hit_distance,
            mirror[2] * hit_distance,
            1.0,
        ]),
    );
    // Confident hits: DFX-29 reconstructs only tiles with one.
    ssr.resources.as_mut().unwrap().radiance = texture(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba16Float,
        &half(&[1.0, 1.0, 1.0, 1.0]),
    );
    let attribs = ScreenSpaceReflectionAttribs::default();
    let mut render = RenderAttributes {
        device: &device,
        queue: &queue,
        device_context: &mut encoder,
        post_fx_context: &mut context,
        color_buffer_srv: &normal,
        depth_buffer_srv: &scene_depth,
        normal_buffer_srv: &normal,
        material_buffer_srv: &material,
        motion_vectors_srv: &motion,
        ssr_attribs: &attribs,
        pass_timestamps: None,
        reset_accumulation: false,
        frame_time: 1.0 / 60.0,
    };
    ssr.update_constant_buffer(&render, false);
    ssr.compute_stencil_mask_and_extract_roughness(&mut render);
    ssr.compute_downsampled_stencil_mask(&mut render);
    ssr.classify_denoiser_tiles(&mut render);
    ssr.compute_spatial_reconstruction(&mut render);
    queue.submit([encoder.finish()]);
    let resolved = read_r16f(&device, &queue, &ssr.resources().resolved_depth)
        [(pixel[1] * SIZE[0] + pixel[0]) as usize];
    assert!(
        (resolved - expected).abs() <= 0.01 * expected,
        "resolved depth {resolved}, the virtual point's {expected}"
    );
}

/// The SSR textures one denoised frame leaves, row-major at full size.
struct Denoised {
    output: Vec<[f32; 4]>,
    radiance_history: Vec<[f32; 4]>,
    variance_history: Vec<f32>,
}

/// One frame of SSR's denoiser over rays supplied at the reconstruction
/// boundary: a ray at `(x, y)` of the ray grid where `hit` holds found a
/// white surface with full confidence; every other ray missed. `filter`
/// gives the reconstruction radius and the bilateral sigma. With
/// `all_tiles`, every denoiser tile is active, as before DFX-29. With
/// `turned`, the previous frame's camera faced the other way
/// (`turned_around`) and the surface moved two screens since, as a surface
/// behind the previous camera does.
#[allow(clippy::too_many_arguments)]
fn denoise_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    ssr: &mut ScreenSpaceReflection,
    context: &mut PostFXContext,
    flags: FeatureFlags,
    filter: [f32; 2],
    index: u32,
    all_tiles: bool,
    turned: bool,
    hit: impl Fn(u32, u32) -> bool,
) -> Denoised {
    use wgpu::util::DeviceExt;
    let mut encoder = device.create_command_encoder(&Default::default());
    // A glossy plane facing the camera (perceptual roughness 0.196, which
    // gives spatial reconstruction nearly its full radius), standing still.
    let scene_depth = depth(device, &mut encoder, 0.95);
    let normal = texture(
        device,
        queue,
        wgpu::TextureFormat::Rgba16Float,
        &half(&[0.0, 0.0, -1.0, 0.0]),
    );
    let material = texture(device, queue, wgpu::TextureFormat::R8Unorm, &[50]);
    // Motion in NDC: 4 is two screens.
    let motion = texture(
        device,
        queue,
        wgpu::TextureFormat::Rg16Float,
        &half(&[if turned { 4.0 } else { 0.0 }, 0.0]),
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
        post_fx_context::FeatureFlags::NONE,
    );
    ssr.prepare_resources(device, &mut encoder, context, flags);
    ssr.prepare_shaders_and_pso(device);
    let camera = camera(false, index);
    let previous = if turned {
        turned_around(camera)
    } else {
        camera
    };
    context.execute(&mut post_fx_context::RenderAttributes {
        device,
        queue,
        device_context: &mut encoder,
        curr_depth_buffer_srv: &scene_depth,
        prev_depth_buffer_srv: &scene_depth,
        curr_camera: Some(&camera),
        prev_camera: Some(&previous),
        camera_attribs_cb: None,
        pass_timestamps: None,
    });
    let [width, height] = if flags.contains(FeatureFlags::HALF_RESOLUTION) {
        [SIZE[0] / 2, SIZE[1] / 2]
    } else {
        SIZE
    };
    let rays = |texel: &dyn Fn(u32, u32) -> [f32; 4]| {
        let data: Vec<u8> = (0..height)
            .flat_map(|y| (0..width).flat_map(move |x| half(&texel(x, y))))
            .collect();
        device
            .create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &data,
            )
            .create_view(&Default::default())
    };
    let resources = ssr.resources.as_mut().unwrap();
    // Rays 1 m long back towards the camera, PDF 1, so every sample weighs.
    resources.ray_direction_pdf = rays(&|_, _| [0.0, 0.0, -1.0, 1.0]);
    resources.radiance = rays(&|x, y| {
        if hit(x, y) { [1.0; 4] } else { [0.0; 4] }
    });
    let attribs = ScreenSpaceReflectionAttribs {
        temporal_radiance_stability_factor: 0.95,
        spatial_reconstruction_radius: filter[0],
        bilateral_cleanup_spatial_sigma_factor: filter[1],
        ..Default::default()
    };
    let mut render = RenderAttributes {
        device,
        queue,
        device_context: &mut encoder,
        post_fx_context: context,
        color_buffer_srv: &normal,
        depth_buffer_srv: &scene_depth,
        normal_buffer_srv: &normal,
        material_buffer_srv: &material,
        motion_vectors_srv: &motion,
        ssr_attribs: &attribs,
        pass_timestamps: None,
        reset_accumulation: false,
        frame_time: 1.0 / 60.0,
    };
    ssr.update_constant_buffer(&render, false);
    ssr.compute_stencil_mask_and_extract_roughness(&mut render);
    ssr.compute_downsampled_stencil_mask(&mut render);
    if all_tiles {
        ssr.resources.as_mut().unwrap().denoiser_tiles =
            texture(device, queue, wgpu::TextureFormat::R8Unorm, &[255]);
    } else {
        ssr.classify_denoiser_tiles(&mut render);
    }
    ssr.compute_spatial_reconstruction(&mut render);
    ssr.compute_temporal_accumulation(&mut render);
    ssr.compute_bilateral_cleanup(&mut render);
    queue.submit([encoder.finish()]);
    let r = ssr.resources();
    let current = (index & 1) as usize;
    Denoised {
        output: read_rgba16f(device, queue, &r.output),
        radiance_history: read_rgba16f(device, queue, &r.radiance_history[current]),
        variance_history: read_r16f(device, queue, &r.variance_history[current]),
    }
}

/// The default reconstruction radius and bilateral sigma.
const DEFAULT_FILTER: [f32; 2] = [4.0, 0.9];
/// The widest reconstruction radius and a bilateral sigma of 2: hits reach
/// past the neighbouring tile.
const WIDE_FILTER: [f32; 2] = [8.0, 2.0];

/// The ray grid's columns that cover pixels `first` and `last`, the first
/// and last of the third denoiser tile: rays along them hit.
fn hit_columns(flags: FeatureFlags) -> impl Fn(u32, u32) -> bool {
    let scale = if flags.contains(FeatureFlags::HALF_RESOLUTION) {
        2
    } else {
        1
    };
    move |x, _| x == 16 / scale || x == 23 / scale
}

/// A checkerboard of hits and misses over the whole ray grid.
fn checkerboard(x: u32, y: u32) -> bool {
    (x + y).is_multiple_of(2)
}

/// The pixel indices of the denoiser tiles three or more from the hit
/// columns' tile, beyond the reach of these filters.
fn far_from_hit_columns() -> impl Iterator<Item = usize> {
    (0..(SIZE[0] * SIZE[1]) as usize).filter(|pixel| pixel % SIZE[0] as usize / 8 >= 5)
}

// PROVENANCE.md DFX-29: skipping the tiles where every ray in reach missed
// changes no radiance, for any reconstruction radius and bilateral sigma.
// Two frames of a checkerboard of hits fill the histories with uneven
// radiance, so the bilateral filter and the history clamp work at the tiles'
// edges; then only the first and last ray columns of one tile hit, reaching
// into the tiles on both sides. Denoising every tile is the reference. The
// wide filter reaches two tiles, which a fixed one-tile margin would miss.
#[test]
fn skipping_tiles_without_hits_changes_no_radiance() {
    let Some((device, queue)) = device() else {
        return;
    };
    for flags in [FeatureFlags::NONE, FeatureFlags::HALF_RESOLUTION] {
        for filter in [DEFAULT_FILTER, WIDE_FILTER] {
            let run = |all_tiles| {
                let mut ssr = ScreenSpaceReflection::new(&device);
                // No fade-in, so the first frame's output is the denoised radiance.
                let mut context = PostFXContext::new(
                    &device,
                    &queue,
                    post_fx_context::CreateInfo {
                        transition_duration: 0.0,
                    },
                );
                let columns = hit_columns(flags);
                (0..3)
                    .map(|index| {
                        denoise_frame(
                            &device,
                            &queue,
                            &mut ssr,
                            &mut context,
                            flags,
                            filter,
                            index,
                            all_tiles,
                            false,
                            |x, y| {
                                if index < 2 {
                                    checkerboard(x, y)
                                } else {
                                    columns(x, y)
                                }
                            },
                        )
                    })
                    .last()
                    .unwrap()
            };
            let (skipped, reference) = (run(false), run(true));
            assert_eq!(
                skipped.output, reference.output,
                "{flags:?} {filter:?}: skipping tiles changed the denoised radiance"
            );
            assert_eq!(
                skipped.radiance_history, reference.radiance_history,
                "{flags:?} {filter:?}: skipping tiles changed the radiance history"
            );
            // The hits reach the neighbouring tiles, and with the wide filter
            // the tiles beyond them, so the checks above bite.
            let reach = if filter == WIDE_FILTER { 2 } else { 1 };
            for tile in [2 - reach, 2 + reach] {
                assert!(
                    (0..(SIZE[0] * SIZE[1]) as usize)
                        .filter(|pixel| pixel % SIZE[0] as usize / 8 == tile)
                        .any(|pixel| reference.output[pixel][3] > 0.0),
                    "{flags:?} {filter:?}: the hits must reach tile {tile} for this check to bite"
                );
            }
            for pixel in far_from_hit_columns() {
                assert_eq!(
                    skipped.variance_history[pixel], 1.0,
                    "{flags:?} {filter:?}: pixel {pixel} of a tile without hits was denoised"
                );
            }
        }
    }
}

// PROVENANCE.md DFX-29: a tile whose rays all start to miss keeps no older
// reflection. Two frames of hits everywhere fill both radiance histories;
// then only one tile's edge columns hit, and the histories of the tiles out
// of their reach hold zero radiance and variance 1 after each frame.
#[test]
fn tiles_that_stop_hitting_keep_no_history() {
    let Some((device, queue)) = device() else {
        return;
    };
    for flags in [FeatureFlags::NONE, FeatureFlags::HALF_RESOLUTION] {
        let mut ssr = ScreenSpaceReflection::new(&device);
        // No fade-in, so the first frame's output is the denoised radiance.
        let mut context = PostFXContext::new(
            &device,
            &queue,
            post_fx_context::CreateInfo {
                transition_duration: 0.0,
            },
        );
        let columns = hit_columns(flags);
        for index in 0..4 {
            let frame = denoise_frame(
                &device,
                &queue,
                &mut ssr,
                &mut context,
                flags,
                DEFAULT_FILTER,
                index,
                false,
                false,
                |x, y| index < 2 || columns(x, y),
            );
            for pixel in far_from_hit_columns() {
                if index < 2 {
                    assert!(
                        frame.radiance_history[pixel][3] > 0.5,
                        "{flags:?} frame {index}: pixel {pixel} must accumulate the hits"
                    );
                } else {
                    assert_eq!(
                        (
                            frame.radiance_history[pixel],
                            frame.variance_history[pixel],
                            frame.output[pixel]
                        ),
                        ([0.0; 4], 1.0, [0.0; 4]),
                        "{flags:?} frame {index}: pixel {pixel} kept an older reflection"
                    );
                }
            }
        }
    }
}

// PROVENANCE.md DFX-31: a reflection's virtual point on or behind the
// previous camera's plane was nowhere on its screen. One frame of hits
// everywhere fills the histories with white; then the camera is found to
// have faced the other way the frame before, so every virtual point, a
// metre beyond the plane ahead of it, lies behind the previous camera,
// where dividing by its negative clip w would mirror it onto the screen at
// the plane's depth. The surface moved two screens, as a surface behind the
// previous camera does. With neither reprojection valid, every pixel keeps
// no history: its variance is 1.
#[test]
fn reflection_hits_behind_the_previous_camera_take_no_history() {
    let Some((device, queue)) = device() else {
        return;
    };
    let mut ssr = ScreenSpaceReflection::new(&device);
    let mut context = PostFXContext::new(&device, &queue, Default::default());
    for index in 0..2 {
        let frame = denoise_frame(
            &device,
            &queue,
            &mut ssr,
            &mut context,
            FeatureFlags::NONE,
            DEFAULT_FILTER,
            index,
            false,
            index == 1,
            |x, y| index == 0 || checkerboard(x, y),
        );
        if index == 1 {
            for (pixel, variance) in frame.variance_history.iter().enumerate() {
                assert_eq!(
                    *variance, 1.0,
                    "pixel {pixel} took history from behind the previous camera"
                );
            }
        }
    }
}
