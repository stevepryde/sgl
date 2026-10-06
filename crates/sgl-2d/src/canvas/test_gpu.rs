//! Headless wgpu helpers for the native GPU tests: a device from whatever
//! adapter the host offers (no surface, no winit) and a texture readback.
//! A host without any adapter is not a failure by default — [`device`]
//! returns `None` and the test prints a skip message and returns. With
//! [`REQUIRE_GPU_ENV`] set, a missing adapter panics instead, so a host that
//! is known to have one (the `check` task sets it on macOS) cannot pass with
//! the pixel tests silently skipped.

use std::sync::mpsc;

use crate::canvas::gpu::Gpu;

/// Environment variable that makes a missing adapter a test failure rather
/// than a skip. Any value other than empty or `0` requires the GPU.
pub(crate) const REQUIRE_GPU_ENV: &str = "SGL_REQUIRE_GPU";

/// Report an unavailable GPU: panic when [`REQUIRE_GPU_ENV`] demands one,
/// otherwise print the skip reason and return `None`.
fn unavailable<T>(reason: &str) -> Option<T> {
    let required = std::env::var(REQUIRE_GPU_ENV).is_ok_and(|v| !v.is_empty() && v != "0");
    assert!(
        !required,
        "GPU test cannot run but {REQUIRE_GPU_ENV} is set: {reason}"
    );
    eprintln!("skipping GPU test: {reason}");
    None
}

/// A headless [`Gpu`], or `None` (after printing why) when no wgpu adapter
/// is available or the adapter cannot provide a default-limits device.
/// Panics instead of returning `None` when [`REQUIRE_GPU_ENV`] is set.
pub(crate) fn gpu() -> Option<Gpu> {
    match Gpu::headless() {
        Ok(gpu) => Some(gpu),
        Err(err) => unavailable(&err.to_string()),
    }
}

/// [`gpu`] as a bare device + queue pair.
pub(crate) fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    gpu().map(|gpu| (gpu.device, gpu.queue))
}

/// Read a `COPY_SRC` 2D texture back as tightly packed rows of
/// `bytes_per_pixel` bytes (wgpu's 256-byte row pitch padding stripped).
/// Blocks until the copy lands.
pub(crate) fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
) -> Vec<u8> {
    let unpadded = bytes_per_pixel * width;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test readback buffer"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("test readback encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let (tx, rx) = mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll for readback");
    rx.recv()
        .expect("map callback")
        .expect("map readback buffer");
    let data = buffer
        .slice(..)
        .get_mapped_range()
        .expect("mapped readback buffer");
    let mut out = Vec::with_capacity((unpadded * height) as usize);
    for row in 0..height {
        let start = (row * padded) as usize;
        out.extend_from_slice(&data[start..start + unpadded as usize]);
    }
    out
}

/// Decode one IEEE 754 binary16 value (little-endian bits) — the channel
/// type of `Rgba16Float` readbacks. Subnormals and infinities included; NaN
/// decodes as NaN.
pub(crate) fn f16_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exp = i32::from((bits >> 10) & 0x1f);
    let mant = f32::from(bits & 0x3ff);
    let magnitude = match exp {
        0 => mant * 2f32.powi(-24),
        0x1f if mant == 0.0 => f32::INFINITY,
        0x1f => f32::NAN,
        _ => (1.0 + mant / 1024.0) * 2f32.powi(exp - 15),
    };
    sign * magnitude
}

/// Decode an `Rgba16Float` readback into `f32` channels (r, g, b, a per
/// pixel, row-major).
pub(crate) fn rgba16f_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|b| f16_to_f32(u16::from_le_bytes([b[0], b[1]])))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::f16_to_f32;

    /// Hand-encoded binary16 values: 1.0 = 0x3C00, 0.5 = 0x3800, -2.0 =
    /// 0xC000, 0.75 = 0x3A00, the smallest subnormal 0x0001 = 2⁻²⁴.
    #[test]
    fn f16_decodes_known_encodings() {
        assert_eq!(f16_to_f32(0x3C00), 1.0);
        assert_eq!(f16_to_f32(0x3800), 0.5);
        assert_eq!(f16_to_f32(0xC000), -2.0);
        assert_eq!(f16_to_f32(0x3A00), 0.75);
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
        assert_eq!(f16_to_f32(0x0000), 0.0);
    }
}
