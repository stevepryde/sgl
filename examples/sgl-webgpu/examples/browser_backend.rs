//! The browser lane's page for sgl-webgpu (testing.md 5): the parts of the
//! backend SGL's own pages do not reach, each run on the browser's WebGPU
//! through `sgl_webgpu::instance` and checked against what the GPU or the
//! browser must produce: bytes read back from buffers the GPU copied or
//! wrote, texels, query results, a resolved multisample edge, a canvas's
//! pixels, and errors the browser reports. Built for wasm32 as a `cdylib`,
//! bound with `wasm-bindgen --target web` and run by `browser/webgpu.test.ts`
//! via `bun scripts/tasks.ts check-browser`.
//!
//! Each check returns `Err(detail)` instead of panicking so the whole report
//! reaches the page, one `ok <name>` or `FAIL <name>` line per check.
#![cfg(target_arch = "wasm32")]
#![allow(clippy::future_not_send)]

use std::fmt::Write as _;
use std::num::NonZeroU64;
use std::ops::Range;
use std::sync::Arc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wgpu::util::DeviceExt as _;

macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err(format!($($arg)+));
        }
    };
}

/// Every render check draws into a 4x4 target, so each pixel's coverage is
/// known from the triangles below.
const SIZE: u32 = 4;
/// Copies to buffers pad each row to WebGPU's 256-byte alignment.
const ROW: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];

/// Vertices 0..3 cover the whole target (red), 3..6 its right half x >= 0
/// (green), 6..9 the half below the anti-diagonal x + y <= 0 (white).
const DRAW_SHADER: &str = "
struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs(@builtin(vertex_index) index: u32) -> Out {
    var positions = array<vec2<f32>, 9>(
        vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0),
        vec2(0.0, -1.0), vec2(2.0, -1.0), vec2(0.0, 3.0),
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0),
    );
    var colors = array<vec4<f32>, 3>(
        vec4(1.0, 0.0, 0.0, 1.0), vec4(0.0, 1.0, 0.0, 1.0), vec4(1.0, 1.0, 1.0, 1.0),
    );
    return Out(vec4(positions[index], 0.0, 1.0), colors[index / 3u]);
}

@fragment
fn fs(input: Out) -> @location(0) vec4<f32> {
    return input.color;
}
";

struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

#[wasm_bindgen]
pub async fn run() -> String {
    console_error_panic_hook::set_once();
    let mut report = String::new();
    let gpu = match setup().await {
        Ok(gpu) => gpu,
        Err(error) => {
            let _ = writeln!(report, "FAIL setup: {error}");
            return report;
        }
    };
    record(
        &mut report,
        "buffer map round trip",
        buffer_map_round_trip(&gpu).await,
    );
    record(
        &mut report,
        "write and copy texels",
        write_and_copy_texels(&gpu).await,
    );
    record(
        &mut report,
        "compute storage writes",
        compute_storage_writes(&gpu).await,
    );
    record(
        &mut report,
        "render bundle and indirect draw",
        bundle_and_indirect(&gpu).await,
    );
    record(
        &mut report,
        "occlusion queries",
        occlusion_queries(&gpu).await,
    );
    record(
        &mut report,
        "multisample resolve",
        multisample_resolve(&gpu).await,
    );
    record(
        &mut report,
        "external image copy",
        external_image_copy(&gpu).await,
    );
    record(
        &mut report,
        "external image colour space",
        external_image_color_space(&gpu).await,
    );
    record(&mut report, "depth slice", depth_slice(&gpu).await);
    record(
        &mut report,
        "texture from a WebGPU handle",
        texture_from_handle(&gpu).await,
    );
    record(&mut report, "surface error", surface_error(&gpu));
    record(&mut report, "error scopes", error_scopes(&gpu).await);
    record(
        &mut report,
        "compilation info",
        compilation_info(&gpu).await,
    );
    record(&mut report, "surface present", surface_present(&gpu).await);
    record(
        &mut report,
        "texture format tiers",
        texture_format_tiers(&gpu).await,
    );
    record(
        &mut report,
        "asynchronous render pipeline",
        async_render_pipeline(&gpu).await,
    );
    record(
        &mut report,
        "asynchronous compute pipeline",
        async_compute_pipeline(&gpu).await,
    );
    record(
        &mut report,
        "asynchronous pipeline error",
        async_pipeline_error(&gpu).await,
    );
    report
}

fn record(report: &mut String, name: &str, result: Result<String, String>) {
    let _ = match result {
        Ok(detail) => writeln!(report, "ok {name}: {detail}"),
        Err(detail) => writeln!(report, "FAIL {name}: {detail}"),
    };
}

