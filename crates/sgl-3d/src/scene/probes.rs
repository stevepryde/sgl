//! The scene's specular probe collection on the GPU: the installed probes'
//! cube array and metadata (`baked_specular_probe`'s content), and
//! `Scene::set_baked_specular_probes`.
use super::probe_grid::{ProbeGrid, REACH};
use super::{Scene, buffer};
use crate::baked_specular_probe::{
    BakedSpecularProbe, LEVELS, MAX_PROBES, ProbeError, SpecularProbeBox, SpecularProbeTexels,
};
use glam::{Mat3, Mat4, Vec3};

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
    /// The capture position in the probe's frame; w is 1 when the proxy
    /// corrects parallax.
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
    /// Each probe's placement, which the metadata and grid are built from.
    placements: Vec<Placement>,
}

/// Where a probe is: what its metadata and the world grid hold of it.
#[derive(Clone, Copy)]
struct Placement {
    /// The capture position in the probe's frame, which a render origin
    /// move leaves unchanged.
    center: Vec3,
    world_to_local: Mat4,
    influence: SpecularProbeBox,
    blend: Vec3,
    proxy: Option<SpecularProbeBox>,
}

impl Placement {
    fn new(probe: &BakedSpecularProbe) -> Self {
        Self {
            center: probe.world_to_local.transform_point3(probe.center),
            world_to_local: probe.world_to_local,
            influence: probe.influence,
            blend: probe.blend,
            proxy: probe.proxy,
        }
    }

    /// The placement in a render frame whose origin lies `by` from this
    /// one's: its frame's origin translated, and its rigid `world_to_local`
    /// rebuilt from that origin rather than translated.
    fn translated(self, by: Vec3) -> Self {
        let rotation = Mat3::from_mat4(self.world_to_local);
        let origin = -(rotation.transpose() * self.world_to_local.w_axis.truncate()) - by;
        let mut world_to_local = self.world_to_local;
        world_to_local.w_axis = (-(rotation * origin)).extend(self.world_to_local.w_axis.w);
        Self {
            world_to_local,
            ..self
        }
    }

    fn metadata(&self) -> ProbeMetadata {
        // Within REACH, as the grid keeps influences, box projection's hit
        // and capture centre stay a finite distance apart
        // (probe_parallax_direction); a proxy that large already acts as
        // distant.
        let reach = Vec3::splat(REACH);
        let proxy = self.proxy.unwrap_or(self.influence);
        let proxy = SpecularProbeBox {
            min: proxy.min.clamp(-reach, reach),
            max: proxy.max.clamp(-reach, reach),
        };
        ProbeMetadata {
            world_to_local: self.world_to_local.to_cols_array_2d(),
            center: self
                .center
                .clamp(proxy.min, proxy.max)
                .extend(f32::from(self.proxy.is_some()))
                .to_array(),
            influence_min: self.influence.min.extend(0.).to_array(),
            influence_max: self.influence.max.extend(0.).to_array(),
            blend: self.blend.extend(0.).to_array(),
            proxy_min: proxy.min.extend(0.).to_array(),
            proxy_max: proxy.max.extend(0.).to_array(),
            sphere: self
                .world_to_local
                .inverse()
                .transform_point3((self.influence.min + self.influence.max) * 0.5)
                .extend((self.influence.max - self.influence.min).length() * 0.5)
                .to_array(),
        }
    }
}

