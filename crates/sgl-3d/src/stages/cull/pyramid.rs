//! The camera's depth pyramid, the cull stage's history (the architecture's
//! "GPU draw lists and occlusion culling"): R32Float levels whose texels
//! hold the farthest opaque depth they cover, built from the opaque depth by
//! Bevy 9d12036's port of AMD's single-pass downsampler (pyramid.wgsl; the
//! sizes and dispatches of `ViewDepthPyramid`, crates/bevy_core_pipeline/
//! src/mip_generation/experimental/depth.rs 557-612 and 683-698). Two
//! layouts of six storage textures each and a uniform, so a device needs six
//! storage textures a stage for it (`SUPPORTED_STORAGE_TEXTURES`).

pub(crate) static PYRAMID: crate::shading::Module = crate::shading::Module {
    name: "depth_pyramid",
    source: include_str!("pyramid.wgsl"),
    deps: &[],
};
/// The entry points the pyramid's pipelines are created with.
pub(crate) const DOWNSAMPLE_DEPTH_FIRST_ENTRY: &str = "downsample_depth_first";
pub(crate) const DOWNSAMPLE_DEPTH_SECOND_ENTRY: &str = "downsample_depth_second";

/// The storage textures a stage binds to build the pyramid: a device with
/// fewer culls by frustum alone.
pub(crate) const SUPPORTED_STORAGE_TEXTURES: u32 = 6;
/// The levels the two passes write: Bevy's `DEPTH_PYRAMID_MIP_COUNT`, whose
/// first level is at most 2048 texels a side (a render size of at most
/// 4095 pixels a side).
const MOST_LEVELS: u32 = 12;
/// The levels the first pass writes; the second writes the rest from the
/// last of them.
const FIRST_PASS_LEVELS: u32 = 6;
/// The first pass's virtual source texels a workgroup a side.
const FIRST_PASS_TEXELS: u32 = 64;

/// The size of the first level of a pyramid over a depth of `size`: each
/// side rounded down to a power of two, as Bevy's `previous_power_of_two`.
pub(crate) fn first_level(size: [u32; 2]) -> [u32; 2] {
    size.map(|side| 1 << (31 - side.max(1).leading_zeros()))
}

/// The levels of a pyramid whose first level is `first`: down to one texel
/// on its longer side.
fn levels(first: [u32; 2]) -> u32 {
    32 - first[0].max(first[1]).leading_zeros()
}

/// The levels the two passes build whole: all of them while the first is
/// at most 2048 texels a side, else only the first pass's, since the second
/// reduces 64×64 texels of its last level.
fn complete_levels(first: [u32; 2]) -> u32 {
    let levels = levels(first);
    if levels <= MOST_LEVELS {
        levels
    } else {
        FIRST_PASS_LEVELS
    }
}

/// A pyramid over a depth of one render size.
pub(crate) struct Pyramid {
    /// The render size it was made for.
    size: [u32; 2],
    /// Every level, which the cull samples.
    all: wgpu::TextureView,
    /// Each level the passes write, at most `MOST_LEVELS`.
    mips: Vec<wgpu::TextureView>,
    /// The levels the cull may read: those the passes build whole.
    complete: u32,
    /// `PyramidConstants`: the levels the passes write.
    constants: wgpu::Buffer,
    /// The passes' groups over the depth they were made for.
    groups: Option<(wgpu::TextureView, [wgpu::BindGroup; 2])>,
}

impl Pyramid {
    /// Every level, as the cull binds it.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.all
    }

    /// The levels the occlusion test may read.
    pub fn complete(&self) -> u32 {
        self.complete
    }

    /// The pyramid's texture, which tests read back.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn texture(&self) -> &wgpu::Texture {
        self.all.texture()
    }
}

/// The downsample's pipelines and what every pyramid's groups share.
pub(crate) struct Builder {
    layouts: [wgpu::BindGroupLayout; 2],
    pipelines: [wgpu::ComputePipeline; 2],
    sampler: wgpu::Sampler,
    /// A 1×1 R32Float storage view bound for the levels a small pyramid
    /// does not have, as Bevy binds its dummy texture; never written, since
    /// the passes stop at the pyramid's levels.
    dummy: wgpu::TextureView,
}