async fn setup() -> Result<Gpu, String> {
    let instance = sgl_webgpu::instance(&wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
        .map_err(|error| format!("no WebGPU adapter: {error}"))?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .map_err(|error| format!("no device: {error}"))?;
    // An error no check expects reaches the console, where the lane fails it.
    device.on_uncaptured_error(Arc::new(|error| {
        web_sys::console::error_1(&format!("uncaptured WebGPU error: {error}").into());
    }));
    Ok(Gpu {
        instance,
        adapter,
        device,
        queue,
    })
}

/// Maps `range` of `buffer` for reading and copies it out.
async fn read(buffer: &wgpu::Buffer, range: Range<u64>) -> Result<Vec<u8>, String> {
    let (sender, receiver) = futures_channel::oneshot::channel();
    buffer.map_async(wgpu::MapMode::Read, range.clone(), move |result| {
        let _ = sender.send(result);
    });
    receiver
        .await
        .map_err(|_| "the map callback never ran".to_string())?
        .map_err(|error| format!("map_async: {error}"))?;
    let bytes = buffer
        .get_mapped_range(range)
        .map_err(|error| error.to_string())?
        .to_vec();
    buffer.unmap();
    Ok(bytes)
}

fn readback_buffer(gpu: &Gpu, size: u64) -> wgpu::Buffer {
    gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn target(
    gpu: &Gpu,
    format: wgpu::TextureFormat,
    samples: u32,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: samples,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

/// Copies a 4x4 four-byte texture to a buffer at `offset` and returns its
/// texels, row by row from the top.
async fn texels(gpu: &Gpu, texture: &wgpu::Texture, offset: u32) -> Result<Vec<[u8; 4]>, String> {
    let size = u64::from(offset + ROW * SIZE);
    let buffer = readback_buffer(gpu, size);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: offset.into(),
                bytes_per_row: Some(ROW),
                rows_per_image: Some(SIZE),
            },
        },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    let bytes = read(&buffer, 0..size).await?;
    ensure!(
        bytes[..offset as usize].iter().all(|&byte| byte == 0),
        "the copy wrote before its buffer offset {offset}"
    );
    let rows = &bytes[offset as usize..];
    Ok((0..SIZE as usize)
        .flat_map(|y| {
            (0..SIZE as usize).map(move |x| {
                let at = y * ROW as usize + x * 4;
                [rows[at], rows[at + 1], rows[at + 2], rows[at + 3]]
            })
        })
        .collect())
}

fn draw_pipeline(gpu: &Gpu, samples: u32) -> wgpu::RenderPipeline {
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("draw"),
            source: wgpu::ShaderSource::Wgsl(DRAW_SHADER.into()),
        });
    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("draw"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
            }),
            multiview_mask: None,
            cache: None,
        })
}

fn color_attachment<'a>(
    view: &'a wgpu::TextureView,
    resolve_target: Option<&'a wgpu::TextureView>,
) -> wgpu::RenderPassColorAttachment<'a> {
    wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            store: wgpu::StoreOp::Store,
        },
    }
}

/// Bytes written through a write mapping reach a second buffer by a GPU
/// copy and read back through a read mapping of part of it.
async fn buffer_map_round_trip(gpu: &Gpu) -> Result<String, String> {
    const LEN: u64 = 256;
    let written: Vec<u8> = (0..=u8::MAX)
        .map(|i| i.wrapping_mul(7).wrapping_add(3))
        .collect();
    let upload = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("upload"),
        size: LEN,
        usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let (sender, receiver) = futures_channel::oneshot::channel();
    upload.map_async(wgpu::MapMode::Write, .., move |result| {
        let _ = sender.send(result);
    });
    receiver
        .await
        .map_err(|_| "the write map callback never ran".to_string())?
        .map_err(|error| format!("map_async(Write): {error}"))?;
    upload
        .get_mapped_range_mut(..)
        .map_err(|error| error.to_string())?
        .copy_from_slice(&written);
    upload.unmap();

    let download = readback_buffer(gpu, LEN);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_buffer_to_buffer(&upload, 0, &download, 0, LEN);
    gpu.queue.submit([encoder.finish()]);

    // Map 64..256 and view 128..192 inside it: the view's bytes are the
    // buffer's 128..192, not the mapping's first 64.
    let (sender, receiver) = futures_channel::oneshot::channel();
    download.map_async(wgpu::MapMode::Read, 64..LEN, move |result| {
        let _ = sender.send(result);
    });
    receiver
        .await
        .map_err(|_| "the read map callback never ran".to_string())?
        .map_err(|error| format!("map_async(Read): {error}"))?;
    let read_back = download
        .get_mapped_range(128..192)
        .map_err(|error| error.to_string())?
        .to_vec();
    download.unmap();
    ensure!(
        read_back == written[128..192],
        "bytes 128..192 read {read_back:?}, wrote {:?}",
        &written[128..192]
    );
    Ok(format!(
        "{LEN} bytes written, copied, and 128..192 read back"
    ))
}

/// Texels written into a sub-rectangle land where it says, and a copy to a
/// buffer at an offset reads them back.
async fn write_and_copy_texels(gpu: &Gpu) -> Result<String, String> {
    let texture = target(
        gpu,
        wgpu::TextureFormat::Rgba8Uint,
        1,
        wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
    );
    // Eight bytes the layout's offset skips, then 2x2 texels.
    let block: [[u8; 4]; 4] = [
        [10, 20, 30, 40],
        [11, 21, 31, 41],
        [12, 22, 32, 42],
        [13, 23, 33, 43],
    ];
    let mut data = vec![0xEE; 8];
    data.extend(block.iter().flatten());
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x: 1, y: 1, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 8,
            bytes_per_row: Some(8),
            rows_per_image: Some(2),
        },
        wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
    );
    let texels = texels(gpu, &texture, 256).await?;
    for y in 0..SIZE as usize {
        for x in 0..SIZE as usize {
            let expected = if (1..3).contains(&x) && (1..3).contains(&y) {
                block[(y - 1) * 2 + (x - 1)]
            } else {
                [0; 4]
            };
            let got = texels[y * SIZE as usize + x];
            ensure!(
                got == expected,
                "texel ({x}, {y}) is {got:?}, expected {expected:?}"
            );
        }
    }
    Ok("2x2 texels at (1, 1) of 4x4, the rest zero".into())
}

