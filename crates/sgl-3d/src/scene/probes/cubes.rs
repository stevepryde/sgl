//! The probe collection's cube array: its texture, the upload of baked
//! probes' texels into it, and the GPU copies that carry cubes from one
//! array to the next or publish a live probe's.
use crate::baked_specular_probe::{BakedSpecularProbe, LEVELS};

/// Bytes per row and rows of one face of a level `size` texels wide.
fn level_rows(format: wgpu::TextureFormat, size: u32) -> (u32, u32) {
    let (width, height) = format.block_dimensions();
    let block = format.block_copy_size(None).unwrap_or(0);
    (size.div_ceil(width) * block, size.div_ceil(height))
}

/// Bytes of a prefiltered cube's every level and face.
pub(crate) fn payload_size(format: wgpu::TextureFormat, face_size: u32) -> usize {
    (0..LEVELS)
        .map(|level| {
            let (row, rows) = level_rows(format, face_size >> level);
            row as usize * rows as usize * 6
        })
        .sum()
}

pub(super) fn cube_array(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
    face_size: u32,
    cubes: u32,
    levels: u32,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: face_size,
            height: face_size,
            depth_or_array_layers: cubes * 6,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

pub(super) fn cube_array_view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::CubeArray),
        ..Default::default()
    })
}

/// Copies every level of `count` cubes from cube `from.1` of `from.0` to
/// cube `to.1` of `to.0`, both `Rgba16Float` arrays of `face_size` texels.
pub(crate) fn copy_cubes(
    encoder: &mut wgpu::CommandEncoder,
    from: (&wgpu::Texture, u32),
    to: (&wgpu::Texture, u32),
    count: u32,
    face_size: u32,
) {
    let at = |texture, cube: u32, level| wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: level,
        origin: wgpu::Origin3d {
            x: 0,
            y: 0,
            z: cube * 6,
        },
        aspect: wgpu::TextureAspect::All,
    };
    for level in 0..LEVELS {
        let size = face_size >> level;
        encoder.copy_texture_to_texture(
            at(from.0, from.1, level),
            at(to.0, to.1, level),
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: count * 6,
            },
        );
    }
}

/// Writes each of `probes`' texels into its cube of `texture`, from cube 0.
pub(super) fn upload(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    probes: &[BakedSpecularProbe],
    face_size: u32,
) {
    let format = texture.format();
    let (block_width, block_height) = format.block_dimensions();
    for (index, probe) in probes.iter().enumerate() {
        let bytes = probe.radiance.texels.bytes();
        let mut offset = 0;
        for level in 0..LEVELS {
            let size = face_size >> level;
            let (row, rows) = level_rows(format, size);
            let count = row as usize * rows as usize * 6;
            crate::counters::write_texture(
                queue,
                wgpu::TexelCopyTextureInfo {
                    mip_level: level,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: index as u32 * 6,
                    },
                    ..texture.as_image_copy()
                },
                &bytes[offset..offset + count],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(rows),
                },
                // A level smaller than a block copies the whole block.
                wgpu::Extent3d {
                    width: size.div_ceil(block_width) * block_width,
                    height: size.div_ceil(block_height) * block_height,
                    depth_or_array_layers: 6,
                },
            );
            offset += count;
        }
    }
}