impl Builder {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("depth pyramid"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&PYRAMID]).into()),
        });
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: wgpu::TextureFormat::R32Float,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let constants = wgpu::BindGroupLayoutEntry {
            binding: 8,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let texture = |binding, sample_type| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let mut first: Vec<_> = (1..=6).map(storage).collect();
        first.extend([
            texture(0, wgpu::TextureSampleType::Depth),
            wgpu::BindGroupLayoutEntry {
                binding: 7,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                count: None,
            },
            constants,
        ]);
        let mut second: Vec<_> = (10..=15).map(storage).collect();
        second.extend([
            texture(9, wgpu::TextureSampleType::Float { filterable: false }),
            constants,
        ]);
        let layouts = [first, second].map(|entries| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("depth pyramid"),
                entries: &entries,
            })
        });
        let pipeline = |layout: &wgpu::BindGroupLayout, entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some(entry),
                        bind_group_layouts: &[Some(layout)],
                        immediate_size: 0,
                    }),
                ),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let pipelines = [
            pipeline(&layouts[0], DOWNSAMPLE_DEPTH_FIRST_ENTRY),
            pipeline(&layouts[1], DOWNSAMPLE_DEPTH_SECOND_ENTRY),
        ];
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("depth pyramid source"),
            ..Default::default()
        });
        let dummy = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("depth pyramid stand-in level"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Float,
                usage: wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        Self {
            layouts,
            pipelines,
            sampler,
            dummy,
        }
    }

    /// A pyramid over a depth of `size`.
    pub fn pyramid(&self, device: &wgpu::Device, size: [u32; 2]) -> Pyramid {
        let first = first_level(size);
        let levels = levels(first);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth pyramid"),
            size: wgpu::Extent3d {
                width: first[0],
                height: first[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let mips = (0..levels.min(MOST_LEVELS))
            .map(|level| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("depth pyramid level"),
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let constants = crate::counters::buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("depth pyramid constants"),
                contents: bytemuck::cast_slice(&[levels.min(MOST_LEVELS), 0, 0, 0]),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        Pyramid {
            size,
            all: texture.create_view(&Default::default()),
            mips,
            complete: complete_levels(first),
            constants,
            groups: None,
        }
    }

    /// Whether `pyramid` is for a depth of `size`.
    pub fn fits(pyramid: &Pyramid, size: [u32; 2]) -> bool {
        pyramid.size == size
    }

    /// Builds `pyramid` from `depth` in `encoder`: the first pass, a
    /// workgroup per 64×64 texels of the virtual source, then, where the
    /// pyramid has more levels than the first pass writes, the second.
    pub fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pyramid: &mut Pyramid,
        depth: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        if pyramid
            .groups
            .as_ref()
            .is_none_or(|(bound, _)| bound != depth)
        {
            let groups = self.groups(device, pyramid, depth);
            pyramid.groups = Some((depth.clone(), groups));
        }
        let groups = &pyramid.groups.as_ref().unwrap().1;
        let levels = pyramid.mips.len() as u32;
        // Bevy's virtual source: each side the next power of two above it.
        let virtual_size = pyramid.size.map(|side| (side + 1).next_power_of_two());
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("depth pyramid"),
            timestamp_writes: timing.and_then(|timing| timing.compute_pass("depth pyramid")),
        });
        pass.set_pipeline(&self.pipelines[0]);
        pass.set_bind_group(0, &groups[0], &[]);
        pass.dispatch_workgroups(
            virtual_size[0].div_ceil(FIRST_PASS_TEXELS),
            virtual_size[1].div_ceil(FIRST_PASS_TEXELS),
            1,
        );
        if levels > FIRST_PASS_LEVELS {
            pass.set_pipeline(&self.pipelines[1]);
            pass.set_bind_group(0, &groups[1], &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
    }

    /// The two passes' groups over `pyramid`'s levels and `depth`.
    fn groups(
        &self,
        device: &wgpu::Device,
        pyramid: &Pyramid,
        depth: &wgpu::TextureView,
    ) -> [wgpu::BindGroup; 2] {
        let mip = |level: usize| pyramid.mips.get(level).unwrap_or(&self.dummy);
        let view = wgpu::BindingResource::TextureView;
        let mut first: Vec<_> = (0..6)
            .map(|level| wgpu::BindGroupEntry {
                binding: level as u32 + 1,
                resource: view(mip(level)),
            })
            .collect();
        first.extend([
            wgpu::BindGroupEntry {
                binding: 0,
                resource: view(depth),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: wgpu::BindingResource::Sampler(&self.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: pyramid.constants.as_entire_binding(),
            },
        ]);
        // The second pass samples the first's last level; a pyramid
        // without it never runs the second pass, and binds every level.
        let sampled = pyramid.mips.get(5).unwrap_or(&pyramid.all);
        let mut second: Vec<_> = (0..6)
            .map(|level| wgpu::BindGroupEntry {
                binding: level as u32 + 10,
                resource: view(mip(level + 6)),
            })
            .collect();
        second.extend([
            wgpu::BindGroupEntry {
                binding: 9,
                resource: view(sampled),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: pyramid.constants.as_entire_binding(),
            },
        ]);
        [(&self.layouts[0], first), (&self.layouts[1], second)].map(|(layout, entries)| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("depth pyramid"),
                layout,
                entries: &entries,
            })
        })
    }
}
