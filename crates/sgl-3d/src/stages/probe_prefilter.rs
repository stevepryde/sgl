//! A specular probe capture's cube and its GGX prefilter: the HDR face the
//! capture's opaque pass renders, one face at a time, averaged into the
//! cube's face; a solid-angle mip chain over the six faces; and a cube with
//! the perceptual roughness `specular_probe_levels.wgsl` assigns each mip,
//! read back for the caller. Used only by `Renderer::capture_specular_probe`.
//!
//! Each face renders at `CAPTURE_SIZE` texels a side, or at the probe's face
//! size if that is larger, and a uniform box (the 2x2 `mip_reduce` chain)
//! averages it to the face size, so the cube's mip 0 holds each texel's
//! area-averaged radiance rather than the one sample at its centre: an
//! emitter thinner than a texel keeps its energy instead of filling the
//! texel. This follows Unreal's supersampled reflection captures
//! (`r.ReflectionCaptureSupersampleFactor`; practice only, no code) and
//! Filament cmgen's area-averaged base level and box mips (ef1a133
//! `libs/ibl/src/CubemapUtils.cpp`, `equirectangularToCubemap` and
//! `downsampleCubemapLevelBoxFilter`). Godot renders its probes at their
//! size with MSAA off; Wicked Engine can render them with 8x MSAA, which
//! needs a multisampled copy of every pipeline, shades once per texel and,
//! at WebGPU's 4 samples, still stores a thin strip at a quarter of a texel.
//! Departure (#263): a fixed angular resolution, not Unreal's fixed factor,
//! since a thin emitter's stored energy errs by the angular sample spacing
//! whatever the face size. An emitter's width is still quantised to whole
//! samples, 1/`CAPTURE_SIZE` of a face's width, which is unbiased over its
//! positions. The uniform box weighs a texel's samples equally, though
//! their solid angles differ, by under 1 % within a texel.
//!
//! Reads: the captured face. Writes: its own face target, cube, mip chain
//! and prefiltered cube. Honours: nothing. Timing groups: none (authoring).
use crate::baked_specular_probe::{LEVELS, ProbeError};
use crate::scene::probes::payload_size;
use crate::view::targets::CaptureFace;

/// The side, in texels, a capture face renders at least: each face renders
/// at this or the probe's face size, whichever is larger.
pub(crate) const CAPTURE_SIZE: u32 = 2048;

/// The side a capture of `face_size` renders each face at.
pub(crate) fn capture_size(face_size: u32) -> u32 {
    face_size.max(CAPTURE_SIZE)
}

/// The GGX prefilter of a captured cube.
pub(crate) static PREFILTER: crate::shading::Module = crate::shading::Module {
    name: "probe_prefilter",
    source: include_str!("probe_prefilter.wgsl"),
    deps: &[&crate::shading::SPECULAR_PROBE_LEVELS],
};
/// The entry points the prefilter's pipelines are created with.
pub(crate) const PREFILTER_ENTRY: &str = "prefilter";
pub(crate) const MIP_REDUCE_ENTRY: &str = "mip_reduce";

pub(crate) struct ProbePrefilter {
    face_size: u32,
    /// The face being captured, at the capture size, with a mip chain down
    /// to the face size.
    rendered: wgpu::Texture,
    rendered_mips: Vec<wgpu::TextureView>,
    motion: wgpu::TextureView,
    depth: wgpu::TextureView,
    /// The captured faces with a solid-angle mip chain for filtered
    /// importance sampling.
    cube: wgpu::TextureView,
    cube_mips: Vec<wgpu::TextureView>,
    /// The prefiltered cube, `LEVELS` mips.
    filtered: wgpu::Texture,
    pipeline: wgpu::ComputePipeline,
    mip_pipeline: wgpu::ComputePipeline,
    sampler: wgpu::Sampler,
}

fn image(
    device: &wgpu::Device,
    size: u32,
    layers: u32,
    mips: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: layers,
        },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

/// One single-level array view of each of `texture`'s `mips` levels.
fn mip_views(texture: &wgpu::Texture, mips: u32) -> Vec<wgpu::TextureView> {
    (0..mips)
        .map(|level| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        })
        .collect()
}

