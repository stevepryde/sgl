//! The fog's froxel volumes of one size: the two that alternate as the
//! frame's froxels and their history, the integrated volume, and the
//! filter's and integration's groups over them.
use super::FORMAT;
use crate::view::history::HistoryFrame;

/// The volumes of one froxel size.
pub(super) struct Volumes {
    pub size: [u32; 3],
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
            holds: Holds::default(),
            integrated_from: 0,
            integrated,
            inject: None,
            integrate,
            filter,
        }
    }
}

/// The history frame count each of a pair of froxel volumes holds.
#[derive(Clone, Copy, Default)]
pub(super) struct Holds {
    frames: [Option<u32>; 2],
    /// The injection destination, distinct from the filtered destination.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    last_injected: usize,
}

impl Holds {
    /// Follows a frame of `history`, which the fog `runs` in or not. A reset
    /// forgets both volumes, even in a frame without fog, so no later frame
    /// takes one from before it for its predecessor's. A frame that runs
    /// writes the volume at its count's parity: its index, and whether it
    /// reprojects the other, which then holds its predecessor's froxels.
    pub fn frame(&mut self, history: &HistoryFrame, runs: bool) -> Option<(usize, bool)> {
        if !history.valid {
            self.frames = [None; 2];
        }
        if !runs {
            return None;
        }
        let index = (history.frames % 2) as usize;
        let reprojects = self.frames[1 - index] == Some(history.frames.wrapping_sub(1));
        self.frames[index] = Some(history.frames);
        #[cfg(all(test, not(target_arch = "wasm32")))]
        {
            self.last_injected = index;
        }
        Some((index, reprojects))
    }

    /// The filter overwrites the other volume too. A retry may follow a
    /// submission without finish_frame, so it cannot assume that volume
    /// still contains the preceding camera's history.
    pub fn filtered(&mut self, index: usize, frame: u32) {
        self.frames[1 - index] = Some(frame);
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
            let begun = history.begin(camera, reset, DVec3::ZERO, 0.);
            let reprojects = holds.frame(&begun, fog).map(|(_, reprojects)| reprojects);
            assert_eq!(
                reprojects, expected,
                "frame {frame}, count {}",
                begun.frames
            );
            history.finish(begun);
        }
    }
}
