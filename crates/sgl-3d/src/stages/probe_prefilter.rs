//! A specular probe capture's cube and its GGX prefilter: the six HDR faces
//! the capture's opaque pass renders, a solid-angle mip chain over them, and
//! a cube with the perceptual roughness `specular_probe_levels.wgsl` assigns
//! each mip, read back for the caller. Used only by
//! `Renderer::capture_specular_probe`.
//!
//! Reads: the captured faces. Writes: its own cube, mip chain and
//! prefiltered cube. Honours: nothing. Timing groups: none (authoring).
use crate::baked_specular_probe::{LEVELS, ProbeError};
use crate::scene::probes::payload_size;
use crate::view::targets::CaptureFace;

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
    /// The rendered faces with a solid-angle mip chain for filtered importance
    /// sampling.
    cube: wgpu::TextureView,
    cube_mips: Vec<wgpu::TextureView>,
    faces: [wgpu::TextureView; 6],
    motion: wgpu::TextureView,
    depth: wgpu::TextureView,
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

impl ProbePrefilter {
    /// `face_size` is a power of two of at least 2^(LEVELS - 1).
    pub fn new(device: &wgpu::Device, face_size: u32) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("specular probe GGX prefilter"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&PREFILTER]).into()),
        });
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC;
        let mip_count = face_size.ilog2() + 1;
        let cube = image(
            device,
            face_size,
            6,
            mip_count,
            crate::shading::gbuffer::COLOR,
            usage | wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_DST,
            "specular probe capture cube",
        );
        let view = |texture: &wgpu::Texture, dimension, level, layer: Option<u32>| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(dimension),
                base_mip_level: level,
                mip_level_count: Some(1),
                base_array_layer: layer.unwrap_or(0),
                array_layer_count: layer.map(|_| 1),
                ..Default::default()
            })
        };
        let auxiliary = |format, label| {
            image(device, face_size, 1, 1, format, usage, label).create_view(&Default::default())
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
            cube: cube.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::Cube),
                ..Default::default()
            }),
            cube_mips: (0..mip_count)
                .map(|level| view(&cube, wgpu::TextureViewDimension::D2Array, level, None))
                .collect(),
            faces: std::array::from_fn(|i| {
                view(&cube, wgpu::TextureViewDimension::D2, 0, Some(i as u32))
            }),
            motion: auxiliary(
                crate::shading::gbuffer::MOTION,
                "specular probe unused motion attachment",
            ),
            depth: auxiliary(crate::shading::gbuffer::DEPTH, "specular probe depth"),
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

    /// The attachments of face `index` (0..6).
    pub fn face(&self, index: usize) -> CaptureFace {
        CaptureFace {
            color: self.faces[index].clone(),
            motion: self.motion.clone(),
            depth: self.depth.clone(),
        }
    }

    /// After all six faces: the solid-angle mip chain, then every GGX level.
    pub fn encode(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        for level in 1..self.cube_mips.len() {
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("specular probe adjacent mips"),
                layout: &self.mip_pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&self.cube_mips[level - 1]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&self.cube_mips[level]),
                    },
                ],
            });
            let size = (self.face_size >> level).max(1);
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.mip_pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(size.div_ceil(8), size.div_ceil(8), 6);
        }
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
            let mapped = readback.get_mapped_range(..);
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
