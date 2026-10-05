//! Motion blur: Wicked Engine's tile-max reconstruction filter
//! (`motion_blur.wgsl`), after Jimenez 2014 and McGuire et al. 2012. Each
//! pixel blurs over the shutter's share of its motion since the last frame,
//! the camera's and moving instances' alike: the frame's largest blur per
//! 32-pixel tile and its tile neighbourhood steer where each pixel gathers,
//! and depth and speed decide which samples may cover it, so moving edges
//! blur over what is behind them while slower surfaces in front, such as
//! what the camera follows, stay sharp.
//!
//! Placement: after antialiasing and before bloom, SMAA and tone mapping. Bevy
//! 9d12036 runs it after TAA (`bevy_anti_alias` `taa/mod.rs` in
//! `EarlyPostProcess`) and before bloom (`bevy_post_process`
//! `motion_blur/mod.rs`), and Wicked Engine 4323a33 after TAA and before
//! tone mapping and bloom (`wiRenderPath3D.cpp` `Postprocess_MotionBlur`).
//!
//! Reads: the antialiased HDR frame (`Completed`), the surface's depth and
//! motion at the render size (the Surface contract), the camera's
//! projection and the frames since history restarted.
//! Writes: its tile targets and the blurred frame, at the frame's size.
//! Honours: the effective shutter (`Settings::motion_blur` and
//! `FrameInput::motion_blur`); without one it does not run.
//! Timing group: `motion blur`.
use crate::shading::gbuffer::COLOR;
use crate::view::frame::FrameContext;
use crate::view::targets::target_with_usage;

/// The tile passes and the filter.
pub(crate) static MOTION_BLUR: crate::shading::Module = crate::shading::Module {
    name: "motion_blur",
    source: include_str!("motion_blur.wgsl"),
    deps: &[&crate::shading::DEPTH, &crate::shading::NOISE],
};

/// Wicked's `MOTIONBLUR_TILESIZE`: frame pixels per tile side, which the
/// shader takes as its override.
const TILE_SIZE: u32 = 32;
/// The shader's workgroup side.
const GROUP_SIZE: u32 = 8;

/// `MotionBlur` in `motion_blur.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MotionBlurUniform {
    velocity_scale: [f32; 2],
    render_scale: [f32; 2],
    near: f32,
    frame: u32,
}

/// The layouts this stage mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "motion_blur",
        "MotionBlur",
        MotionBlurUniform,
        [velocity_scale, render_scale, near, frame]
    )]
}

/// The entry points, in the order they run.
const ENTRY_POINTS: [&str; 4] = [
    "motionblur_tileMaxVelocity_horizontal",
    "motionblur_tileMaxVelocity_vertical",
    "motionblur_neighborhoodMaxVelocity",
    "motionblur",
];

/// The targets for one frame size.
struct Targets {
    size: [u32; 2],
    /// Each tile column's largest and smallest blur per frame row.
    horizontal: wgpu::TextureView,
    /// Each tile's.
    tiles: wgpu::TextureView,
    /// Each tile neighbourhood's.
    neighborhood: wgpu::TextureView,
    /// The blurred frame.
    output: wgpu::TextureView,
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
            horizontal: target("motion blur tile columns", [tiles[0], size[1]]),
            tiles: target("motion blur tiles", tiles),
            neighborhood: target("motion blur tile neighbourhoods", tiles),
            output: target("motion-blurred HDR scene", size),
        }
    }
}

pub(crate) struct MotionBlur {
    pipelines: [wgpu::ComputePipeline; 4],
    layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    targets: Option<Targets>,
    /// The groups of each pass, one set per frame view blurred (TAA
    /// alternates two) and surface depth, until the targets or the views
    /// are replaced.
    groups: Vec<(wgpu::TextureView, wgpu::TextureView, [wgpu::BindGroup; 4])>,
}