/// A compute pass writes a storage buffer through a dynamic offset, with a
/// pipeline-overridable constant set at pipeline creation.
async fn compute_storage_writes(gpu: &Gpu) -> Result<String, String> {
    const COUNT: u32 = 64;
    const OFFSET: u32 = 256;
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compute"),
            source: wgpu::ShaderSource::Wgsl(
                "
override scale: u32 = 1u;
@group(0) @binding(0) var<storage, read_write> values: array<u32, 64>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    values[id.x] = id.x * id.x * scale + 1u;
}
"
                .into(),
            ),
        });
    let layout = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(u64::from(COUNT * 4)),
                },
                count: None,
            }],
        });
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("compute"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
    let pipeline = gpu
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("compute"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &[("scale", 3.0)],
                ..Default::default()
            },
            cache: None,
        });
    let size = u64::from(OFFSET + COUNT * 4);
    let storage = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("values"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("compute"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &storage,
                offset: 0,
                size: NonZeroU64::new(u64::from(COUNT * 4)),
            }),
        }],
    });
    let readback = readback_buffer(gpu, size);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[OFFSET]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&storage, 0, &readback, 0, size);
    gpu.queue.submit([encoder.finish()]);
    let bytes = read(&readback, 0..size).await?;
    ensure!(
        bytes[..OFFSET as usize].iter().all(|&byte| byte == 0),
        "the pass wrote before its dynamic offset {OFFSET}"
    );
    for i in 0..COUNT {
        let at = (OFFSET + i * 4) as usize;
        let got = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        let expected = i * i * 3 + 1;
        ensure!(got == expected, "value {i} is {got}, expected {expected}");
    }
    Ok(format!(
        "{COUNT} values at dynamic offset {OFFSET}, scale constant 3"
    ))
}

/// A render bundle's indexed draw fills the target red; an indirect draw
/// whose arguments sit at offset 16 then fills its right half green.
async fn bundle_and_indirect(gpu: &Gpu) -> Result<String, String> {
    let pipeline = draw_pipeline(gpu, 1);
    let texture = target(
        gpu,
        wgpu::TextureFormat::Rgba8Unorm,
        1,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let indices = gpu
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("indices"),
            contents: &[0u8, 0, 1, 0, 2, 0, 0, 0],
            usage: wgpu::BufferUsages::INDEX,
        });
    // A zero draw at offset 0, then vertex_count 3, instance_count 1,
    // first_vertex 3 (the right half), first_instance 0.
    let arguments: Vec<u8> = [0u32, 0, 0, 0, 3, 1, 3, 0]
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect();
    let indirect = gpu
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("indirect"),
            contents: &arguments,
            usage: wgpu::BufferUsages::INDIRECT,
        });
    let mut bundle_encoder =
        gpu.device
            .create_render_bundle_encoder(&wgpu::RenderBundleEncoderDescriptor {
                label: Some("bundle"),
                color_formats: &[Some(wgpu::TextureFormat::Rgba8Unorm)],
                depth_stencil: None,
                sample_count: 1,
                multiview: None,
            });
    bundle_encoder.set_pipeline(&pipeline);
    bundle_encoder.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint16);
    bundle_encoder.draw_indexed(0..3, 0, 0..1);
    let bundle = bundle_encoder.finish(&wgpu::RenderBundleDescriptor {
        label: Some("bundle"),
    });

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bundle and indirect"),
            color_attachments: &[Some(color_attachment(&view, None))],
            ..Default::default()
        });
        pass.execute_bundles([&bundle]);
        pass.set_pipeline(&pipeline);
        pass.draw_indirect(&indirect, 16);
    }
    gpu.queue.submit([encoder.finish()]);
    let texels = texels(gpu, &texture, 0).await?;
    for (index, &got) in texels.iter().enumerate() {
        let x = index % SIZE as usize;
        let expected = if x < 2 { RED } else { GREEN };
        ensure!(
            got == expected,
            "pixel ({x}, {}) is {got:?}, expected {expected:?}",
            index / SIZE as usize
        );
    }
    Ok("left half red from the bundle, right half green from the indirect draw".into())
}

/// Query 0 brackets a draw the scissor removes entirely, query 1 one that
/// covers the target: the first passes no sample, the second some.
async fn occlusion_queries(gpu: &Gpu) -> Result<String, String> {
    let pipeline = draw_pipeline(gpu, 1);
    let texture = target(
        gpu,
        wgpu::TextureFormat::Rgba8Unorm,
        1,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let queries = gpu.device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("occlusion"),
        ty: wgpu::QueryType::Occlusion,
        count: 2,
    });
    let resolved = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("resolved queries"),
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = readback_buffer(gpu, 16);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("occlusion"),
            color_attachments: &[Some(color_attachment(&view, None))],
            occlusion_query_set: Some(&queries),
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        pass.set_scissor_rect(0, 0, 1, 1);
        pass.begin_occlusion_query(0);
        pass.draw(3..6, 0..1);
        pass.end_occlusion_query();
        pass.set_scissor_rect(0, 0, SIZE, SIZE);
        pass.begin_occlusion_query(1);
        pass.draw(0..3, 0..1);
        pass.end_occlusion_query();
    }
    encoder.resolve_query_set(&queries, 0..2, &resolved, 0);
    encoder.copy_buffer_to_buffer(&resolved, 0, &readback, 0, 16);
    gpu.queue.submit([encoder.finish()]);
    let bytes = read(&readback, 0..16).await?;
    let hidden = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    let visible = u64::from_le_bytes(bytes[8..].try_into().unwrap());
    ensure!(
        hidden == 0,
        "the scissored-out draw passed {hidden} samples"
    );
    ensure!(visible != 0, "the covering draw passed no sample");
    Ok(format!(
        "scissored-out draw 0 samples, covering draw {visible}"
    ))
}

