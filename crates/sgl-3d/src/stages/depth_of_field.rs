//! Depth of field: Wicked Engine's tiled gather (`depth_of_field.wgsl`),
//! after Jimenez 2014, with Bevy's thin-lens circle of confusion. Each
//! pixel blurs into the disc the frame's lens gives a point at its depth:
//! the largest and smallest circle of confusion per 32-pixel tile and its
//! tile neighbourhood steer, at half the scene size, a gather that sorts
//! what it reads into a background and a foreground, so a blurred
//! foreground spreads over what lies behind it while a sharp subject keeps
//! its edge against a blurred background; the composite then takes each
//! pixel between its own colour and that blur.
//!
//! Placement: after antialiasing and before motion blur, bloom and tone
//! mapping. Wicked Engine 4323a33 runs it after TAA and before motion blur
//! (`wiRenderPath3D.cpp` `Postprocess_DepthOfField`), and Filament 3c5ff3a
//! after TAA and before bloom (`Renderer.cpp`).
//!
//! Reads: the antialiased HDR frame (`Completed`), each texel at most
//! `shading::SCENE_COLOR`'s bound, the surface's depth at the render size
//! (the Surface contract), the camera's projection and the frame's lens.
//! Writes: its tile targets at the frame's size over `TILE_SIZE`, its
//! half-size presort, prefilter, gather and postfilter, and the focused
//! frame, at the frame's size.
//! Honours: `Settings::depth_of_field` and `FrameInput::depth_of_field`
//! (the effective configuration's `depth_of_field`); without them it runs
//! no pass and holds no target.
//! Timing group: `depth of field`.
use crate::DepthOfFieldParameters;
use crate::settings::DepthOfField as Level;
use crate::shading::gbuffer::COLOR;
use crate::view::compile::{Compiled, SharedModule};
use crate::view::effective::DepthOfFieldFrame;
use crate::view::frame::FrameContext;
use crate::view::targets::target_with_usage;

/// Frame pixels per tile side: the gather's largest radius, which a tile's
/// 3x3 neighbourhood reaches (`DOF_TILESIZE`).
pub(crate) const TILE_SIZE: u32 = 32;
/// Threads along each side of a workgroup.
const GROUP_SIZE: u32 = 8;

/// The passes and their entry points in `depth_of_field.wgsl`, in order.
pub(crate) const ENTRY_POINTS: [&str; 7] = [
    "dof_tile_horizontal",
    "dof_tile_vertical",
    "dof_neighborhood",
    "dof_prepass",
    "dof_main",
    "dof_postfilter",
    "dof_upsample",
];

pub(crate) static DEPTH_OF_FIELD: crate::shading::Module = crate::shading::Module {
    name: "depth_of_field",
    source: sgl_post_fx::embed_wgsl!("depth_of_field.wgsl"),
    deps: &[&crate::shading::DEPTH, &crate::shading::SCENE_COLOR],
};

/// The frame's lens and gather at `level`, under `projection`, while the
/// level is on and `lens` is one: finite and positive, focused beyond its
/// focal length. The focal length is Bevy's (`calculate_focal_length`, dof
/// `mod.rs` 706fdd9): half the sensor's height over the tangent of half
/// the projection's vertical field of view.
pub(crate) fn resolve(
    level: Level,
    lens: &DepthOfFieldParameters,
    projection: glam::Mat4,
) -> Option<DepthOfFieldFrame> {
    let rings = level.rings()?;
    let focal_length = 0.5 * lens.sensor_height * projection.y_axis.y;
    let positive = |v: f32| v.is_finite() && v > 0.;
    (positive(lens.focus_distance)
        && positive(lens.f_number)
        && positive(lens.sensor_height)
        && positive(focal_length)
        && lens.focus_distance > focal_length)
        .then_some(DepthOfFieldFrame {
            focus_distance: lens.focus_distance,
            f_number: lens.f_number,
            sensor_height: lens.sensor_height,
            focal_length,
            rings,
        })
}

/// `DepthOfField` in `depth_of_field.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DepthOfFieldUniform {
    near: f32,
    coc_scale: f32,
    focus: f32,
    rings: u32,
    render_scale: [f32; 2],
}

