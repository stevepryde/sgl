//! The fog's froxel volumes of one size: the two that alternate as the
//! frame's froxels and their history, the integrated volume, and the
//! filter's and integration's groups over them.
use super::FORMAT;

/// The volumes of one froxel size.
pub(super) struct Volumes {
    pub size: [u32; 3],
    /// The froxels, alternating: a frame writes the one at its history frame
    /// count's parity and reprojects the other.
    pub scattering: [wgpu::TextureView; 2],
    /// The history frame count each of `scattering` holds.
    pub holds: [Option<u32>; 2],
    /// Which of `scattering` the last integration read: the one its frame
    /// wrote, or with the filter the other, which holds them filtered.
    pub integrated_from: usize,
    /// Each column's integration.
    pub integrated: wgpu::TextureView,
    /// Injection into `scattering[i]` from the other, with the scene's fog
    /// volume buffer and the reached froxels' buffer they bind, until either
    /// is replaced.
    pub inject: Option<(wgpu::Buffer, wgpu::Buffer, [wgpu::BindGroup; 2])>,
    /// Integration of `scattering[i]`.
    pub integrate: [wgpu::BindGroup; 2],
    /// The filter of `scattering[i]`: along x into `integrated`, then along
    /// y into the other of `scattering`, which the injection has reprojected,
    /// so the history it leaves is unfiltered.
    pub filter: [[wgpu::BindGroup; 2]; 2],
}

impl Volumes {
    /// Volumes of `size` froxels, whose filter and integration groups bind
    /// `uniform`.
    pub fn new(
        device: &wgpu::Device,
        filter_layout: &wgpu::BindGroupLayout,
        integrate_layout: &wgpu::BindGroupLayout,
        uniform: &wgpu::Buffer,
        size: [u32; 3],
    ) -> Self {
        let volume = |label| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: size[2],
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D3,
                    format: FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let scattering = [volume("fog froxels"), volume("fog froxels")];
        let integrated = volume("integrated fog");
        let view = wgpu::BindingResource::TextureView;
        let integrate = [0, 1].map(|index| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("volumetric fog integration"),
                layout: integrate_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: view(&scattering[index]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: view(&integrated),
                    },
                ],
            })
        });
        let filter_pass = |source, dest| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("volumetric fog filter"),
                layout: filter_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: view(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 8,
                        resource: view(dest),
                    },
                ],
            })
        };
        let filter = [0, 1].map(|index| {
            [
                filter_pass(&scattering[index], &integrated),
                filter_pass(&integrated, &scattering[1 - index]),
            ]
        });
        Self {
            size,
            scattering,
            holds: [None; 2],
            integrated_from: 0,
            integrated,
            inject: None,
            integrate,
            filter,
        }
    }
}