/// A 4x multisampled triangle edge through the diagonal pixels' centres
/// resolves them to a grey strictly between the covered and uncovered sides.
async fn multisample_resolve(gpu: &Gpu) -> Result<String, String> {
    let pipeline = draw_pipeline(gpu, 4);
    let multisampled = target(
        gpu,
        wgpu::TextureFormat::Rgba8Unorm,
        4,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let resolved = target(
        gpu,
        wgpu::TextureFormat::Rgba8Unorm,
        1,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let samples = multisampled.create_view(&wgpu::TextureViewDescriptor::default());
    let resolve_view = resolved.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("multisample"),
            color_attachments: &[Some(color_attachment(&samples, Some(&resolve_view)))],
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        pass.draw(6..9, 0..1);
    }
    gpu.queue.submit([encoder.finish()]);
    let texels = texels(gpu, &resolved, 0).await?;
    // Pixel (x, y) from the top-left has its centre at x + y = (x - y) / 2
    // in clip space: below the edge it is covered, above it uncovered.
    for (index, &got) in texels.iter().enumerate() {
        let (x, y) = (index % SIZE as usize, index / SIZE as usize);
        let fits = match x.cmp(&y) {
            std::cmp::Ordering::Less => got == WHITE,
            std::cmp::Ordering::Greater => got == BLACK,
            std::cmp::Ordering::Equal => {
                (1..=254).contains(&got[0]) && got[0] == got[1] && got[1] == got[2] && got[3] == 255
            }
        };
        ensure!(fits, "pixel ({x}, {y}) resolved to {got:?}");
    }
    Ok(format!("edge pixels resolved to grey {}", texels[0][0]))
}

/// A 2D canvas's two colour bands copy into a texture upside down with
/// `flip_y`.
async fn external_image_copy(gpu: &Gpu) -> Result<String, String> {
    const TOP: [u8; 4] = [200, 40, 120, 255];
    const BOTTOM: [u8; 4] = [30, 160, 90, 255];
    let canvas = web_sys::OffscreenCanvas::new(SIZE, SIZE).map_err(|error| format!("{error:?}"))?;
    let context: web_sys::OffscreenCanvasRenderingContext2d = canvas
        .get_context("2d")
        .map_err(|error| format!("{error:?}"))?
        .ok_or("no 2d context")?
        .dyn_into()
        .map_err(|_| "not a 2d context")?;
    context.set_fill_style_str("rgb(200, 40, 120)");
    context.fill_rect(0.0, 0.0, f64::from(SIZE), f64::from(SIZE / 2));
    context.set_fill_style_str("rgb(30, 160, 90)");
    context.fill_rect(
        0.0,
        f64::from(SIZE / 2),
        f64::from(SIZE),
        f64::from(SIZE / 2),
    );
    let texture = target(
        gpu,
        wgpu::TextureFormat::Rgba8Unorm,
        1,
        wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC,
    );
    gpu.queue.copy_external_image_to_texture(
        &wgpu::CopyExternalImageSourceInfo {
            source: wgpu::ExternalImageSource::OffscreenCanvas(canvas),
            origin: wgpu::Origin2d::ZERO,
            flip_y: true,
        },
        wgpu::CopyExternalImageDestInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
            color_space: wgpu::PredefinedColorSpace::Srgb,
            premultiplied_alpha: false,
        },
        texture.size(),
    );
    let texels = texels(gpu, &texture, 0).await?;
    for (index, &got) in texels.iter().enumerate() {
        let y = index / SIZE as usize;
        let expected = if y < SIZE as usize / 2 { BOTTOM } else { TOP };
        ensure!(
            near(got, expected),
            "texel ({}, {y}) is {got:?}, expected {expected:?}",
            index % SIZE as usize
        );
    }
    Ok("the canvas's bottom band on top, its top band below".into())
}

/// Within one 8-bit step: canvas and texture are both sRGB, but a browser
/// may round its colour pipeline differently.
fn near(got: [u8; 4], expected: [u8; 4]) -> bool {
    got.iter().zip(expected).all(|(&a, b)| a.abs_diff(b) <= 1)
}

