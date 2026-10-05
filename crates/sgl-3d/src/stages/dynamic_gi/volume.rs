//! The resources of one dynamic GI volume's probes and the layouts of the
//! groups that bind them.
use super::{ALLOCATION_BYTES, CONVERGENCE_BYTES, Key};
use crate::scene::dynamic_gi::InstalledVolume;
use crate::shading::dynamic_gi as layout;

/// The bind group layouts of the stage's own groups.
pub(super) struct Layouts {
    pub allocate: wgpu::BindGroupLayout,
    /// The trace's group 3, which `TracePaths` lays out for each path.
    pub trace: [wgpu::BindGroupLayoutEntry; 3],
    pub update: wgpu::BindGroupLayout,
}

impl Layouts {
    pub fn new(device: &wgpu::Device) -> Self {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let uniform = entry(
            0,
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
        );
        let storage = |binding, read_only| {
            entry(
                binding,
                wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
            )
        };
        let rays = |binding| {
            entry(
                binding,
                wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Uint,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            )
        };
        let written = |binding, format| {
            entry(
                binding,
                wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
            )
        };
        let layout = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            })
        };
        Self {
            allocate: layout(
                "dynamic GI allocation",
                &[
                    uniform,
                    storage(1, true),
                    storage(2, true),
                    storage(3, false),
                    storage(4, false),
                    written(5, wgpu::TextureFormat::Rg32Uint),
                    storage(6, false),
                    storage(7, true),
                    storage(8, false),
                ],
            ),
            trace: [
                uniform,
                rays(1),
                written(2, wgpu::TextureFormat::Rgba32Uint),
            ],
            update: layout(
                "dynamic GI blend",
                &[
                    uniform,
                    storage(1, true),
                    rays(2),
                    storage(3, false),
                    storage(4, false),
                    storage(5, false),
                    written(6, layout::FORMAT),
                    storage(7, true),
                    storage(8, false),
                ],
            ),
        }
    }
}

/// The resources of one volume's probes: the probe texture lit group 0
/// binds, the blends' history, and this frame's rays.
pub(super) struct Volume {
    pub key: Key,
    /// The placement the last frame that ran the probes gave them.
    pub installed: InstalledVolume,
    /// What that frame's probes followed (`Inputs`).
    pub inputs: Option<super::Inputs>,
    /// The most rays a probe traces, which the rays' textures hold.
    pub max_rays: u32,
    pub probes: wgpu::TextureView,
    /// Each irradiance texel's estimator.
    pub variance: wgpu::Buffer,
    /// Each depth texel's mean and mean square distance.
    pub depth_history: wgpu::Buffer,
    /// Each probe's offset, whether it has been blended, and its share of
    /// back faces, which classifies it.
    pub probe_states: wgpu::Buffer,
    /// Each probe's request and periods, then the rays it traces this
    /// frame beside its fixed rays.
    pub ray_counts: wgpu::Buffer,
    /// The probes that trace this frame, which the blends gather.
    pub traced_probes: wgpu::Buffer,
    /// The volume's variability (`DdgiConvergence` in update.wgsl).
    pub convergence: wgpu::Buffer,
    /// The trace's indirect dispatch and the frame's ray count.
    pub allocation: wgpu::Buffer,
    /// Made with the volume, and again for another most rays.
    pub rays: Option<Rays>,
}

/// A frame's rays: the list the allocation writes and the trace reads, and
/// the results the trace writes and the blends read, with the groups that
/// bind them.
pub(super) struct Rays {
    pub allocate: wgpu::BindGroup,
    /// The list the allocation writes and the trace reads, and the results
    /// the trace writes, which the trace's group 3 binds
    /// (`Layouts::trace`).
    pub list: wgpu::TextureView,
    pub results: wgpu::TextureView,
    pub update: wgpu::BindGroup,
}

fn buffer_entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

fn view_entry(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

fn storage_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some(label),
            size: size.max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        },
    )
}

/// How many probes a lattice of `probes` holds.
pub(super) fn probe_count(probes: [u32; 3]) -> u64 {
    probes.iter().map(|&n| u64::from(n)).product()
}

