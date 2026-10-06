//! Regression for #313: real mask passes must use this frame's materials.
//! Read masks rather than duplicating extraction or classification shader math.
use super::*;
use crate::post_fx_context::FrameDesc;
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

fn read_mask(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) -> Vec<bool> {
    let texture = view.texture();
    let stride = 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(stride * texture.height()),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            aspect: wgpu::TextureAspect::DepthOnly,
            ..texture.as_image_copy()
        },
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
    buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let bytes = buffer.get_mapped_range(..).unwrap();
    (0..texture.height())
        .flat_map(|y| (0..texture.width()).map(move |x| (y * stride + x * 2) as usize))
        .map(|at| u16::from_le_bytes([bytes[at], bytes[at + 1]]) != 0)
        .collect()
}

#[test]
fn current_materials_control_full_and_half_resolution_masks() {
    let Some((device, queue)) = device() else {
        return;
    };
    // A background pixel beside a foreground pixel in every fourth block,
    // plus a background-only strip. Depth is fixed while materials move.
    let mut encoder = device.create_command_encoder(&Default::default());
    let scene_depth = depth(&device, &mut encoder, 1.0);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(
            "

        @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
            let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
            return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
        }
        @fragment fn fs(@builtin(position) p: vec4<f32>) -> @builtin(frag_depth) f32 {
            return select(0.5, 1.0, u32(p.x) % 8u == 7u || p.y >= 32.0);
        }"
            .into(),
        ),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
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
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[],
        }),
        multiview_mask: None,
        cache: None,
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &scene_depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
    }
    queue.submit([encoder.finish()]);
    let mut failures = Vec::new();
    for flags in [FeatureFlags::NONE, FeatureFlags::HALF_RESOLUTION] {
        let mut ssr = ScreenSpaceReflection::new(&device);
        let mut context = PostFXContext::new(&device, &queue, Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        context.prepare_resources(
            &device,
            &FrameDesc {
                index: 0,
                width: 64,
                height: 48,
                output_width: 64,
                output_height: 48,
            },
            post_fx_context::FeatureFlags::NONE,
        );
        ssr.prepare_resources(&device, &mut encoder, &mut context, flags);
        ssr.prepare_shaders_and_pso(&device);
        queue.submit([encoder.finish()]);
        // Independent scene expectations: rough blocks reject; mixed blocks
        // reject at half resolution; background never becomes a full-res ray.
        for (name, pattern, full_expected, half_expected, full_row, half_row) in [
            ("initially rough", [230; 8], 0, 0, [0; 8], [0; 4]),
            (
                "glossy warmup",
                [25; 8],
                1792,
                512,
                [1, 1, 1, 1, 1, 1, 1, 0],
                [1; 4],
            ),
            (
                "mixed after glossy",
                [25, 25, 230, 230, 25, 230, 25, 230],
                1024,
                128,
                [1, 1, 0, 0, 1, 0, 1, 0],
                [1, 0, 0, 0],
            ),
            (
                "boundary moved",
                [230, 230, 25, 25, 230, 25, 25, 230],
                1024,
                128,
                [0, 0, 1, 1, 0, 1, 1, 0],
                [0, 1, 0, 0],
            ),
            ("rough after motion", [230; 8], 0, 0, [0; 8], [0; 4]),
        ] {
            let data: Vec<u8> = (0..48)
                .flat_map(|_| (0..64).map(|x| pattern[x % 8]))
                .collect();
            let material = texels(&device, &queue, wgpu::TextureFormat::R8Unorm, &data);
            let mut encoder = device.create_command_encoder(&Default::default());
            let attribs = ScreenSpaceReflectionAttribs::default();
            let mut render = RenderAttributes {
                device: &device,
                queue: &queue,
                device_context: &mut encoder,
                post_fx_context: &mut context,
                color_buffer_srv: &material,
                depth_buffer_srv: &scene_depth,
                normal_buffer_srv: &material,
                material_buffer_srv: &material,
                motion_vectors_srv: &material,
                ssr_attribs: &attribs,
                pass_timestamps: None,
                reset_accumulation: false,
                frame_time: 1.0 / 60.0,
            };
            ssr.update_constant_buffer(&render, false);
            ssr.compute_stencil_mask_and_extract_roughness(&mut render);
            ssr.compute_downsampled_stencil_mask(&mut render);
            queue.submit([encoder.finish()]);
            let full = read_mask(&device, &queue, &ssr.resources().depth_stencil_mask);
            let full_count = full.iter().filter(|&&v| v).count();
            println!("{flags:?} {name}: full active {full_count}, expected {full_expected}");
            if full
                .iter()
                .enumerate()
                .any(|(i, &active)| active != (i / 64 < 32 && full_row[i % 8] != 0))
            {
                failures.push(format!("{name}: full mask differs from scene eligibility"));
            }
            if full_count != full_expected {
                failures.push(format!("{name}: full {full_count} != {full_expected}"));
            }
            if let Some(mask) = &ssr.resources().depth_stencil_mask_half_res {
                let half = read_mask(&device, &queue, mask);
                let half_count = half.iter().filter(|&&v| v).count();
                println!("{flags:?} {name}: half active {half_count}, expected {half_expected}");
                if half
                    .iter()
                    .enumerate()
                    .any(|(i, &active)| active != (i / 32 < 16 && half_row[i % 4] != 0))
                {
                    failures.push(format!("{name}: half mask differs from scene eligibility"));
                }
                if half_count != half_expected {
                    failures.push(format!("{name}: half {half_count} != {half_expected}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