/// An sRGB canvas's pure red copied with `color_space: DisplayP3` arrives
/// as Display P3's encoding of that red, (0.9175, 0.2003, 0.1386) by CSS
/// Color 4's conversion, where an sRGB copy keeps (1, 0, 0).
async fn external_image_color_space(gpu: &Gpu) -> Result<String, String> {
    const P3_RED: [u8; 4] = [234, 51, 35, 255];
    let canvas = web_sys::OffscreenCanvas::new(SIZE, SIZE).map_err(|error| format!("{error:?}"))?;
    let context: web_sys::OffscreenCanvasRenderingContext2d = canvas
        .get_context("2d")
        .map_err(|error| format!("{error:?}"))?
        .ok_or("no 2d context")?
        .dyn_into()
        .map_err(|_| "not a 2d context")?;
    context.set_fill_style_str("rgb(255, 0, 0)");
    context.fill_rect(0.0, 0.0, f64::from(SIZE), f64::from(SIZE));
    let texture = target(
        gpu,
        wgpu::TextureFormat::Rgba8Unorm,
        1,
        wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC,
    );
    gpu.queue.copy_external_image_to_texture(
        &wgpu::CopyExternalImageSourceInfo {
            source: wgpu::ExternalImageSource::OffscreenCanvas(canvas),
            origin: wgpu::Origin2d::ZERO,
            flip_y: false,
        },
        wgpu::CopyExternalImageDestInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
            color_space: wgpu::PredefinedColorSpace::DisplayP3,
            premultiplied_alpha: false,
        },
        texture.size(),
    );
    let texels = texels(gpu, &texture, 0).await?;
    for (index, &got) in texels.iter().enumerate() {
        ensure!(
            got.iter().zip(P3_RED).all(|(&a, b)| a.abs_diff(b) <= 2),
            "texel {index} is {got:?}, expected Display P3 red {P3_RED:?}"
        );
    }
    Ok(format!("sRGB red arrived as Display P3 {:?}", texels[0]))
}

/// A pass clears slice 1 of a two-slice 3D target through `depth_slice`:
/// slice 1 holds the clear colour, slice 0 keeps the zeroes WebGPU
/// initialises a texture with.
async fn depth_slice(gpu: &Gpu) -> Result<String, String> {
    const MAGENTA: [u8; 4] = [255, 0, 255, 255];
    const SLICES: u32 = 2;
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("3d target"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: SLICES,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let size = u64::from(ROW * SIZE * SLICES);
    let buffer = readback_buffer(gpu, size);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("depth slice"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: Some(1),
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: 1.0,
                    g: 0.0,
                    b: 1.0,
                    a: 1.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ROW),
                rows_per_image: Some(SIZE),
            },
        },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    let bytes = read(&buffer, 0..size).await?;
    for z in 0..SLICES {
        let expected = if z == 1 { MAGENTA } else { [0; 4] };
        for y in 0..SIZE {
            for x in 0..SIZE {
                let at = ((z * SIZE + y) * ROW + x * 4) as usize;
                let got = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
                ensure!(
                    got == expected,
                    "slice {z} texel ({x}, {y}) is {got:?}, expected {expected:?}"
                );
            }
        }
    }
    Ok("slice 1 cleared, slice 0 untouched".into())
}