pub(super) fn texture(
    device: &wgpu::Device,
    label: &str,
    size: [u64; 2],
    format: wgpu::TextureFormat,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size[0] as u32,
                height: size[1] as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

impl Volume {
    /// Fresh probes for `key`, every one unblended: what a restart starts
    /// from, as Wicked clears its resources on its first frame.
    pub fn new(
        device: &wgpu::Device,
        layouts: &Layouts,
        uniform: (&wgpu::Buffer, &wgpu::Buffer),
        (key, installed): (Key, InstalledVolume),
        max_rays: u32,
    ) -> Self {
        let count = probe_count(installed.probes);
        let irradiance_texels = u64::from(layout::COLOR_RESOLUTION * layout::COLOR_RESOLUTION);
        let depth_texels = u64::from(layout::DEPTH_RESOLUTION * layout::DEPTH_RESOLUTION);
        let probes = texture(
            device,
            "dynamic GI probes",
            layout::texture_size(installed.probes),
            layout::FORMAT,
        );
        let variance = storage_buffer(
            device,
            "dynamic GI irradiance estimators",
            count * irradiance_texels * 6 * 4,
        );
        let depth_history =
            storage_buffer(device, "dynamic GI depth moments", count * depth_texels * 4);
        let probe_states = storage_buffer(device, "dynamic GI probe states", count * 16);
        let ray_counts = storage_buffer(device, "dynamic GI ray counts", count * 4);
        let traced_probes = storage_buffer(device, "dynamic GI traced probes", count * 4);
        let convergence = crate::counters::buffer(
            device,
            &wgpu::BufferDescriptor {
                label: Some("dynamic GI convergence"),
                size: CONVERGENCE_BYTES,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            },
        );
        let allocation = crate::counters::buffer(
            device,
            &wgpu::BufferDescriptor {
                label: Some("dynamic GI ray allocation"),
                size: ALLOCATION_BYTES,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::INDIRECT
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            },
        );
        let mut volume = Self {
            key,
            installed,
            inputs: None,
            max_rays,
            probes,
            variance,
            depth_history,
            probe_states,
            ray_counts,
            traced_probes,
            convergence,
            allocation,
            rays: None,
        };
        volume.rays = Some(volume.ray_groups(device, layouts, uniform, max_rays));
        volume
    }

    /// The rays' textures for at most `max_rays` a probe, and the groups
    /// that bind them with the volume's buffers: the results hold each
    /// probe's slots, and the list a frame's rays, at most its budget.
    pub fn ray_groups(
        &self,
        device: &wgpu::Device,
        layouts: &Layouts,
        (uniform, moving_bounds): (&wgpu::Buffer, &wgpu::Buffer),
        max_rays: u32,
    ) -> Rays {
        let slots = probe_count(self.installed.probes) * u64::from(layout::ray_stride(max_rays));
        let list = texture(
            device,
            "dynamic GI ray list",
            layout::ray_texture_size(slots.min(u64::from(super::budget(max_rays)))),
            wgpu::TextureFormat::Rg32Uint,
        );
        let results = texture(
            device,
            "dynamic GI ray results",
            layout::ray_texture_size(slots),
            wgpu::TextureFormat::Rgba32Uint,
        );
        let buffer = buffer_entry;
        let view = view_entry;
        let group = |label, layout, entries: &[wgpu::BindGroupEntry]| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries,
            })
        };
        Rays {
            allocate: group(
                "dynamic GI allocation",
                &layouts.allocate,
                &[
                    buffer(0, uniform),
                    buffer(1, &self.variance),
                    buffer(2, &self.probe_states),
                    buffer(3, &self.ray_counts),
                    buffer(4, &self.allocation),
                    view(5, &list),
                    buffer(6, &self.traced_probes),
                    buffer(7, moving_bounds),
                    buffer(8, &self.convergence),
                ],
            ),
            update: group(
                "dynamic GI blend",
                &layouts.update,
                &[
                    buffer(0, uniform),
                    buffer(1, &self.ray_counts),
                    view(2, &results),
                    buffer(3, &self.variance),
                    buffer(4, &self.depth_history),
                    buffer(5, &self.probe_states),
                    view(6, &self.probes),
                    buffer(7, &self.traced_probes),
                    buffer(8, &self.convergence),
                ],
            ),
            list,
            results,
        }
    }
}