impl MotionBlur {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("motion blur"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&MOTION_BLUR]).into()),
        });
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
        let float = wgpu::TextureSampleType::Float { filterable: false };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("motion blur"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture(1, float),
                texture(2, wgpu::TextureSampleType::Depth),
                texture(3, float),
                texture(4, float),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: COLOR,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("motion blur"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipelines = ENTRY_POINTS.map(|entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &shader,
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
            layout,
            uniform: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("motion blur settings"),
                    size: size_of::<MotionBlurUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            targets: None,
            groups: Vec::new(),
        }
    }

    /// The blurred frame, once the stage has run.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn output(&self) -> Option<&wgpu::TextureView> {
        self.targets.as_ref().map(|targets| &targets.output)
    }

    /// Drops the bind groups, which hold frame views and shared targets
    /// that a resize or a new antialiasing context replaces.
    pub fn forget_inputs(&mut self) {
        self.groups.clear();
    }

    /// Blurs `frame`, the antialiased HDR frame, while the frame has a
    /// shutter and continues history: the blurred frame, or `None` when it
    /// did not run. A frame that restarts history is not blurred, since its
    /// moving instances' motion reaches back past the restart.
    pub fn encode(
        &mut self,
        ctx: &mut FrameContext<'_>,
        frame: &wgpu::TextureView,
    ) -> Option<&wgpu::TextureView> {
        let shutter = ctx.effective.motion_blur?;
        if !ctx.history.valid {
            return None;
        }
        let size = frame.texture().size();
        let size = [size.width, size.height];
        let render = ctx.sizes.render;
        let uniform = MotionBlurUniform {
            velocity_scale: size.map(|v| v as f32 * shutter),
            render_scale: [0, 1].map(|axis| render[axis] as f32 / size[axis] as f32),
            // `perspective`'s near plane (the effective configuration
            // requires its projection).
            near: ctx.input.camera.projection.w_axis.z,
            frame: ctx.history.frames,
        };
        crate::counters::write_buffer(ctx.queue, &self.uniform, 0, bytemuck::bytes_of(&uniform));
        self.dispatch(
            ctx.device,
            ctx.encoder,
            frame,
            [&ctx.targets.motion, ctx.surface.depth],
            ctx.timing,
        );
        self.targets.as_ref().map(|targets| &targets.output)
    }

    /// The tile passes and the filter of `frame` with the `motion` and
    /// `depth` targets.
    fn dispatch(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        frame: &wgpu::TextureView,
        [motion, depth]: [&wgpu::TextureView; 2],
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let size = frame.texture().size();
        let size = [size.width, size.height];
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            self.targets = Some(Targets::new(device, size));
            self.groups.clear();
        }
        let groups = self.groups(device, frame, motion, depth);
        let tiles = size.map(|v| v.div_ceil(TILE_SIZE));
        let dispatches = [[tiles[0], size[1]], tiles, tiles, size];
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("motion blur"),
            timestamp_writes: timing.and_then(|t| t.compute_pass("motion blur")),
        });
        for ((pipeline, group), extent) in self.pipelines.iter().zip(&groups).zip(dispatches) {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(
                extent[0].div_ceil(GROUP_SIZE),
                extent[1].div_ceil(GROUP_SIZE),
                1,
            );
        }
    }

    /// Each pass's group for `frame` and `depth`, made once while they are
    /// the frame blurred and the surface depth (the opaque depth, or the
    /// receivers'): binding 4 is the tiles a pass reads, 5 what it writes.
    fn groups(
        &mut self,
        device: &wgpu::Device,
        frame: &wgpu::TextureView,
        motion: &wgpu::TextureView,
        depth: &wgpu::TextureView,
    ) -> [wgpu::BindGroup; 4] {
        if let Some((.., groups)) = self
            .groups
            .iter()
            .find(|(view, surface, _)| view == frame && surface == depth)
        {
            return groups.clone();
        }
        let targets = self.targets.as_ref().unwrap();
        let passes = [
            (&targets.tiles, &targets.horizontal),
            (&targets.horizontal, &targets.tiles),
            (&targets.tiles, &targets.neighborhood),
            (&targets.neighborhood, &targets.output),
        ];
        let groups = passes.map(|(read, write)| {
            let views = [motion, depth, frame, read, write];
            let mut entries = vec![wgpu::BindGroupEntry {
                binding: 0,
                resource: self.uniform.as_entire_binding(),
            }];
            entries.extend(
                views
                    .iter()
                    .zip(1..)
                    .map(|(view, binding)| wgpu::BindGroupEntry {
                        binding,
                        resource: wgpu::BindingResource::TextureView(view),
                    }),
            );
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("motion blur"),
                layout: &self.layout,
                entries: &entries,
            })
        });
        self.groups
            .push((frame.clone(), depth.clone(), groups.clone()));
        groups
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