/// Calls `target[method](...args)`.
fn call(target: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, String> {
    let function: js_sys::Function = js_sys::Reflect::get(target, &method.into())
        .map_err(|error| format!("{method}: {error:?}"))?
        .dyn_into()
        .map_err(|_| format!("{method} is not a function"))?;
    function
        .apply(target, &args.iter().collect::<js_sys::Array>())
        .map_err(|error| format!("{method}: {error:?}"))
}

/// A plain JS object holding `entries`.
fn object(entries: &[(&str, JsValue)]) -> Result<JsValue, String> {
    let object = js_sys::Object::new();
    for (key, value) in entries {
        js_sys::Reflect::set(&object, &(*key).into(), value)
            .map_err(|error| format!("{key}: {error:?}"))?;
    }
    Ok(object.into())
}

/// Texels the browser's own queue writes into a `GPUTexture` made on the raw
/// `GPUDevice` read back through wgpu once the handle is wrapped. wgpu runs
/// the drop callback when it drops the wrapped texture, and neither its
/// `destroy` nor its drop destroys the handle, which reads back again.
async fn texture_from_handle(gpu: &Gpu) -> Result<String, String> {
    let device =
        sgl_webgpu::device_as_webgpu(&gpu.device).ok_or("the device has no GPUDevice handle")?;
    let extent = || js_sys::Array::of2(&SIZE.into(), &SIZE.into()).into();
    // GPUTextureUsage.COPY_SRC | GPUTextureUsage.COPY_DST.
    let descriptor = object(&[
        ("size", extent()),
        ("format", "rgba8unorm".into()),
        ("usage", 0x03.into()),
    ])?;
    let handle = call(&device, "createTexture", &[descriptor])?;
    let written: Vec<[u8; 4]> = (0..16u8)
        .map(|i| [i * 16, 255 - i * 16, i * 5 + 1, 255 - i])
        .collect();
    let bytes: Vec<u8> = written.iter().flatten().copied().collect();
    let queue =
        js_sys::Reflect::get(&device, &"queue".into()).map_err(|error| format!("{error:?}"))?;
    call(
        &queue,
        "writeTexture",
        &[
            object(&[("texture", handle.clone())])?,
            js_sys::Uint8Array::from(bytes.as_slice()).into(),
            object(&[
                ("bytesPerRow", (SIZE * 4).into()),
                ("rowsPerImage", SIZE.into()),
            ])?,
            extent(),
        ],
    )?;

    let descriptor = wgpu::TextureDescriptor {
        label: Some("from handle"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    };
    let dropped = std::rc::Rc::new(std::cell::Cell::new(false));
    let texture = sgl_webgpu::create_texture_from_webgpu_handle(
        &gpu.device,
        handle.clone(),
        &descriptor,
        Some(Box::new({
            let dropped = dropped.clone();
            move || dropped.set(true)
        })),
    );
    ensure!(
        sgl_webgpu::texture_as_webgpu(&texture).as_ref() == Some(&handle),
        "the wrapped texture's handle is not the one it wraps"
    );
    let got = texels(gpu, &texture, 0).await?;
    ensure!(got == written, "read back {got:?}, wrote {written:?}");
    texture.destroy();
    ensure!(
        !dropped.get(),
        "the drop callback ran before wgpu dropped the texture"
    );
    drop(texture);
    ensure!(
        dropped.get(),
        "dropping the texture never ran the drop callback"
    );

    let again =
        sgl_webgpu::create_texture_from_webgpu_handle(&gpu.device, handle, &descriptor, None);
    let got = texels(gpu, &again, 0).await?;
    ensure!(
        got == written,
        "after destroy and drop, read back {got:?}, wrote {written:?}"
    );
    Ok("16 texels written by the browser read back through wgpu, twice".into())
}

/// A canvas that already holds a 2D context gives WebGPU none, so
/// `create_surface` fails. The browser's reason goes to the console, where
/// the lane expects that one line.
fn surface_error(gpu: &Gpu) -> Result<String, String> {
    let canvas: web_sys::HtmlCanvasElement = web_sys::window()
        .and_then(|window| window.document())
        .ok_or("no document")?
        .create_element("canvas")
        .map_err(|error| format!("{error:?}"))?
        .dyn_into()
        .map_err(|_| "not a canvas")?;
    canvas
        .get_context("2d")
        .map_err(|error| format!("{error:?}"))?
        .ok_or("no 2d context")?;
    match gpu
        .instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
    {
        Ok(_) => Err("create_surface succeeded on a canvas holding a 2d context".into()),
        Err(error) => Ok(format!("create_surface: {error}")),
    }
}

/// The browser reports a bind group whose buffer is smaller than its
/// layout's minimum binding size to the scope around it, and nothing for a
/// valid one.
async fn error_scopes(gpu: &Gpu) -> Result<String, String> {
    let layout = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scoped"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(64),
                },
                count: None,
            }],
        });
    let bind_group = |size: u64| {
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniform"),
            size,
            usage: wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: false,
        });
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scoped"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        })
    };
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _invalid = bind_group(16);
    let invalid = scope.pop().await;
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _valid = bind_group(64);
    let valid = scope.pop().await;
    let description = match invalid {
        Some(wgpu::Error::Validation { description, .. }) => description,
        other => return Err(format!("the 16-byte binding's scope caught {other:?}")),
    };
    ensure!(
        valid.is_none(),
        "the 64-byte binding's scope caught {valid:?}"
    );
    Ok(format!(
        "16-byte binding: {}",
        description.lines().next().unwrap_or_default()
    ))
}

/// What the adapter's `get_texture_format_features` reports for every plain
/// colour format, on a device requested with every feature the adapter
/// reports, `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` (the browser's
/// `texture-formats-tier2`) among them, is what the browser accepts: each storage
/// access in a bind group layout, and the storage, render-attachment and
/// 4x multisample usages in a texture. The browser's own validation is the
/// oracle, so a capability the backend claims that the browser refuses, or
/// one it withholds that the browser grants, fails.
async fn texture_format_tiers(gpu: &Gpu) -> Result<String, String> {
    let feature = wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
    let adapter = gpu
        .instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
        .map_err(|error| format!("no second adapter: {error}"))?;
    ensure!(
        adapter.features().contains(feature),
        "the adapter does not report texture-formats-tier2 as {feature:?}"
    );
    let (device, _queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            required_features: adapter.features(),
            ..Default::default()
        })
        .await
        .map_err(|error| format!("no device with {feature:?}: {error}"))?;
    ensure!(
        device.features().contains(feature),
        "the device lacks {feature:?}"
    );
    let mut mismatches = Vec::new();
    let mut checked = 0;
    for format in PLAIN_COLOR_FORMATS {
        let claimed = adapter.get_texture_format_features(format);
        for (capability, claim) in capabilities(claimed) {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            capability.create(&device, format);
            let granted = scope.pop().await.is_none();
            checked += 1;
            if claim != granted {
                mismatches.push(format!("{format:?} {capability:?}: claimed {claim}"));
            }
        }
    }
    ensure!(
        mismatches.is_empty(),
        "{} of {checked} capabilities differ from the browser's: {}",
        mismatches.len(),
        mismatches.join("; ")
    );
    Ok(format!(
        "{checked} capabilities of {} formats as the browser grants them",
        PLAIN_COLOR_FORMATS.len()
    ))
}