impl DepthOfFieldUniform {
    /// The passes' values for `lens` under a projection whose near plane is
    /// `near`, focusing a frame of `size` from a depth target of `render`.
    /// The circle of confusion's diameter on the sensor is Bevy's
    /// `coc_scale_factor * |z - focus| / (z * (focus - focal_length))`, with
    /// `coc_scale_factor` its `focal_length² / (sensor_height *
    /// aperture_f_stops)` and the image's height over the sensor's; the
    /// passes take half of it, the radius, in pixels of the half-size gather.
    fn new(lens: &DepthOfFieldFrame, near: f32, size: [u32; 2], render: [u32; 2]) -> Self {
        let f = lens.focal_length;
        let diameter = f * f / (lens.sensor_height * lens.f_number * (lens.focus_distance - f))
            * size[1] as f32;
        Self {
            near,
            // Finite, so a point on the plane of focus is 0 however open the
            // lens: an infinite scale times 0 would be NaN.
            coc_scale: (diameter * 0.5 * 0.5).min(f32::MAX),
            focus: lens.focus_distance,
            rings: lens.rings,
            render_scale: [0, 1].map(|axis| render[axis] as f32 / size[axis] as f32),
        }
    }
}

/// The layouts this stage mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "depth_of_field",
        "DepthOfField",
        DepthOfFieldUniform,
        [near, coc_scale, focus, rings, render_scale]
    )]
}

/// The stage: nothing until a frame runs it. Its programs, constant buffer
/// and sampler are one compiled bundle, with the targets they fill.
pub(crate) struct DepthOfField(Compiled<Filter>);

const LABEL: &str = "depth of field";

/// The targets for one frame size.
struct Targets {
    size: [u32; 2],
    /// Each tile column's per frame row.
    horizontal: wgpu::TextureView,
    tiles: wgpu::TextureView,
    neighborhood: wgpu::TextureView,
    /// Half the frame's size.
    presort: wgpu::TextureView,
    prefilter: wgpu::TextureView,
    main: wgpu::TextureView,
    postfilter: wgpu::TextureView,
    /// The focused frame.
    output: wgpu::TextureView,
}

impl Targets {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let target = |label, size| {
            target_with_usage(
                device,
                label,
                size,
                COLOR,
                wgpu::TextureUsages::STORAGE_BINDING,
            )
        };
        let tiles = size.map(|v| v.div_ceil(TILE_SIZE));
        let half = size.map(|v| v.div_ceil(2));
        Self {
            size,
            horizontal: target("depth of field tile columns", [tiles[0], size[1]]),
            tiles: target("depth of field tiles", tiles),
            neighborhood: target("depth of field tile neighbourhoods", tiles),
            presort: target("depth of field presort", half),
            prefilter: target("depth of field prefilter", half),
            main: target("depth of field gather", half),
            postfilter: target("depth of field postfilter", half),
            output: target("focused HDR scene", size),
        }
    }

    /// Each pass's dispatch, in threads.
    fn extents(&self) -> [[u32; 2]; 7] {
        let tiles = self.size.map(|v| v.div_ceil(TILE_SIZE));
        let half = self.size.map(|v| v.div_ceil(2));
        [
            [tiles[0], self.size[1]],
            tiles,
            tiles,
            half,
            half,
            half,
            self.size,
        ]
    }
}

struct Filter {
    pipelines: [wgpu::ComputePipeline; 7],
    uniform: wgpu::Buffer,
    sampler: wgpu::Sampler,
    targets: Option<Targets>,
    /// The focused frame was made or let go since `take_replaced`.
    replaced: bool,
    /// Each pass's group for each frame view focused (TAA alternates two)
    /// and surface depth, until the targets or the views are replaced.
    groups: Vec<(wgpu::TextureView, wgpu::TextureView, [wgpu::BindGroup; 7])>,
}

impl Filter {
    fn new(device: &wgpu::Device) -> Self {
        let shader = SharedModule::composed(LABEL, &DEPTH_OF_FIELD);
        let pipelines = ENTRY_POINTS.map(|entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: shader.get(device),
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &[("DOF_TILESIZE", f64::from(TILE_SIZE))],
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
                    label: Some("depth of field settings"),
                    size: size_of::<DepthOfFieldUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("depth of field linear clamp"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            targets: None,
            replaced: false,
            groups: Vec::new(),
        }
    }
}

impl DepthOfField {
    pub fn new() -> Self {
        Self(Compiled::default())
    }

    /// The focused frame, while the stage holds one.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn output(&self) -> Option<&wgpu::TextureView> {
        Some(&self.0.ready()?.targets.as_ref()?.output)
    }

    /// Drops the bind groups, which hold frame views and shared targets
    /// that a resize or a new antialiasing context replaces.
    pub fn forget_inputs(&mut self) {
        if let Some(filter) = self.0.ready_mut() {
            filter.groups.clear();
        }
    }

    /// Whether the focused frame was made or let go since the last call:
    /// the stages that read it then drop the groups that bind the old one.
    pub fn take_replaced(&mut self) -> bool {
        self.0
            .ready_mut()
            .is_some_and(|filter| std::mem::take(&mut filter.replaced))
    }

