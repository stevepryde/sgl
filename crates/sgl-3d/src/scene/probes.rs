//! The scene's specular probe collection on the GPU: the installed probes'
//! cube array and metadata (`baked_specular_probe`'s content), and
//! `Scene::set_baked_specular_probes`.
use super::probe_grid::ProbeGrid;
use super::{Scene, buffer};
use crate::baked_specular_probe::{
    BakedSpecularProbe, LEVELS, MAX_PROBES, ProbeError, SpecularProbeTexels,
};

impl SpecularProbeTexels {
    fn format(&self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba16Float(_) => wgpu::TextureFormat::Rgba16Float,
            Self::Bc6hUfloat(_) => wgpu::TextureFormat::Bc6hRgbUfloat,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProbeMetadata {
    world_to_local: [[f32; 4]; 4],
    /// W is 1 when the proxy corrects parallax.
    center: [f32; 4],
    influence_min: [f32; 4],
    influence_max: [f32; 4],
    blend: [f32; 4],
    proxy_min: [f32; 4],
    proxy_max: [f32; 4],
    sphere: [f32; 4],
}

// One immutable storage buffer: fixed arrays keep host and WGSL indexing
// bounded independently of caller data. The world grid's words follow it
// in the same buffer (`ProbeCollection::grid`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CollectionMetadata {
    counts: [u32; 4],
    /// `probe_grid::ProbeGrid`'s origin, scale and size.
    grid_origin: [f32; 3],
    grid_scale: f32,
    grid_size: [u32; 3],
    padding: u32,
    probes: [ProbeMetadata; MAX_PROBES],
}

impl CollectionMetadata {
    fn new(grid: &ProbeGrid) -> Self {
        Self {
            grid_origin: grid.origin.to_array(),
            grid_scale: grid.scale,
            grid_size: grid.size.to_array(),
            ..bytemuck::Zeroable::zeroed()
        }
    }
}

pub(crate) struct UploadedProbes {
    pub view: wgpu::TextureView,
    /// The collection's metadata, then the world grid's cells and their
    /// probe lists (`ProbeGrid::words`).
    pub metadata: wgpu::Buffer,
}

pub(crate) fn validate_face_size(face_size: u32, limits: &wgpu::Limits) -> Result<(), ProbeError> {
    if !face_size.is_power_of_two()
        || face_size < 1 << (LEVELS - 1)
        || face_size > limits.max_texture_dimension_2d
    {
        return Err(ProbeError::InvalidDimensions);
    }
    Ok(())
}

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

impl BakedSpecularProbe {
    fn validate(&self, limits: &wgpu::Limits) -> Result<(), ProbeError> {
        self.validate_placement()?;
        validate_face_size(self.radiance.face_size, limits)?;
        let texels = &self.radiance.texels;
        let expected = payload_size(texels.format(), self.radiance.face_size);
        if texels.bytes().len() != expected {
            return Err(ProbeError::InvalidPayloadLength {
                expected,
                actual: texels.bytes().len(),
            });
        }
        texels.validate_radiance()
    }
}

fn cube_array(
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

fn cube_array_view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::CubeArray),
        ..Default::default()
    })
}

