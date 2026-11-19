//! The fog's froxel volumes of one size: the two that alternate as the
//! frame's froxels and their history, the integrated volume (the frame's
//! one fog volume, which also holds the aerial slices while aerial
//! perspective is in effect), and the filter's and integration's groups
//! over them.
use super::FORMAT;
use crate::view::history::HistoryFrame;

/// The volumes of one froxel size.
pub(super) struct Volumes {
    /// The froxels' (1×1×1 while only aerial perspective runs: no froxel
    /// volume is allocated for it).
    pub size: [u32; 3],
    /// The integrated volume's: the froxels' across, or the aerial volume's
    /// without the fog, and every slice of the frame's fog volume
    /// (`FogLayout::slices`).
    pub integrated_size: [u32; 3],
    /// The froxels, alternating: a frame writes the one at its history frame
    /// count's parity and reprojects the other.
    pub scattering: [wgpu::TextureView; 2],
    /// The history frame count each of `scattering` holds.
    pub holds: Holds,
    /// Which of `scattering` the last integration read: the one its frame
    /// wrote, or with the filter the other, which holds them filtered.
    pub integrated_from: usize,
    /// Each column's integration.
    pub integrated: wgpu::TextureView,
    /// Injection into `scattering[i]` from the other, with the scene's fog
    /// volume buffer and the reached froxels' buffer they bind, until either
    /// is replaced.
    pub inject: Option<(wgpu::Buffer, wgpu::Buffer, [wgpu::BindGroup; 2])>,
    /// Integration of `scattering[i]`, composing the aerial volume it binds
    /// (the sky stage's, or a stand-in), until that is replaced.
    pub integrate: Option<(wgpu::TextureView, [wgpu::BindGroup; 2])>,
    /// The filter of `scattering[i]`: along x into `integrated`, then along
    /// y into the other of `scattering`, which the injection has reprojected,
    /// so the history it leaves is unfiltered.
    pub filter: [[wgpu::BindGroup; 2]; 2],
}

impl Volumes {
    /// Volumes of `size` froxels integrated into a volume of
    /// `integrated_size`, whose filter groups bind `uniform`.
    pub fn new(
        device: &wgpu::Device,
        filter_layout: &wgpu::BindGroupLayout,
        uniform: &wgpu::Buffer,
        (size, integrated_size): ([u32; 3], [u32; 3]),
    ) -> Self {
        let scattering = [
            volume(device, "fog froxels", size),
            volume(device, "fog froxels", size),
        ];
        let (integrated, filter) =
            integrated(device, filter_layout, uniform, &scattering, integrated_size);
        Self {
            size,
            integrated_size,
            scattering,
            holds: Holds::default(),
            integrated_from: 0,
            integrated,
            inject: None,
            integrate: None,
            filter,
        }
    }

    /// Makes the integrated volume at `integrated_size` anew, with the
    /// filter's and the integration's groups that bind it, keeping the
    /// froxels and their history: aerial perspective coming into or out of
    /// effect changes only the integrated volume's slices.
    pub fn resize_integrated(
        &mut self,
        device: &wgpu::Device,
        filter_layout: &wgpu::BindGroupLayout,
        uniform: &wgpu::Buffer,
        integrated_size: [u32; 3],
    ) {
        (self.integrated, self.filter) = integrated(
            device,
            filter_layout,
            uniform,
            &self.scattering,
            integrated_size,
        );
        self.integrated_size = integrated_size;
        self.integrate = None;
    }

    /// Makes the integration's groups over `aerial` (the aerial volume or a
    /// stand-in) and its `sampler`, binding `uniform`, unless they bind it
    /// already.
    pub fn bind_aerial(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        uniform: &wgpu::Buffer,
        (aerial, sampler): (&wgpu::TextureView, &wgpu::Sampler),
    ) {
        if self
            .integrate
            .as_ref()
            .is_none_or(|(bound, _)| bound != aerial)
        {
            let view = wgpu::BindingResource::TextureView;
            let groups = [0, 1].map(|index| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("volumetric fog integration"),
                    layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: uniform.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: view(&self.scattering[index]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: view(&self.integrated),
                        },
                        wgpu::BindGroupEntry {
                            binding: 10,
                            resource: view(aerial),
                        },
                        wgpu::BindGroupEntry {
                            binding: 11,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                    ],
                })
            });
            self.integrate = Some((aerial.clone(), groups));
        }
    }

    /// The integration's group of `scattering[index]`, once `bind_aerial`
    /// has made it.
    pub fn integrate_group(&self, index: usize) -> &wgpu::BindGroup {
        &self
            .integrate
            .as_ref()
            .expect("bind_aerial before integrating")
            .1[index]
    }
}

