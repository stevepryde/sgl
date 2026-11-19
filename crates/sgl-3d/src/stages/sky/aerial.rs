//! The aerial volume (`aerial_view_lut.wgsl`): Hillaire 2020's camera volume
//! as Bevy 9d12036 ports it, 32×32×32 RGBA16F over the camera's frustum,
//! each texel the in-scattered radiance (rgb) and mean transmittance (a)
//! between the camera and its slice's end along its column's ray, slices
//! spaced by the square to 32 km (`shading/sky.wgsl`'s mapping). It follows
//! the camera, so it is computed whole every frame aerial perspective is in
//! effect and keeps no history; the fog's integration composes it into the
//! frame's one fog volume (the architecture's Fog). It reads the stage's
//! uniform and its transmittance and multiple-scattering tables, never adds
//! to their inputs: the tables and the environment atlas still depend on the
//! camera's height alone.
use crate::shading::sky::{AERIAL_SIDE, AERIAL_SLICES};
use glam::Mat4;

/// The pass.
pub(crate) static AERIAL_VIEW: crate::shading::Module = crate::shading::Module {
    name: "sky_aerial_view",
    source: include_str!("aerial_view_lut.wgsl"),
    deps: &[&super::SKY_COMMON, &crate::shading::FOG],
};
/// The entry point its pipeline is created with.
pub(crate) const AERIAL_VIEW_ENTRY: &str = "aerial_view_lut_main";
/// Its workgroup's side along x and y.
const WORKGROUP: u32 = 8;

/// `SkyAerial` in `aerial_view_lut.wgsl`: the camera the volume lies over
/// and the game's strength.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SkyAerialUniform {
    pub world_from_view: [[f32; 4]; 4],
    /// The unjittered projection, as `shading::fog::froxel_projection`
    /// packs it.
    pub projection: [f32; 4],
    pub strength: f32,
    /// WGSL rounds `SkyAerial` up to its 16-byte alignment.
    pub padding: [f32; 3],
}

impl SkyAerialUniform {
    /// The volume over a camera of `view` and unjittered `projection`, at the
    /// game's `strength`.
    pub fn new(view: Mat4, projection: Mat4, strength: f32) -> Self {
        Self {
            world_from_view: view.inverse().to_cols_array_2d(),
            projection: crate::shading::fog::froxel_projection(projection),
            strength,
            padding: [0.; 3],
        }
    }
}

/// The pass's pipeline and layout.
pub(super) struct AerialPipeline {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl AerialPipeline {
    pub fn new(device: &wgpu::Device) -> Self {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let uniform = |binding, size| {
            entry(
                binding,
                wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(size as u64),
                },
            )
        };
        let table = |binding| {
            entry(
                binding,
                wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            )
        };
        // Two sampled tables and one storage volume, as the sky view.
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aerial perspective"),
            entries: &[
                uniform(0, size_of::<super::SkyUniform>()),
                table(1),
                table(2),
                entry(
                    3,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
                uniform(9, size_of::<SkyAerialUniform>()),
                entry(
                    10,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: super::FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D3,
                    },
                ),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(AERIAL_VIEW.name),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&AERIAL_VIEW]).into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(AERIAL_VIEW.name),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        Self {
            pipeline: device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(AERIAL_VIEW.name),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(AERIAL_VIEW_ENTRY),
                compilation_options: Default::default(),
                cache: None,
            }),
            layout,
        }
    }
}

/// The volume, its uniform and its pass's group.
pub(super) struct AerialVolume {
    #[cfg_attr(not(test), allow(dead_code))]
    texture: wgpu::Texture,
    /// The volume as the fog's integration samples it.
    pub view: wgpu::TextureView,
    uniform: wgpu::Buffer,
    group: wgpu::BindGroup,
}

impl AerialVolume {
    /// The volume marched through the stage's `tables` (transmittance and
    /// multiple scattering) and `sky` uniform, sampled through `sampler`.
    pub fn new(
        device: &wgpu::Device,
        pipeline: &AerialPipeline,
        (sky, tables, sampler): (&wgpu::Buffer, [&wgpu::TextureView; 2], &wgpu::Sampler),
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerial perspective volume"),
            size: wgpu::Extent3d {
                width: AERIAL_SIDE,
                height: AERIAL_SIDE,
                depth_or_array_layers: AERIAL_SLICES,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: super::FORMAT,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let uniform = crate::counters::buffer(
            device,
            &wgpu::BufferDescriptor {
                label: Some("aerial perspective"),
                size: size_of::<SkyAerialUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            },
        );
        let texture_entry = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aerial perspective"),
            layout: &pipeline.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: sky.as_entire_binding(),
                },
                texture_entry(1, tables[0]),
                texture_entry(2, tables[1]),
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: uniform.as_entire_binding(),
                },
                texture_entry(10, &view),
            ],
        });
        Self {
            texture,
            view,
            uniform,
            group,
        }
    }

    /// Computes the volume over the camera `aerial` describes, timed as
    /// `aerial perspective`.
    pub fn encode(
        &self,
        pipeline: &AerialPipeline,
        (queue, encoder): (&wgpu::Queue, &mut wgpu::CommandEncoder),
        aerial: &SkyAerialUniform,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        crate::counters::write_buffer(queue, &self.uniform, 0, bytemuck::bytes_of(aerial));
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("aerial perspective"),
            timestamp_writes: timing.and_then(|timing| timing.compute_pass("aerial perspective")),
        });
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &self.group, &[]);
        let side = AERIAL_SIDE.div_ceil(WORKGROUP);
        pass.dispatch_workgroups(side, side, 1);
    }

    /// The volume's texture, which tests read back.
    #[cfg(test)]
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}

/// The layout this module mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "sky_aerial_view",
        "SkyAerial",
        SkyAerialUniform,
        [world_from_view, projection, strength]
    )]
}
