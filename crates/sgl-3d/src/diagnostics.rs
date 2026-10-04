//! Raw GPU observations for renderer integration tests and diagnostics
//! (feature `diagnostics`): the frame's intermediate targets and readback.
use crate::InstanceId;
use crate::content::identity::Identity;

/// The value `DiagnosticTarget::SourceId`'s R channel holds for `instance`'s
/// pixels in a frame rendered while the scene had it.
pub fn source_id(instance: InstanceId) -> u32 {
    instance.index() as u32 + 1
}

/// An intermediate target of the last rendered frame
/// (`Renderer::diagnostic_target`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticTarget {
    /// Depth32Float, reversed-Z.
    Depth,
    /// Rgba16Float: signed octahedral world-space base normal in RG and coat
    /// normal in BA.
    Normal,
    /// Rg16Float: current minus previous unjittered UV, +y down.
    Motion,
    /// Rg32Uint: each pixel's raster source in R (0 none, else the
    /// instance's [`source_id`]) and primitive in G.
    SourceId,
    /// The linear HDR scene after reflections and transparent effects.
    Composite,
    /// R32Uint: XeGTAO visibility in 0..=255, when ambient occlusion ran.
    AmbientOcclusion,
    /// Rgba16Float at the output size: the tone-mapped scene before
    /// presentation, when the frame captured it
    /// (`Diagnostics::capture_tone_target` or `frame_probe`).
    ToneMapped,
    /// R32Float, 1×1: the frame's exposure multiplier, which the tone map
    /// and FSR2 read.
    Exposure,
}

pub fn read(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    bpp: u32,
) -> Vec<u8> {
    let size = texture.size();
    let row = (size.width * bpp).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("frame evidence"),
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
    buffer.map_async(wgpu::MapMode::Read, .., move |r| {
        tx.send(r).unwrap();
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().unwrap();
    let mapped = buffer.get_mapped_range(..);
    let mut pixels = Vec::new();
    for line in mapped.chunks(row as usize) {
        pixels.extend_from_slice(&line[..(size.width * bpp) as usize]);
    }
    pixels
}
pub fn half(bytes: &[u8]) -> f32 {
    let bits = u16::from_le_bytes([bytes[0], bytes[1]]);
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = ((bits >> 10) & 31) as i32;
    let fraction = (bits & 1023) as f32;
    sign * if exponent == 0 {
        fraction * 2f32.powi(-24)
    } else if exponent == 31 {
        if fraction == 0. {
            f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        (1. + fraction / 1024.) * 2f32.powi(exponent - 15)
    }
}
