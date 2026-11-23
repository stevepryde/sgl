//! The tile passes of the shared `motion_tiles` module
//! (`shading::motion_tiles`) over a motion target: their pipelines, made
//! from a program that composes the module, and their targets at one frame
//! size. Motion blur and TAA's motion reach each hold their own, run at
//! their own size and place in the frame (Frame items 11 and 14).
use crate::shading::gbuffer::COLOR;
use crate::shading::motion_tiles::{ENTRY_POINTS, GROUP_SIZE, MotionTilesUniform, TILE_SIZE};
use crate::view::targets::target_with_usage;

/// The tile targets for one frame size.
struct Targets {
    size: [u32; 2],
    /// Each tile column's largest and smallest velocity per frame row.
    horizontal: wgpu::TextureView,
    /// Each tile's.
    tiles: wgpu::TextureView,
    /// Each tile neighbourhood's.
    neighborhood: wgpu::TextureView,
}

impl Targets {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let tiles = size.map(|v| v.div_ceil(TILE_SIZE));
        let target = |label, size| {
            target_with_usage(
                device,
                label,
                size,
                COLOR,
                wgpu::TextureUsages::STORAGE_BINDING,
            )
        };
        Self {
            size,
            horizontal: target("motion tile columns", [tiles[0], size[1]]),
            tiles: target("motion tiles", tiles),
            neighborhood: target("motion tile neighbourhoods", tiles),
        }
    }
}

pub(crate) struct MotionTiles {
    pipelines: [wgpu::ComputePipeline; 3],
    uniform: wgpu::Buffer,
    targets: Option<Targets>,
    /// Each pass's group for a motion target and a frame, until the targets
    /// or the views are replaced.
    groups: Vec<(wgpu::TextureView, wgpu::TextureView, [wgpu::BindGroup; 3])>,
}

impl MotionTiles {
    /// The tile passes of `module`, a program composing `motion_tiles`.
    pub fn new(device: &wgpu::Device, module: &wgpu::ShaderModule) -> Self {
        let pipelines = ENTRY_POINTS.map(|entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &[("MOTIONBLUR_TILESIZE", f64::from(TILE_SIZE))],
                    ..Default::default()
                },
                cache: None,
            })
        });
        Self {
            pipelines,
            uniform: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("motion tiles"),
                    size: size_of::<MotionTilesUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            targets: None,
            groups: Vec::new(),
        }
    }

    /// The scales the passes read (`MotionTiles`), which a program's other
    /// passes may bind too.
    pub fn uniform(&self) -> &wgpu::Buffer {
        &self.uniform
    }

    /// Drops the bind groups, which hold views a resize or a new
    /// antialiasing context replaces.
    pub fn forget_inputs(&mut self) {
        self.groups.clear();
    }

    /// How many sets of the passes' groups it keeps, one for each motion
    /// target and frame bound since the targets or the views were replaced.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn cached_groups(&self) -> usize {
        self.groups.len()
    }

    /// Writes `values`, then, in `pass`, tiles `motion` for `frame`, whose
    /// size the tiles cover. Returns each tile neighbourhood's largest
    /// velocity (xy) and smallest (zw), in `TILE_SIZE` tiles of `frame`.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::ComputePass<'_>,
        [motion, frame]: [&wgpu::TextureView; 2],
        values: MotionTilesUniform,
    ) -> &wgpu::TextureView {
        crate::counters::write_buffer(queue, &self.uniform, 0, bytemuck::bytes_of(&values));
        let size = frame.texture().size();
        let size = [size.width, size.height];
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            self.targets = Some(Targets::new(device, size));
            self.groups.clear();
        }
        let groups = self.groups(device, motion, frame);
        let tiles = size.map(|v| v.div_ceil(TILE_SIZE));
        let dispatches = [[tiles[0], size[1]], tiles, tiles];
        for ((pipeline, group), extent) in self.pipelines.iter().zip(&groups).zip(dispatches) {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(
                extent[0].div_ceil(GROUP_SIZE),
                extent[1].div_ceil(GROUP_SIZE),
                1,
            );
        }
        &self.targets.as_ref().unwrap().neighborhood
    }

    /// Each pass's group for `motion` and `frame`, made once while they are
    /// the views tiled.
    fn groups(
        &mut self,
        device: &wgpu::Device,
        motion: &wgpu::TextureView,
        frame: &wgpu::TextureView,
    ) -> [wgpu::BindGroup; 3] {
        if let Some((.., groups)) = self
            .groups
            .iter()
            .find(|(m, f, _)| m == motion && f == frame)
        {
            return groups.clone();
        }
        let targets = self.targets.as_ref().unwrap();
        let view = wgpu::BindingResource::TextureView;
        let entry = |binding, resource| wgpu::BindGroupEntry { binding, resource };
        let entries = [
            vec![
                entry(0, self.uniform.as_entire_binding()),
                entry(1, view(motion)),
                entry(3, view(frame)),
                entry(5, view(&targets.horizontal)),
            ],
            vec![
                entry(4, view(&targets.horizontal)),
                entry(5, view(&targets.tiles)),
            ],
            vec![
                entry(4, view(&targets.tiles)),
                entry(5, view(&targets.neighborhood)),
            ],
        ];
        let mut entries = entries.into_iter();
        let groups = self.pipelines.each_ref().map(|pipeline| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("motion tiles"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &entries.next().unwrap(),
            })
        });
        self.groups
            .push((motion.clone(), frame.clone(), groups.clone()));
        groups
    }
}
