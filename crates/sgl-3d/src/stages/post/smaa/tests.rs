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
        // Hardware rasterization supplies input and reference; the reference is
        // rendered at 16x each dimension, then averaged without using SMAA logic.
        let shader=device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("independent diagonal geometry"), source: wgpu::ShaderSource::Wgsl(r#"
                @vertex fn fullscreen(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> {
                    let p=array<vec2<f32>,3>(vec2<f32>(-1.0,-1.0),vec2<f32>(-1.0,1.0),vec2<f32>(0.43,1.0));
                    return vec4<f32>(p[i],0.0,1.0);
                }
                @fragment fn white()->@location(0) vec4<f32> { return vec4<f32>(1.0); }
            "#.into()),
        });
        let raster = pipeline(
            &device,
            &shader,
            "white",
            "fullscreen",
            wgpu::TextureFormat::Rgba8Unorm,
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
        let mut smaa = Smaa::new(&device, &queue, 1, 1, wgpu::TextureFormat::Rgba8Unorm).unwrap();
        let capture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".cache/smaa-qa");
        std::fs::create_dir_all(&capture).unwrap();
        for size in [[128, 96], [192, 128]] {
            smaa.resize(&device, size[0], size[1]);
            let input = image_target(&device, size);
            let output = image_target(&device, size);
            let reference = image_target(&device, [size[0] * 16, size[1] * 16]);
            render(&input);
            render(&reference);
            let mut encoder = device.create_command_encoder(&Default::default());
            smaa.encode(
                &device,
                &mut encoder,
                &input.create_view(&Default::default()),
                &output.create_view(&Default::default()),
                None,
            );
            queue.submit([encoder.finish()]);
            let raw = read(&device, &queue, &input);
            let aa = read(&device, &queue, &output);
            let high = read(&device, &queue, &reference);
            let mut expected = vec![0u8; raw.len()];
            let (mut raw_error, mut aa_error) = (0.0f64, 0.0f64);
            for y in 0..size[1] as usize {
                for x in 0..size[0] as usize {
                    let mut total = 0u32;
                    for sy in 0..16 {
                        for sx in 0..16 {
                            total += u32::from(
                                high[((y * 16 + sy) * size[0] as usize * 16 + x * 16 + sx) * 4],
                            );
                        }
                    }
                    let value = f64::from(total) / 256.0;
                    let i = (y * size[0] as usize + x) * 4;
                    expected[i..i + 3].fill(value.round() as u8);
                    expected[i + 3] = 255;
                    // Exclude the viewport boundary, which has no outside image.
                    if x > 1 && y > 1 && x + 2 < size[0] as usize && y + 2 < size[1] as usize {
                        raw_error += (f64::from(raw[i]) - value).abs();
                        aa_error += (f64::from(aa[i]) - value).abs();
                    }
                }
            }
            eprintln!("{size:?}: raw absolute coverage error={raw_error:.2}; SMAA={aa_error:.2}");
            for (name, pixels) in [("raw", &raw), ("smaa", &aa), ("reference", &expected)] {
                image::save_buffer(
                    capture.join(format!("{}-{name}.png", size[0])),
                    pixels,
                    size[0],
                    size[1],
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            assert!(
                aa_error < raw_error * 0.9,
                "SMAA must improve diagonal coverage against independently rasterized reference"
            );
        }
    });
}