/// A volume of the fog's format and `size`.
fn volume(device: &wgpu::Device, label: &str, size: [u32; 3]) -> wgpu::TextureView {
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
}

/// An integrated volume of `size` and the filter's groups over it and
/// `scattering`, which bind `uniform`.
fn integrated(
    device: &wgpu::Device,
    filter_layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    scattering: &[wgpu::TextureView; 2],
    size: [u32; 3],
) -> (wgpu::TextureView, [[wgpu::BindGroup; 2]; 2]) {
    let integrated = volume(device, "integrated fog", size);
    let view = wgpu::BindingResource::TextureView;
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
    (integrated, filter)
}

/// The history frame count and covered length each froxel volume holds.
#[derive(Clone, Copy, Default)]
pub(super) struct Holds {
    frames: [Option<(u32, f32)>; 2],
    /// The injection destination, distinct from the filtered destination.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    last_injected: usize,
}

impl Holds {
    /// Follows a frame of `history`, with its fog's covered `length`, or
    /// none when fog does not run. A reset
    /// forgets both volumes, even in a frame without fog, so no later frame
    /// takes one from before it for its predecessor's. A frame that runs
    /// writes the volume at its count's parity: its index, and, when the
    /// other holds its predecessor's froxels, the length that volume's depth
    /// slices cover, through which the frame reprojects it. Any length
    /// reprojects: a froxel finds its world point in the predecessor's
    /// slices of their own length (`fog.wgsl` `inject`).
    pub fn frame(
        &mut self,
        history: &HistoryFrame,
        length: Option<f32>,
    ) -> Option<(usize, Option<f32>)> {
        if !history.valid {
            self.frames = [None; 2];
        }
        let length = length?;
        let index = (history.frames % 2) as usize;
        let previous = self.frames[1 - index]
            .filter(|&(frames, _)| frames == history.frames.wrapping_sub(1))
            .map(|(_, length)| length);
        self.frames[index] = Some((history.frames, length));
        #[cfg(all(test, not(target_arch = "wasm32")))]
        {
            self.last_injected = index;
        }
        Some((index, previous))
    }

    /// The filter overwrites the other volume too. A retry may follow a
    /// submission without finish_frame, so it cannot assume that volume
    /// still contains the preceding camera's history.
    pub fn filtered(&mut self, index: usize) {
        self.frames[1 - index] = self.frames[index];
    }

    /// The volume the last frame the fog ran in wrote.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn last(&self) -> usize {
        self.last_injected
    }
}

#[cfg(test)]
mod tests {
    use super::Holds;
    use crate::view::history::{CameraFrame, CameraHistory};
    use glam::{DVec3, Mat4};
    use wasm_bindgen_test::wasm_bindgen_test;

    // Defect: a reset in a frame without fog leaves the volumes' holds, so
    // fog returning at the count after one it ran at before the reset
    // reprojects froxels from before it (sgl#570). The oracle is the camera
    // history's contract: nothing from before a reset is reused, and a
    // frame reprojects its predecessor's froxels within one history.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_reset_without_fog_ends_the_froxels_history() {
        let camera = CameraFrame {
            view: Mat4::IDENTITY,
            projection: Mat4::IDENTITY,
            jitter: [0.; 2],
        };
        let mut history = CameraHistory::default();
        let mut holds = Holds::default();
        // Each frame's reset, whether it has fog, and whether it reprojects
        // (none where the fog does not run).
        let frames = [
            (true, true, Some(false)),
            (false, true, Some(true)),
            (true, false, None),
            (false, true, Some(false)),
            (false, true, Some(true)),
        ];
        for (frame, (reset, fog, expected)) in frames.into_iter().enumerate() {
            let begun = history.begin(camera, reset, DVec3::ZERO, 0., 0);
            let reprojects = holds
                .frame(&begun, fog.then_some(64.))
                .map(|(_, previous)| previous.is_some());
            assert_eq!(
                reprojects, expected,
                "frame {frame}, count {}",
                begun.frames
            );
            history.finish(begun);
        }
    }
}