impl UploadedProbes {
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        probes: &[BakedSpecularProbe],
    ) -> Result<Self, ProbeError> {
        let limits = device.limits();
        if probes.len() > MAX_PROBES || probes.len() as u32 * 6 > limits.max_texture_array_layers {
            return Err(ProbeError::TooManyProbes);
        }
        let face_size = probes[0].radiance.face_size;
        let format = probes[0].radiance.texels.format();
        if format.required_features() != wgpu::Features::empty()
            && !device.features().contains(format.required_features())
        {
            return Err(ProbeError::CompressionUnsupported);
        }
        for probe in probes {
            probe.validate(&limits)?;
            if probe.radiance.face_size != face_size || probe.radiance.texels.format() != format {
                return Err(ProbeError::MixedRadiance);
            }
        }
        let grid = ProbeGrid::new(probes);
        let mut metadata = CollectionMetadata::new(&grid);
        metadata.counts[0] = probes.len() as u32;
        for (slot, probe) in metadata.probes.iter_mut().zip(probes) {
            let proxy = probe.proxy.unwrap_or(probe.influence);
            *slot = ProbeMetadata {
                world_to_local: probe.world_to_local.to_cols_array_2d(),
                center: probe
                    .center
                    .extend(f32::from(probe.proxy.is_some()))
                    .to_array(),
                influence_min: probe.influence.min.extend(0.).to_array(),
                influence_max: probe.influence.max.extend(0.).to_array(),
                blend: probe.blend.extend(0.).to_array(),
                proxy_min: proxy.min.extend(0.).to_array(),
                proxy_max: proxy.max.extend(0.).to_array(),
                sphere: probe
                    .world_to_local
                    .inverse()
                    .transform_point3((probe.influence.min + probe.influence.max) * 0.5)
                    .extend((probe.influence.max - probe.influence.min).length() * 0.5)
                    .to_array(),
            };
        }
        let texture = cube_array(
            device,
            "baked specular probes",
            format,
            face_size,
            probes.len() as u32,
            LEVELS,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let (block_width, block_height) = format.block_dimensions();
        for (index, probe) in probes.iter().enumerate() {
            let bytes = probe.radiance.texels.bytes();
            let mut offset = 0;
            for level in 0..LEVELS {
                let size = face_size >> level;
                let (row, rows) = level_rows(format, size);
                let count = row as usize * rows as usize * 6;
                queue.write_texture(
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
        Ok(Self {
            view: cube_array_view(&texture),
            metadata: collection_buffer(
                device,
                "baked specular probe collection",
                &metadata,
                &grid,
            ),
        })
    }

    pub(crate) fn empty(device: &wgpu::Device) -> Self {
        let grid = ProbeGrid::empty();
        Self {
            view: cube_array_view(&cube_array(
                device,
                "empty specular probe collection",
                wgpu::TextureFormat::Rgba16Float,
                1,
                1,
                1,
                wgpu::TextureUsages::TEXTURE_BINDING,
            )),
            metadata: collection_buffer(
                device,
                "empty specular probe collection",
                &CollectionMetadata::new(&grid),
                &grid,
            ),
        }
    }
}

/// `metadata`, then `grid`'s words, padded to the struct's 16-byte
/// alignment, which a binding of it must fill.
fn collection_buffer(
    device: &wgpu::Device,
    label: &str,
    metadata: &CollectionMetadata,
    grid: &ProbeGrid,
) -> wgpu::Buffer {
    let mut bytes = bytemuck::bytes_of(metadata).to_vec();
    bytes.extend_from_slice(bytemuck::cast_slice(&grid.words));
    bytes.resize(bytes.len().next_multiple_of(16), 0);
    buffer(device, label, &bytes, wgpu::BufferUsages::STORAGE)
}

impl Scene {
    /// Replace the installed collection; an empty slice clears it. All probes
    /// must share one face size. Invalid input leaves the previous collection
    /// installed. Receivers take environment specular from the probes whose
    /// influence contains them, then the sky.
    pub fn set_baked_specular_probes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        probes: &[BakedSpecularProbe],
    ) -> Result<(), super::SceneError> {
        self.baked_specular_probes = if probes.is_empty() {
            None
        } else {
            Some(UploadedProbes::new(device, queue, probes)?)
        };
        // World-space ray hits bind the collection in a renderer's group 0.
        self.resources = super::next_generation();
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 2] {
    use crate::shading::layout_tests::mirror;
    [
        mirror!(
            "geometry",
            "BakedProbe",
            ProbeMetadata,
            [
                world_to_local,
                center,
                influence_min,
                influence_max,
                blend,
                proxy_min,
                proxy_max,
                sphere,
            ]
        ),
        mirror!(
            "geometry",
            "ProbeCollection",
            CollectionMetadata,
            [counts, grid_origin, grid_scale, grid_size, padding, probes]
        ),
    ]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::baked_specular_probe::{SpecularProbeBox, SpecularProbeRadiance};
    use glam::{Mat4, Vec3};

    fn fixture() -> BakedSpecularProbe {
        BakedSpecularProbe {
            center: Vec3::ZERO,
            world_to_local: Mat4::IDENTITY,
            influence: SpecularProbeBox {
                min: Vec3::splat(-2.),
                max: Vec3::splat(2.),
            },
            blend: Vec3::splat(0.5),
            proxy: Some(SpecularProbeBox {
                min: Vec3::splat(-2.),
                max: Vec3::splat(2.),
            }),
            radiance: SpecularProbeRadiance {
                face_size: 64,
                texels: SpecularProbeTexels::Rgba16Float(vec![
                    0x3c00;
                    payload_size(
                        wgpu::TextureFormat::Rgba16Float,
                        64
                    ) / 2
                ]),
            },
        }
    }

    // Malformed asset input must be rejected before texture creation/upload,
    // which otherwise reports a GPU validation error or admits poisoned radiance.
    #[test]
    fn malformed_radiance_and_truncated_levels_are_rejected() {
        fn values(probe: &mut BakedSpecularProbe) -> &mut Vec<u16> {
            match &mut probe.radiance.texels {
                SpecularProbeTexels::Rgba16Float(values) => values,
                SpecularProbeTexels::Bc6hUfloat(_) => unreachable!(),
            }
        }
        let mut probe = fixture();
        for invalid in [0x7c00, 0xfc00, 0x7e00, 0xbc00, 0x8001] {
            values(&mut probe)[17] = invalid;
            assert!(matches!(
                probe.validate(&wgpu::Limits::default()),
                Err(ProbeError::InvalidRadiance { component: 17 })
            ));
        }
        values(&mut probe)[17] = 0x8000; // IEEE negative zero is finite and nonnegative.
        assert!(probe.validate(&wgpu::Limits::default()).is_ok());
        values(&mut probe).pop();
        assert!(matches!(
            probe.validate(&wgpu::Limits::default()),
            Err(ProbeError::InvalidPayloadLength { .. })
        ));
    }

    #[test]
    fn box_projection_rejects_scale_shear_and_outside_capture_origin() {
        let mut probe = fixture();
        probe.world_to_local = Mat4::from_scale(Vec3::new(1., 2., 1.));
        assert!(matches!(
            probe.validate(&wgpu::Limits::default()),
            Err(ProbeError::InvalidProbe(_))
        ));
        probe.world_to_local = Mat4::IDENTITY;
        probe.world_to_local.y_axis.x = 0.2;
        assert!(matches!(
            probe.validate(&wgpu::Limits::default()),
            Err(ProbeError::InvalidProbe(_))
        ));
        probe.world_to_local =
            Mat4::from_rotation_y(0.7) * Mat4::from_translation(Vec3::new(-10., 0., 0.));
        assert!(matches!(
            probe.validate(&wgpu::Limits::default()),
            Err(ProbeError::InvalidProbe(_))
        ));
        probe.center = Vec3::new(10., 0., 0.);
        assert!(probe.validate(&wgpu::Limits::default()).is_ok());
        probe.blend.x = -0.1;
        assert!(matches!(
            probe.validate(&wgpu::Limits::default()),
            Err(ProbeError::InvalidProbe(_))
        ));
    }

    #[test]
    fn unaddressable_face_sizes_are_rejected_before_payload_access() {
        let mut probe = fixture();
        for face_size in [0, 32, 96, u32::MAX] {
            probe.radiance.face_size = face_size;
            assert!(matches!(
                probe.validate(&wgpu::Limits::default()),
                Err(ProbeError::InvalidDimensions)
            ));
        }
    }
}