/// The collection's metadata and world grid over `placements`, in one
/// buffer.
fn collection(device: &wgpu::Device, placements: &[Placement]) -> wgpu::Buffer {
    let influences: Vec<_> = placements
        .iter()
        .map(|placement| (placement.world_to_local, placement.influence))
        .collect();
    let grid = ProbeGrid::new(&influences);
    let mut metadata = CollectionMetadata::new(&grid);
    metadata.counts[0] = placements.len() as u32;
    for (slot, placement) in metadata.probes.iter_mut().zip(placements) {
        *slot = placement.metadata();
    }
    collection_buffer(device, "baked specular probe collection", &metadata, &grid)
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
        let placements: Vec<_> = probes.iter().map(Placement::new).collect();
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
        Ok(Self {
            view: cube_array_view(&texture),
            metadata: collection(device, &placements),
            placements,
        })
    }

    /// Moves the render origin by `by` (`Scene::move_origin`): each probe's
    /// frame, which carries its centre, and the world grid built over them
    /// anew. A capture's radiance is about its centre and is unchanged.
    pub(crate) fn move_origin(&mut self, device: &wgpu::Device, by: Vec3) {
        for placement in &mut self.placements {
            *placement = placement.translated(by);
        }
        self.metadata = collection(device, &self.placements);
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
            placements: Vec::new(),
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
        self.edited();
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

    // Defect: a box wider than f32 can hold passes as finite with `min <
    // max`, and its infinite extent reaches the probe grid and shading.
    // Independent signal: f32 overflow of `max - min`.
    #[test]
    fn boxes_wider_than_f32_can_hold_are_rejected() {
        let mut probe = fixture();
        let huge = SpecularProbeBox {
            min: Vec3::splat(-1e13),
            max: Vec3::splat(1e13),
        };
        probe.influence = huge;
        probe.proxy = Some(huge);
        assert!(probe.validate(&wgpu::Limits::default()).is_ok());
        let overflowing = SpecularProbeBox {
            min: Vec3::new(-1., -f32::MAX, -1.),
            max: Vec3::new(1., f32::MAX, 1.),
        };
        probe.influence = overflowing;
        assert!(matches!(
            probe.validate(&wgpu::Limits::default()),
            Err(ProbeError::InvalidProbe(_))
        ));
        probe.influence = huge;
        probe.proxy = Some(overflowing);
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

    // Defect: box projection normalised `hit - centre` by its length, whose
    // square overflows f32 once the proxy reaches about 1e19 m, and a proxy
    // near f32's range overflowed the hit distance too, so the probe was
    // sampled toward zero or NaN: an arbitrary texel. Oracle: Lagarde and
    // Zanuttini's construction. A proxy that large acts as distant, so a
    // receiver near the capture centre samples the face its reflection
    // direction's major axis names; an ordinary proxy is sampled toward the
    // ray's hit on it from the capture centre. Each face holds its own value
    // (+X 1, -X 2, +Y 3, -Y 4, +Z 5, -Z 6) on every level.
    #[test]
    fn box_projection_samples_the_face_toward_the_proxy_hit() {
        use crate::settings::{self, Settings};
        use std::f32::consts::FRAC_PI_2;
        let Some((device, queue)) = crate::test_support::device() else {
            return;
        };
        // The probe frame turns world (x, y, z) about `origin` into local
        // (-y, x, z); local (u, v, w) is world origin + (v, -u, w).
        let origin = Vec3::new(10., -4., 3.);
        let world_to_local = Mat4::from_rotation_z(FRAC_PI_2) * Mat4::from_translation(-origin);
        let local_to_world = world_to_local.inverse();
        let texels: Vec<u16> = (0..LEVELS)
            .flat_map(|level| {
                (0..6u16).flat_map(move |face| {
                    let value = crate::test_support::to_half(f32::from(face + 1));
                    [value, 0, 0, 0x3c00].repeat((64usize >> level).pow(2))
                })
            })
            .collect();
        // The capture at local (0.9, 0, 0), inside a proxy of half-width
        // `half`.
        let probe = |half: f32| BakedSpecularProbe {
            center: local_to_world.transform_point3(Vec3::new(0.9, 0., 0.)),
            world_to_local,
            influence: SpecularProbeBox {
                min: Vec3::splat(-1.),
                max: Vec3::splat(1.),
            },
            blend: Vec3::ZERO,
            proxy: Some(SpecularProbeBox {
                min: Vec3::splat(-half),
                max: Vec3::splat(half),
            }),
            radiance: SpecularProbeRadiance {
                face_size: 64,
                texels: SpecularProbeTexels::Rgba16Float(texels.clone()),
            },
        };
        let mut scene = Scene::new(&device, &queue);
        scene
            .set_baked_specular_probes(
                &device,
                &queue,
                &[probe(1.), probe(1e20), probe(f32::MAX / 2.)],
            )
            .unwrap();
        let receiver = origin + Vec3::new(0.2, 0.1, -0.3);
        // (probe, receiver, world direction, expected face value).
        let mut cases = vec![
            // From the frame's origin along local (1, 0.3, 0), world -Y, the
            // ray meets the unit proxy at local (1, 0.3, 0), which lies along
            // local (0.1, 0.3, 0), world +X, from the capture.
            (0, origin, Vec3::new(0.3, -1., 0.), 1.),
        ];
        for index in [1, 2] {
            for (direction, face) in [
                (Vec3::X, 1.),
                (Vec3::NEG_X, 2.),
                (Vec3::Y, 3.),
                (Vec3::NEG_Y, 4.),
                (Vec3::new(0.36, -0.8, 0.48), 4.),
                (Vec3::new(-0.9, 0.3, 0.2), 2.),
            ] {
                cases.push((index, receiver, direction, face));
            }
        }
        // From local (-0.49, -0.49, -0.49) f32::MAX along local (1, 0.95,
        // 0.9), world (0.95, -1, 0.9), the ray meets the f32::MAX / 2 proxy
        // on its local +X face, at about (1, 0.9, 0.8) of its half-width,
        // which lies along world -Y from the capture.
        cases.push((
            2,
            local_to_world.transform_point3(Vec3::splat(-0.49 * f32::MAX)),
            Vec3::new(0.95, -1., 0.9),
            4.,
        ));
        let vec3 = |v: Vec3| format!("vec3({:?},{:?},{:?})", v.x, v.y, v.z);
        let count = cases.len();
        let observation = format!(
            r#"@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 let probes=array<u32,{count}>({});
 let worlds=array<vec3<f32>,{count}>({});
 let directions=array<vec3<f32>,{count}>({});
 for (var i=0u;i<{count}u;i++) {{
  output[i]=vec4(probe_radiance(probes[i],worlds[i],directions[i],0.,environment_sampler),0.);
 }}
}}
"#,
            cases
                .iter()
                .map(|c| format!("{}u", c.0))
                .collect::<Vec<_>>()
                .join(","),
            cases
                .iter()
                .map(|c| vec3(c.1))
                .collect::<Vec<_>>()
                .join(","),
            cases
                .iter()
                .map(|c| vec3(c.2.normalize()))
                .collect::<Vec<_>>()
                .join(","),
        );
        let settings = Settings {
            antialiasing: settings::Antialiasing::Off,
            bloom: settings::Bloom::Off,
            ambient_occlusion: settings::AmbientOcclusionQuality::Off,
            screen_space_reflections: settings::ScreenSpaceReflections::Off,
            world_space_reflections: settings::WorldSpaceReflections::Off,
            dynamic_gi: settings::DynamicGiQuality::Off,
            ..Settings::default()
        };
        let mut renderer = crate::Renderer::for_test(&device, &queue, [16, 16], &settings);
        let input = crate::FrameInput::new(crate::Camera {
            view: Mat4::IDENTITY,
            projection: crate::perspective(1., 1., 0.1),
            eye: Vec3::ZERO,
        });
        let sampled = crate::test_support::observe_ray_hits(
            &device,
            &queue,
            &mut renderer,
            &mut scene,
            &input,
            &settings,
            &observation,
            count,
        );
        for ((index, _, direction, face), sample) in cases.iter().zip(&sampled) {
            assert!(
                (sample[0] - face).abs() < 0.01,
                "probe {index} toward {direction}: sampled {sample:?}, expected face {face}"
            );
        }
    }
}