/// WebGPU's plain colour formats (§26.1.1) but the 16-bit normalized ones,
/// whose feature this backend does not map.
const PLAIN_COLOR_FORMATS: [wgpu::TextureFormat; 36] = {
    use wgpu::TextureFormat as F;
    [
        F::R8Unorm,
        F::R8Snorm,
        F::R8Uint,
        F::R8Sint,
        F::Rg8Unorm,
        F::Rg8Snorm,
        F::Rg8Uint,
        F::Rg8Sint,
        F::Rgba8Unorm,
        F::Rgba8UnormSrgb,
        F::Rgba8Snorm,
        F::Rgba8Uint,
        F::Rgba8Sint,
        F::Bgra8Unorm,
        F::Bgra8UnormSrgb,
        F::R16Uint,
        F::R16Sint,
        F::R16Float,
        F::Rg16Uint,
        F::Rg16Sint,
        F::Rg16Float,
        F::Rgba16Uint,
        F::Rgba16Sint,
        F::Rgba16Float,
        F::R32Uint,
        F::R32Sint,
        F::R32Float,
        F::Rg32Uint,
        F::Rg32Sint,
        F::Rg32Float,
        F::Rgba32Uint,
        F::Rgba32Sint,
        F::Rgba32Float,
        F::Rgb10a2Uint,
        F::Rgb10a2Unorm,
        F::Rg11b10Ufloat,
    ]
};

/// One format capability the browser validates when an object is created.
#[derive(Clone, Copy, Debug)]
enum Capability {
    /// A bind group layout entry of this storage access.
    Storage(wgpu::StorageTextureAccess),
    /// A texture of this usage and sample count.
    Texture(wgpu::TextureUsages, u32),
}

impl Capability {
    fn create(self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        match self {
            Self::Storage(access) => {
                let _ = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("format tier"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access,
                            format,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    }],
                });
            }
            Self::Texture(usage, sample_count) => {
                let _ = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("format tier"),
                    size: wgpu::Extent3d {
                        width: SIZE,
                        height: SIZE,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                });
            }
        }
    }
}

/// The capabilities checked, each with whether `claimed` grants it.
fn capabilities(claimed: wgpu::TextureFormatFeatures) -> [(Capability, bool); 6] {
    use wgpu::StorageTextureAccess as Access;
    use wgpu::TextureFormatFeatureFlags as Flags;
    use wgpu::TextureUsages as Usages;
    let flag = |flag| claimed.flags.contains(flag);
    let usage = |usage| claimed.allowed_usages.contains(usage);
    [
        (
            Capability::Storage(Access::ReadOnly),
            flag(Flags::STORAGE_READ_ONLY),
        ),
        (
            Capability::Storage(Access::WriteOnly),
            flag(Flags::STORAGE_WRITE_ONLY),
        ),
        (
            Capability::Storage(Access::ReadWrite),
            flag(Flags::STORAGE_READ_WRITE),
        ),
        (
            Capability::Texture(Usages::STORAGE_BINDING, 1),
            usage(Usages::STORAGE_BINDING),
        ),
        (
            Capability::Texture(Usages::RENDER_ATTACHMENT, 1),
            usage(Usages::RENDER_ATTACHMENT),
        ),
        (
            Capability::Texture(Usages::RENDER_ATTACHMENT, 4),
            flag(Flags::MULTISAMPLE_X4),
        ),
    ]
}

/// The browser's compilation message for an unresolved name points, in
/// UTF-8 bytes, at that name on its line, past multi-byte characters.
async fn compilation_info(gpu: &Gpu) -> Result<String, String> {
    const SOURCE: &str = "@compute @workgroup_size(1)\nfn main() {\n    /* ünïcödé ✓ */ let x = undefined_name;\n}\n";
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("broken"),
            source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
        });
    let caught = scope.pop().await;
    let info = module.get_compilation_info().await;
    ensure!(
        caught.is_some(),
        "creating the broken module raised no validation error"
    );
    let message = info
        .messages
        .iter()
        .find(|message| message.message_type == wgpu::CompilationMessageType::Error)
        .ok_or_else(|| format!("no error message among {:?}", info.messages))?;
    let location = message
        .location
        .ok_or_else(|| format!("the error has no location: {}", message.message))?;
    ensure!(
        location.line_number == 3,
        "the error is on line {}",
        location.line_number
    );
    let at = &SOURCE[location.offset as usize..];
    ensure!(
        at.starts_with("undefined_name"),
        "the error points at {:?}",
        at.lines().next().unwrap_or_default()
    );
    Ok(format!("line 3: {}", message.message))
}

/// A canvas surface configured, acquired, cleared and presented shows the
/// clear colour to the page.
async fn surface_present(gpu: &Gpu) -> Result<String, String> {
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or("no document")?;
    let canvas: web_sys::HtmlCanvasElement = document
        .create_element("canvas")
        .map_err(|error| format!("{error:?}"))?
        .dyn_into()
        .map_err(|_| "not a canvas")?;
    let surface = gpu
        .instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
        .map_err(|error| format!("create_surface: {error}"))?;
    let mut config = surface
        .get_default_config(&gpu.adapter, SIZE, SIZE)
        .ok_or("the surface supports no format")?;
    config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
    surface.configure(&gpu.device, &config);
    let frame = match surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(frame) => frame,
        other => return Err(format!("get_current_texture: {other:?}")),
    };
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("present"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: 0.2,
                    g: 0.4,
                    b: 0.6,
                    a: 1.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    gpu.queue.submit([encoder.finish()]);
    gpu.queue.present(frame);
    // The browser presents the canvas when it next updates the rendering.
    next_frame().await?;
    next_frame().await?;

    let copy = web_sys::OffscreenCanvas::new(SIZE, SIZE).map_err(|error| format!("{error:?}"))?;
    let context: web_sys::OffscreenCanvasRenderingContext2d = copy
        .get_context("2d")
        .map_err(|error| format!("{error:?}"))?
        .ok_or("no 2d context")?
        .dyn_into()
        .map_err(|_| "not a 2d context")?;
    context
        .draw_image_with_html_canvas_element(&canvas, 0.0, 0.0)
        .map_err(|error| format!("drawImage: {error:?}"))?;
    let pixels = context
        .get_image_data(0.0, 0.0, f64::from(SIZE), f64::from(SIZE))
        .map_err(|error| format!("getImageData: {error:?}"))?
        .data();
    // 0.2, 0.4 and 0.6 of 255.
    let expected = [51, 102, 153, 255];
    for (index, pixel) in pixels.chunks_exact(4).enumerate() {
        let got = [pixel[0], pixel[1], pixel[2], pixel[3]];
        ensure!(
            near(got, expected),
            "canvas pixel {index} shows {got:?}, expected {expected:?}"
        );
    }
    Ok(format!("{:?} canvas shows the clear colour", config.format))
}