impl ProbePrefilter {
    /// `face_size` is a power of two of at least 2^(LEVELS - 1), and each
    /// face renders at `capture_size`, a power of two no smaller
    /// (`capture_size(face_size)` for a probe).
    pub fn new(device: &wgpu::Device, face_size: u32, capture_size: u32) -> Self {
        debug_assert!(capture_size >= face_size && capture_size.is_power_of_two());
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("specular probe GGX prefilter"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&PREFILTER]).into()),
        });
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC;
        let reduced = usage | wgpu::TextureUsages::STORAGE_BINDING;
        let rendered_mip_count = (capture_size / face_size).ilog2() + 1;
        let rendered = image(
            device,
            capture_size,
            1,
            rendered_mip_count,
            crate::shading::gbuffer::COLOR,
            reduced,
            "specular probe capture face",
        );
        let mip_count = face_size.ilog2() + 1;
        let cube = image(
            device,
            face_size,
            6,
            mip_count,
            crate::shading::gbuffer::COLOR,
            reduced | wgpu::TextureUsages::COPY_DST,
            "specular probe capture cube",
        );
        let auxiliary = |format, label| {
            image(device, capture_size, 1, 1, format, usage, label).create_view(&Default::default())
        };
        let compute = |entry_point| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry_point),
                layout: None,
                module: &shader,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            face_size,
            rendered_mips: mip_views(&rendered, rendered_mip_count),
            rendered,
            motion: auxiliary(
                crate::shading::gbuffer::MOTION,
                "specular probe unused motion attachment",
            ),
            depth: auxiliary(crate::shading::gbuffer::DEPTH, "specular probe depth"),
            cube: cube.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::Cube),
                ..Default::default()
            }),
            cube_mips: mip_views(&cube, mip_count),
            filtered: image(
                device,
                face_size,
                6,
                LEVELS,
                wgpu::TextureFormat::Rgba16Float,
                wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                "prefiltered specular probe",
            ),
            pipeline: compute(PREFILTER_ENTRY),
            mip_pipeline: compute(MIP_REDUCE_ENTRY),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("specular probe capture trilinear"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            }),
        }
    }

    /// The attachments a face renders into, at the capture size. Each face
    /// is resolved (`resolve_face`) before the next renders.
    pub fn face(&self) -> CaptureFace {
        CaptureFace {
            color: self.rendered.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                mip_level_count: Some(1),
                ..Default::default()
            }),
            motion: self.motion.clone(),
            depth: self.depth.clone(),
        }
    }

    /// After face `index` (0..6) renders: its uniform box average to the
    /// face size, into the cube's face.
    pub fn resolve_face(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        index: u32,
    ) {
        self.reduce(
            device,
            encoder,
            &self.rendered_mips,
            self.rendered.width(),
            1,
        );
        let level = self.rendered_mips.len() as u32 - 1;
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                mip_level: level,
                ..self.rendered.as_image_copy()
            },
            wgpu::TexelCopyTextureInfo {
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: index,
                },
                ..self.cube.texture().as_image_copy()
            },
            wgpu::Extent3d {
                width: self.face_size,
                height: self.face_size,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Each of `mips` after the first, the 2x2 mean of the one before:
    /// `layers` layers, `size` texels a side at the first.
    fn reduce(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        mips: &[wgpu::TextureView],
        size: u32,
        layers: u32,
    ) {
        for (level, pair) in (1u32..).zip(mips.windows(2)) {
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("specular probe adjacent mips"),
                layout: &self.mip_pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&pair[0]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&pair[1]),
                    },
                ],
            });
            let size = (size >> level).max(1);
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.mip_pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(size.div_ceil(8), size.div_ceil(8), layers);
        }
    }

    /// After all six faces: the solid-angle mip chain, then every GGX level.
    pub fn encode(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        self.reduce(device, encoder, &self.cube_mips, self.face_size, 6);
        for level in 0..LEVELS {
            let uniform = crate::counters::buffer_init(
                device,
                &wgpu::util::BufferInitDescriptor {
                    label: Some("specular probe level"),
                    contents: bytemuck::bytes_of(&level),
                    usage: wgpu::BufferUsages::UNIFORM,
                },
            );
            let target = self.filtered.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("specular probe GGX level"),
                layout: &self.pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&self.cube),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&target),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: uniform.as_entire_binding(),
                    },
                ],
            });
            let size = self.face_size >> level;
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(size.div_ceil(8), size.div_ceil(8), 6);
        }
    }

    /// Submits `encoder` and reads every level of the prefiltered cube
    /// back, in `SpecularProbeRadiance` order. Blocks for the device, which
    /// nothing else may poll meanwhile; in a browser, whose device cannot
    /// block, it fails with `Readback`.
    pub fn read(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mut encoder: wgpu::CommandEncoder,
    ) -> Result<Vec<u16>, ProbeError> {
        let face_size = self.face_size;
        let rows: Vec<_> = (0..LEVELS)
            .map(|level| {
                let size = face_size >> level;
                let row = (size * 8).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
                (size, row)
            })
            .collect();
        let bytes: u64 = rows
            .iter()
            .map(|&(size, row)| u64::from(row) * u64::from(size) * 6)
            .sum();
        let readback = crate::counters::buffer(
            device,
            &wgpu::BufferDescriptor {
                label: Some("static specular probe readback"),
                size: bytes,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            },
        );
        let mut offset = 0;
        for (level, &(size, row)) in rows.iter().enumerate() {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    mip_level: level as u32,
                    ..self.filtered.as_image_copy()
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset,
                        bytes_per_row: Some(row),
                        rows_per_image: Some(size),
                    },
                },
                wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 6,
                },
            );
            offset += u64::from(row) * u64::from(size) * 6;
        }
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        readback.map_async(wgpu::MapMode::Read, .., move |result| {
            let _ = tx.send(result);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| ProbeError::Readback(e.to_string()))?;
        // A device that waits maps the buffer within this `poll`, provided
        // nothing else polls the device meanwhile and takes the callback.
        // WebGPU's poll cannot wait: its mapping completes only after control
        // returns to the browser, so the readback is unavailable there.
        rx.try_recv()
            .map_err(|_| ProbeError::Readback("the readback was not ready after polling".into()))?
            .map_err(|e| ProbeError::Readback(e.to_string()))?;
        let mut rgba16 =
            Vec::with_capacity(payload_size(wgpu::TextureFormat::Rgba16Float, face_size) / 2);
        {
            let mapped = readback
                .get_mapped_range(..)
                .expect("mapped prefilter readback");
            let mut offset = 0;
            for &(size, row) in &rows {
                for line in
                    mapped[offset..offset + (row * size * 6) as usize].chunks_exact(row as usize)
                {
                    rgba16.extend(
                        line[..size as usize * 8]
                            .chunks_exact(2)
                            .map(|value| u16::from_le_bytes([value[0], value[1]])),
                    );
                }
                offset += (row * size * 6) as usize;
            }
        }
        readback.unmap();
        Ok(rgba16)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
