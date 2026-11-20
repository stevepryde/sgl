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
//! Reads: the antialiased HDR frame (`Completed`), each texel at most
//! `shading::SCENE_COLOR`'s bound, the surface's depth and
//! motion at the render size (the Surface contract), the camera's
//! projection and the frames since history restarted.
//! Writes: its tile targets (`view::motion_tiles`, the passes it shares with
//! TAA's motion reach) and the blurred frame, at the frame's size.
//! Honours: the effective shutter (`Settings::motion_blur` and
//! `FrameInput::motion_blur`); without one it does not run.
//! Timing group: `motion blur`.
use crate::shading::gbuffer::COLOR;
use crate::shading::motion_tiles::{GROUP_SIZE, MOTION_TILES, MotionTilesUniform};
use crate::view::frame::FrameContext;
use crate::view::motion_tiles::MotionTiles;
use crate::view::targets::target_with_usage;

/// The tile passes and the filter.
pub(crate) static MOTION_BLUR: crate::shading::Module = crate::shading::Module {
    name: "motion_blur",
    source: include_str!("motion_blur.wgsl"),
    deps: &[
        &MOTION_TILES,
        &crate::shading::DEPTH,
        &crate::shading::NOISE,
        &crate::shading::SCENE_COLOR,
        &crate::shading::FINITE,
    ],
};
/// The filter's entry point, after the tile passes.
pub(crate) const FILTER_ENTRY: &str = "motionblur";

/// `MotionBlur` in `motion_blur.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MotionBlurUniform {
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
        [near, frame]
    )]
}

pub(crate) struct MotionBlur(Option<Filter>);

struct Filter {
    tiles: MotionTiles,
    filter: wgpu::ComputePipeline,
    uniform: wgpu::Buffer,
    /// The blurred frame, at its size.
    output: Option<wgpu::TextureView>,
    /// The filter's group for each frame view blurred (TAA alternates two)
    /// and surface depth, until the targets or the views are replaced.
    groups: Vec<(wgpu::TextureView, wgpu::TextureView, wgpu::BindGroup)>,
}

impl Filter {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("motion blur"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&MOTION_BLUR]).into()),
        });
        let filter = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(FILTER_ENTRY),
            layout: None,
            module: &shader,
            entry_point: Some(FILTER_ENTRY),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &[(
                    "MOTIONBLUR_TILESIZE",
                    f64::from(crate::shading::motion_tiles::TILE_SIZE),
                )],
                ..Default::default()
            },
            cache: None,
        });
        Self {
            tiles: MotionTiles::new(device, &shader),
            filter,
            uniform: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("motion blur settings"),
                    size: size_of::<MotionBlurUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            output: None,
            groups: Vec::new(),
        }
    }
}

impl MotionBlur {
    pub fn new(device: &wgpu::Device, enabled: bool) -> Self {
        Self(enabled.then(|| Filter::new(device)))
    }

    /// The blurred frame, once the stage has run.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn output(&self) -> Option<&wgpu::TextureView> {
        self.0.as_ref()?.output.as_ref()
    }

    /// Drops the bind groups, which hold frame views and shared targets
    /// that a resize or a new antialiasing context replaces.
    pub fn forget_inputs(&mut self) {
        if let Some(filter) = &mut self.0 {
            filter.tiles.forget_inputs();
            filter.groups.clear();
        }
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
        let tiles = MotionTilesUniform {
            velocity_scale: size.map(|v| v as f32 * shutter),
            render_scale: [0, 1].map(|axis| render[axis] as f32 / size[axis] as f32),
        };
        let uniform = MotionBlurUniform {
            // `perspective`'s near plane (the effective configuration
            // requires its projection).
            near: ctx.input.camera.projection.w_axis.z,
            frame: ctx.history.frames,
        };
        let filter = self.0.get_or_insert_with(|| Filter::new(ctx.device));
        filter.dispatch(
            (ctx.device, ctx.queue),
            ctx.encoder,
            frame,
            [&ctx.targets.motion, ctx.surface.depth],
            (tiles, uniform),
            ctx.timing,
        );
        filter.output.as_ref()
    }
}

impl Filter {
    /// The tile passes and the filter of `frame` with the `motion` and
    /// `depth` targets and the passes' values.
    fn dispatch(
        &mut self,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
        encoder: &mut wgpu::CommandEncoder,
        frame: &wgpu::TextureView,
        [motion, depth]: [&wgpu::TextureView; 2],
        (tiles, uniform): (MotionTilesUniform, MotionBlurUniform),
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        crate::counters::write_buffer(queue, &self.uniform, 0, bytemuck::bytes_of(&uniform));
        let size = frame.texture().size();
        if self
            .output
            .as_ref()
            .is_none_or(|output| output.texture().size() != size)
        {
            self.output = Some(target_with_usage(
                device,
                "motion-blurred HDR scene",
                [size.width, size.height],
                COLOR,
                wgpu::TextureUsages::STORAGE_BINDING,
            ));
            self.groups.clear();
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("motion blur"),
            timestamp_writes: timing.and_then(|t| t.compute_pass("motion blur")),
        });
        let neighborhood = self
            .tiles
            .encode(device, queue, &mut pass, [motion, frame], tiles)
            .clone();
        let group = self.group(device, [motion, depth, frame], &neighborhood);
        pass.set_pipeline(&self.filter);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(
            size.width.div_ceil(GROUP_SIZE),
            size.height.div_ceil(GROUP_SIZE),
            1,
        );
    }

    /// The filter's group for `frame` and `depth`, made once while they are
    /// the frame blurred and the surface depth (the opaque depth, or the
    /// receivers'), over the tile `neighborhood`.
    fn group(
        &mut self,
        device: &wgpu::Device,
        [motion, depth, frame]: [&wgpu::TextureView; 3],
        neighborhood: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        if let Some((.., group)) = self
            .groups
            .iter()
            .find(|(view, surface, _)| view == frame && surface == depth)
        {
            return group.clone();
        }
        let view = wgpu::BindingResource::TextureView;
        let entry = |binding, resource| wgpu::BindGroupEntry { binding, resource };
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("motion blur"),
            layout: &self.filter.get_bind_group_layout(0),
            entries: &[
                entry(0, self.tiles.uniform().as_entire_binding()),
                entry(1, view(motion)),
                entry(2, view(depth)),
                entry(3, view(frame)),
                entry(4, view(neighborhood)),
                entry(5, view(self.output.as_ref().unwrap())),
                entry(6, self.uniform.as_entire_binding()),
            ],
        });
        self.groups
            .push((frame.clone(), depth.clone(), group.clone()));
        group
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