async fn next_frame() -> Result<(), String> {
    let window = web_sys::window().ok_or("no window")?;
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let _ = window.request_animation_frame(&resolve);
    });
    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map(|_| ())
        .map_err(|error| format!("requestAnimationFrame: {error:?}"))
}

/// A render pipeline created asynchronously draws as its shader says: the
/// whole target red, then its right half green.
async fn async_render_pipeline(gpu: &Gpu) -> Result<String, String> {
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("draw"),
            source: wgpu::ShaderSource::Wgsl(DRAW_SHADER.into()),
        });
    let pipeline = sgl_webgpu::create_render_pipeline_async(
        &gpu.device,
        &wgpu::RenderPipelineDescriptor {
            label: Some("asynchronous draw"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
            }),
            multiview_mask: None,
            cache: None,
        },
    )
    .await
    .map_err(|error| format!("creation failed: {error}"))?;
    let texture = target(
        gpu,
        wgpu::TextureFormat::Rgba8Unorm,
        1,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("asynchronous draw"),
            color_attachments: &[Some(color_attachment(&view, None))],
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..6, 0..1);
    }
    gpu.queue.submit([encoder.finish()]);
    let texels = texels(gpu, &texture, 0).await?;
    for (index, &got) in texels.iter().enumerate() {
        let x = index % SIZE as usize;
        let expected = if x < 2 { RED } else { GREEN };
        ensure!(
            got == expected,
            "pixel ({x}, {}) is {got:?}, expected {expected:?}",
            index / SIZE as usize
        );
    }
    Ok("left half red, right half green".into())
}

/// A compute pipeline created asynchronously, with its layout derived from
/// its shader and a constant set at creation, writes the values its shader
/// says.
async fn async_compute_pipeline(gpu: &Gpu) -> Result<String, String> {
    const COUNT: u32 = 64;
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("asynchronous compute"),
            source: wgpu::ShaderSource::Wgsl(
                "
override step: u32 = 1u;
@group(0) @binding(0) var<storage, read_write> values: array<u32, 64>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    values[id.x] = id.x * step + 5u;
}
"
                .into(),
            ),
        });
    let pipeline = sgl_webgpu::create_compute_pipeline_async(
        &gpu.device,
        &wgpu::ComputePipelineDescriptor {
            label: Some("asynchronous compute"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &[("step", 7.0)],
                ..Default::default()
            },
            cache: None,
        },
    )
    .await
    .map_err(|error| format!("creation failed: {error}"))?;
    let size = u64::from(COUNT * 4);
    let storage = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("values"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("asynchronous compute"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: storage.as_entire_binding(),
        }],
    });
    let readback = readback_buffer(gpu, size);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&storage, 0, &readback, 0, size);
    gpu.queue.submit([encoder.finish()]);
    let bytes = read(&readback, 0..size).await?;
    for (i, value) in (0..COUNT).zip(bytes.chunks_exact(4)) {
        let got = u32::from_le_bytes([value[0], value[1], value[2], value[3]]);
        let expected = i * 7 + 5;
        ensure!(got == expected, "value {i} is {got}, expected {expected}");
    }
    Ok(format!("{COUNT} values, step constant 7"))
}

/// A pipeline the browser must reject, its entry point missing from its
/// module, resolves to a validation error that names the entry point, and
/// a validation scope around its creation catches nothing.
async fn async_pipeline_error(gpu: &Gpu) -> Result<String, String> {
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("no such entry point"),
            source: wgpu::ShaderSource::Wgsl("@compute @workgroup_size(1) fn main() {}".into()),
        });
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let created = sgl_webgpu::create_compute_pipeline_async(
        &gpu.device,
        &wgpu::ComputePipelineDescriptor {
            label: Some("no such entry point"),
            layout: None,
            module: &module,
            entry_point: Some("absent"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        },
    )
    .await;
    let caught = scope.pop().await;
    let description = match created {
        Err(wgpu::Error::Validation { description, .. }) => description,
        Err(other) => return Err(format!("rejected with a non-validation error: {other:?}")),
        Ok(_) => return Err("the browser created it".into()),
    };
    ensure!(
        description.contains("absent"),
        "the error does not name the entry point: {description}"
    );
    ensure!(caught.is_none(), "the error scope caught {caught:?}");
    Ok(format!(
        "validation error: {}",
        description.lines().next().unwrap_or_default()
    ))
}