    /// Focuses `frame`, the antialiased HDR frame, while the frame runs
    /// depth of field: the focused frame, or `None` when it did not run, in
    /// which case the stage releases its targets and keeps its pipelines.
    /// A planning pass that finds the pipelines missing queues them and
    /// runs nothing (`Renderer::compile`).
    pub fn encode(
        &mut self,
        ctx: &mut FrameContext<'_>,
        frame: &wgpu::TextureView,
    ) -> Option<&wgpu::TextureView> {
        let Some(lens) = ctx.effective.depth_of_field else {
            if let Some(filter) = self.0.ready_mut()
                && filter.targets.take().is_some()
            {
                filter.replaced = true;
                filter.groups.clear();
            }
            return None;
        };
        let size = frame.texture().size();
        let size = [size.width, size.height];
        let uniform = DepthOfFieldUniform::new(
            &lens,
            // `perspective`'s near plane (the effective configuration
            // requires its projection).
            ctx.input.camera.projection.w_axis.z,
            size,
            ctx.sizes.render,
        );
        let filter = self.0.get(ctx.compiler, LABEL, || Filter::new)?;
        filter.dispatch(
            (ctx.device, ctx.queue),
            ctx.encoder,
            [frame, ctx.surface.depth],
            uniform,
            ctx.timing,
        );
        Some(&filter.targets.as_ref()?.output)
    }
}

impl Filter {
    /// The passes over `frame` with the surface's `depth`.
    fn dispatch(
        &mut self,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
        encoder: &mut wgpu::CommandEncoder,
        [frame, depth]: [&wgpu::TextureView; 2],
        uniform: DepthOfFieldUniform,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        crate::counters::write_buffer(queue, &self.uniform, 0, bytemuck::bytes_of(&uniform));
        let size = frame.texture().size();
        let size = [size.width, size.height];
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            self.targets = Some(Targets::new(device, size));
            self.replaced = true;
            self.groups.clear();
        }
        let groups = self.groups(device, frame, depth);
        let extents = self.targets.as_ref().unwrap().extents();
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("depth of field"),
            timestamp_writes: timing.and_then(|t| t.compute_pass("depth of field")),
        });
        for ((pipeline, group), extent) in self.pipelines.iter().zip(&groups).zip(extents) {
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
    /// the frame focused and the surface depth (the opaque depth, or the
    /// receivers').
    fn groups(
        &mut self,
        device: &wgpu::Device,
        frame: &wgpu::TextureView,
        depth: &wgpu::TextureView,
    ) -> [wgpu::BindGroup; 7] {
        if let Some((.., groups)) = self
            .groups
            .iter()
            .find(|(view, surface, _)| view == frame && surface == depth)
        {
            return groups.clone();
        }
        let t = self.targets.as_ref().unwrap();
        let view = wgpu::BindingResource::TextureView;
        let entry = |binding, resource| wgpu::BindGroupEntry { binding, resource };
        let settings = || entry(0, self.uniform.as_entire_binding());
        let sampler = || entry(10, wgpu::BindingResource::Sampler(&self.sampler));
        let entries = [
            vec![
                settings(),
                entry(1, view(depth)),
                entry(2, view(frame)),
                entry(4, view(&t.horizontal)),
            ],
            vec![entry(3, view(&t.horizontal)), entry(4, view(&t.tiles))],
            vec![entry(3, view(&t.tiles)), entry(4, view(&t.neighborhood))],
            vec![
                settings(),
                entry(1, view(depth)),
                entry(2, view(frame)),
                entry(3, view(&t.neighborhood)),
                entry(4, view(&t.presort)),
                entry(5, view(&t.prefilter)),
                sampler(),
            ],
            vec![
                settings(),
                entry(3, view(&t.neighborhood)),
                entry(4, view(&t.main)),
                entry(6, view(&t.presort)),
                entry(7, view(&t.prefilter)),
                sampler(),
            ],
            vec![entry(4, view(&t.postfilter)), entry(8, view(&t.main))],
            vec![
                settings(),
                entry(1, view(depth)),
                entry(2, view(frame)),
                entry(3, view(&t.neighborhood)),
                entry(4, view(&t.output)),
                entry(9, view(&t.postfilter)),
                sampler(),
            ],
        ];
        let mut entries = entries.into_iter();
        let groups = self.pipelines.each_ref().map(|pipeline| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("depth of field"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &entries.next().unwrap(),
            })
        });
        self.groups
            .push((frame.clone(), depth.clone(), groups.clone()));
        groups
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
