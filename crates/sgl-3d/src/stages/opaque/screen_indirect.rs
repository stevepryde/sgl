//! Screen-space indirect light: the opaque stage's gather, denoise and
//! accumulation at half the render size, after ambient occlusion, and the
//! kept result the next frame's camera lighting reads (the architecture's
//! Screen-space indirect light, D-65). See screen_indirect/README.md.
use crate::settings::ScreenSpaceIndirect;
use crate::shading::uniforms::ViewUniform;
use crate::view::cached_group::CachedGroup;
use crate::view::compile::{Compiled, Compiler};
use crate::view::history::{HistoryFrame, Kept, StageRun};

/// The pass's four dispatches.
pub(crate) static SCREEN_INDIRECT_PASS: crate::shading::Module = crate::shading::Module {
    name: "screen_indirect_pass",
    source: sgl_post_fx::embed_wgsl!("screen_indirect.wgsl"),
    deps: &[
        &crate::shading::GBUFFER,
        &crate::shading::SCREEN_INDIRECT,
        &super::ambient_occlusion::XE_GTAO_KERNELS,
    ],
};
/// The entry points its pipelines are created with.
pub(crate) const PREFILTER_ENTRY: &str = "prefilter_sources";
pub(crate) const GATHER_ENTRY: &str = "gather";
pub(crate) const DENOISE_ENTRY: &str = "denoise";
pub(crate) const ACCUMULATE_ENTRY: &str = "accumulate";
/// Its timing group, which also holds the depth pyramid in a frame whose
/// ambient occlusion is off.
pub(crate) const TIMING_GROUP: &str = "screen-space indirect";

/// The reach in metres SSRT3 b38d821 (`SSRT_HDRP.cs` `radius`) clamps its
/// radius to, and the default it and Godot b130438's `ssil_radius` take.
const MIN_RADIUS: f32 = 1.;
const MAX_RADIUS: f32 = 25.;
const DEFAULT_RADIUS: f32 = 5.;
/// The most slices and steps a side a pixel takes, the High level
/// (`SCREEN_INDIRECT_MOST_SLICES` and `SCREEN_INDIRECT_MOST_STEPS` in
/// screen_indirect.wgsl).
const MOST_SLICES: u32 = 2;
const MOST_STEPS: u32 = 12;

/// The frame's reach for `radius` metres: clamped to `MIN_RADIUS..=MAX_RADIUS`,
/// NaN to `DEFAULT_RADIUS`.
pub(crate) fn radius(radius: f32) -> f32 {
    if radius.is_nan() {
        DEFAULT_RADIUS
    } else {
        radius.clamp(MIN_RADIUS, MAX_RADIUS)
    }
}

/// The slices and steps a side of `level`: SSRT3's defaults, one slice of
/// twelve steps a side, at Low, and two slices at High.
fn samples(level: ScreenSpaceIndirect) -> (u32, u32) {
    match level {
        ScreenSpaceIndirect::Off | ScreenSpaceIndirect::Low => (1, MOST_STEPS),
        ScreenSpaceIndirect::High => (MOST_SLICES, MOST_STEPS),
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    view: [[f32; 4]; 4],
    size: [u32; 2],
    half_size: [u32; 2],
    pixel_size: [f32; 2],
    ndc_mul: [f32; 2],
    ndc_add: [f32; 2],
    jitter_delta: [f32; 2],
    radius: f32,
    slices: u32,
    steps: u32,
    noise_index: u32,
    reset: u32,
    /// WGSL rounds the struct up to its 16-byte alignment.
    padding: [u32; 3],
}

/// `ScreenIndirectView` in shading/screen_indirect.wgsl: the camera of the
/// frame the kept result was written in, in this frame's render frame.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct KeptView {
    /// World to view, rotation only, by column.
    pub rotation: [[f32; 4]; 3],
    pub projection_scale: [f32; 2],
    pub render_size: [f32; 2],
}

impl KeptView {
    /// `camera`'s, for a kept result of a render of `size`.
    fn of(camera: &crate::view::history::CameraFrame, size: [u32; 2]) -> Self {
        let view = camera.view;
        Self {
            rotation: [view.x_axis, view.y_axis, view.z_axis].map(|axis| {
                let mut column = axis.to_array();
                column[3] = 0.;
                column
            }),
            projection_scale: [camera.projection.x_axis.x, camera.projection.y_axis.y],
            render_size: size.map(|extent| extent as f32),
        }
    }
}

/// The bounce sources' levels (`SCREEN_INDIRECT_SOURCE_MIPS`): one below
/// each depth mip the march reads but the first.
const SOURCE_MIPS: u32 = 4;
/// The level-0 texels the last level's texel is the mean of, a side.
const SOURCE_FOOTPRINT: u32 = 1 << (SOURCE_MIPS - 1);

/// The pass's targets at half of `size`: the bounce sources and a view of
/// each of their levels, the gathered light and its edges, the denoised
/// light, and the kept result's pair (shading/screen_indirect.wgsl).
struct Targets {
    size: [u32; 2],
    half: [u32; 2],
    sources: wgpu::TextureView,
    source_levels: [wgpu::TextureView; SOURCE_MIPS as usize],
    gathered: wgpu::TextureView,
    edges: wgpu::TextureView,
    denoised: wgpu::TextureView,
    kept: [wgpu::TextureView; 2],
}

impl Targets {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let half = size.map(|extent| extent.div_ceil(2).max(1));
        let make = |label, format| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: half[0],
                        height: half[1],
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
        };
        let light = wgpu::TextureFormat::Rgba16Float;
        // Each side a multiple of the last level's footprint, so every level
        // covers the half-size frame exactly: the shader takes each level's
        // size from the texture, its one owner.
        let sources = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("screen-space indirect bounce sources"),
            size: wgpu::Extent3d {
                width: half[0].next_multiple_of(SOURCE_FOOTPRINT),
                height: half[1].next_multiple_of(SOURCE_FOOTPRINT),
                depth_or_array_layers: 1,
            },
            mip_level_count: SOURCE_MIPS,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg32Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Self {
            size,
            half,
            source_levels: std::array::from_fn(|level| {
                sources.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: level as u32,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            }),
            sources: sources.create_view(&Default::default()),
            gathered: make("screen-space indirect gathered light", light),
            edges: make("screen-space indirect edges", wgpu::TextureFormat::R32Uint),
            denoised: make("screen-space indirect denoised light", light),
            kept: std::array::from_fn(|_| {
                make(
                    "screen-space indirect kept light",
                    wgpu::TextureFormat::Rgba32Uint,
                )
            }),
        }
    }
}

/// What the frame's camera lighting reads: the last finished frame's kept
/// result and the view it was kept in.
#[derive(Clone, Copy)]
pub(crate) struct KeptRead<'a> {
    pub kept: &'a wgpu::TextureView,
    pub view: &'a wgpu::Buffer,
    /// Which half of the pair `kept` is, for the reader's cached groups.
    pub half: usize,
}

/// What the frame's own inputs the pass reads.
pub(crate) struct Inputs<'a> {
    pub depth_pyramid: &'a wgpu::TextureView,
    pub hilbert: &'a wgpu::TextureView,
    pub depth: &'a wgpu::TextureView,
    pub normals: &'a wgpu::TextureView,
    pub color: &'a wgpu::TextureView,
    pub motion: &'a wgpu::TextureView,
}

/// The pass's programs, apart from the kept result, which selects the
/// camera lighting's pipeline whether or not they are compiled yet.
struct Programs {
    prefilter: wgpu::ComputePipeline,
    gather: wgpu::ComputePipeline,
    denoise: wgpu::ComputePipeline,
    accumulate: wgpu::ComputePipeline,
    /// The prefilter's, gather's and denoise's groups, and the
    /// accumulation's for each half it writes.
    groups: [CachedGroup; 5],
}

/// The set's name for its compile job.
const LABEL: &str = "screen-space indirect light";

pub(crate) struct ScreenIndirect {
    programs: Compiled<Programs>,
    params: wgpu::Buffer,
    kept_view: wgpu::Buffer,
    targets: Option<Targets>,
    /// The run and settings (level, radius, the pyramid's radius) of the
    /// last finished frame's pass
    /// (SI-1, SI-6).
    history: Kept<(StageRun, crate::view::effective::ScreenIndirect)>,
}

impl Programs {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("screen-space indirect light"),
            source: wgpu::ShaderSource::Wgsl(
                crate::shading::compose(&[&SCREEN_INDIRECT_PASS]).into(),
            ),
        });
        let pipeline = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let prefilter = pipeline(PREFILTER_ENTRY);
        let gather = pipeline(GATHER_ENTRY);
        let denoise = pipeline(DENOISE_ENTRY);
        let accumulate = pipeline(ACCUMULATE_ENTRY);
        let groups = [
            CachedGroup::new(prefilter.get_bind_group_layout(0)),
            CachedGroup::new(gather.get_bind_group_layout(0)),
            CachedGroup::new(denoise.get_bind_group_layout(0)),
            CachedGroup::new(accumulate.get_bind_group_layout(0)),
            CachedGroup::new(accumulate.get_bind_group_layout(0)),
        ];
        Self {
            prefilter,
            gather,
            denoise,
            accumulate,
            groups,
        }
    }
}

impl ScreenIndirect {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let uniform = |label, size| {
            crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some(label),
                    size,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            )
        };
        Self {
            programs: Compiled::default(),
            params: uniform(
                "screen-space indirect parameters",
                std::mem::size_of::<Params>() as u64,
            ),
            kept_view: uniform(
                "screen-space indirect kept view",
                std::mem::size_of::<KeptView>() as u64,
            ),
            targets: None,
            history: Kept::default(),
        }
    }

    /// Begins a frame of a render of `size` whose history is `history`,
    /// with the pass running as `settings` says: leaves the frame's run
    /// and returns whether its camera lighting takes the last finished
    /// frame's kept result (`read`), where the frame continues it (SI-1,
    /// SI-2). Off, it releases its targets and leaves nothing, so the next
    /// frame that runs it restarts.
    pub(crate) fn begin(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        settings: Option<crate::view::effective::ScreenIndirect>,
        size: [u32; 2],
        history: &HistoryFrame,
    ) -> bool {
        let Some(settings) = settings else {
            self.targets = None;
            self.history.forget();
            return false;
        };
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            self.targets = Some(Targets::new(device, size));
            self.history.forget();
        }
        let last = self.history.last();
        // Another level, radius or pyramid (ambient occlusion turned on or
        // off, or its radius changed) restarts it (SI-1).
        let continues = history.valid && last.is_some_and(|&(_, kept)| kept == settings);
        let run = StageRun::after(last.map(|(run, _)| run), continues);
        self.history.leave((run, settings));
        let Some(previous) = history.previous_camera.filter(|_| run.count > 0) else {
            return false;
        };
        crate::counters::write_buffer(
            queue,
            &self.kept_view,
            0,
            bytemuck::bytes_of(&KeptView::of(&previous, size)),
        );
        true
    }

    /// What the frame's camera lighting reads, where it continues the last
    /// finished frame's run (`begin`): that frame's kept result.
    pub(crate) fn read(&self) -> Option<KeptRead<'_>> {
        let &(run, ..) = self.history.left()?;
        if run.count == 0 {
            return None;
        }
        let half = 1 - run.current;
        Some(KeptRead {
            kept: &self.targets.as_ref()?.kept[half],
            view: &self.kept_view,
            half,
        })
    }

    /// The frame's bounce sources, gather, denoise and accumulation into
    /// the half of the pair its run writes (`begin`), seen by `view`, at
    /// `settings`, with the frame's `inputs`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode(
        &mut self,
        (device, compiler): (&wgpu::Device, &Compiler),
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        inputs: &Inputs<'_>,
        view: &ViewUniform,
        history: &HistoryFrame,
        settings: crate::view::effective::ScreenIndirect,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let Some(programs) = self.programs.get(compiler, LABEL, || Programs::new) else {
            return;
        };
        let &(run, ..) = self
            .history
            .left()
            .expect("the frame began the pass (`begin`)");
        let targets = self.targets.as_ref().unwrap();
        let (slices, steps) = samples(settings.level);
        let projection = view.projection;
        let tan_half = [1. / projection[0][0], 1. / projection[1][1]];
        let size = targets.size;
        let camera = history.camera;
        let previous = history.previous_camera.unwrap_or(camera);
        let params = Params {
            view: view.view,
            size,
            half_size: targets.half,
            pixel_size: [1. / size[0] as f32, 1. / size[1] as f32],
            ndc_mul: [2. * tan_half[0], -2. * tan_half[1]],
            // As ambient occlusion's: the raster's subpixel offset undone.
            ndc_add: [
                -tan_half[0] * (1. + 2. * view.jitter[0]),
                tan_half[1] * (1. - 2. * view.jitter[1]),
            ],
            // As ambient occlusion's history: the previous frame's pixel
            // grid where the current pixel's surface stood.
            jitter_delta: [
                (previous.jitter[0] - camera.jitter[0]) * 0.5,
                (camera.jitter[1] - previous.jitter[1]) * 0.5,
            ],
            radius: settings.radius,
            slices,
            steps,
            noise_index: history.frames,
            reset: u32::from(run.count == 0),
            padding: [0; 3],
        };
        crate::counters::write_buffer(queue, &self.params, 0, bytemuck::bytes_of(&params));
        let texture = |binding, view| (binding, wgpu::BindingResource::TextureView(view));
        let workgroups = targets.half.map(|extent| extent.div_ceil(8));
        let group = programs.groups[0].get(
            device,
            "screen-space indirect bounce sources",
            &[
                (0, self.params.as_entire_binding()),
                texture(2, inputs.normals),
                texture(3, inputs.color),
                texture(17, &targets.source_levels[0]),
                texture(18, &targets.source_levels[1]),
                texture(19, &targets.source_levels[2]),
                texture(20, &targets.source_levels[3]),
            ],
        );
        dispatch(encoder, &programs.prefilter, group, workgroups, timing);
        let group = programs.groups[1].get(
            device,
            "screen-space indirect gather",
            &[
                (0, self.params.as_entire_binding()),
                texture(1, inputs.depth_pyramid),
                texture(2, inputs.normals),
                texture(5, inputs.hilbert),
                texture(6, &targets.gathered),
                texture(7, &targets.edges),
                texture(21, &targets.sources),
            ],
        );
        dispatch(encoder, &programs.gather, group, workgroups, timing);
        let group = programs.groups[2].get(
            device,
            "screen-space indirect denoise",
            &[
                (0, self.params.as_entire_binding()),
                texture(8, &targets.gathered),
                texture(9, &targets.edges),
                texture(10, &targets.denoised),
            ],
        );
        dispatch(encoder, &programs.denoise, group, workgroups, timing);
        let current = run.current;
        let group = programs.groups[3 + current].get(
            device,
            "screen-space indirect accumulation",
            &[
                (0, self.params.as_entire_binding()),
                texture(1, inputs.depth_pyramid),
                texture(2, inputs.normals),
                texture(11, &targets.denoised),
                texture(12, inputs.depth),
                texture(13, inputs.motion),
                texture(14, &targets.kept[1 - current]),
                texture(15, &targets.kept[current]),
                (16, self.kept_view.as_entire_binding()),
            ],
        );
        dispatch(encoder, &programs.accumulate, group, workgroups, timing);
    }

    /// Asks for the programs in a planning pass that cannot encode the
    /// pass yet (its depth pyramid's stage is still compiling).
    pub(crate) fn plan(&mut self, compiler: &Compiler) {
        self.programs.get(compiler, LABEL, || Programs::new);
    }

    /// Settles the last render's run: see `Kept::settle`.
    pub(crate) fn settle(&mut self, finished: bool) {
        self.history.settle(finished);
    }

    /// Whether it holds its targets.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn holds_targets(&self) -> bool {
        self.targets.is_some()
    }

    /// The kept result the last encoded frame wrote.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn written(&self) -> Option<&wgpu::TextureView> {
        let &(run, ..) = self.history.left()?;
        Some(&self.targets.as_ref()?.kept[run.current])
    }
}

fn dispatch(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    group: &wgpu::BindGroup,
    workgroups: [u32; 2],
    timing: Option<&crate::timing::GpuTiming>,
) {
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("screen-space indirect light"),
        timestamp_writes: timing.and_then(|t| t.compute_pass(TIMING_GROUP)),
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, group, &[]);
    pass.dispatch_workgroups(workgroups[0], workgroups[1], 1);
}

/// The constants with WGSL twins.
#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 4] {
    [
        ("SCREEN_INDIRECT_MOST_SLICES", MOST_SLICES),
        ("SCREEN_INDIRECT_MOST_STEPS", MOST_STEPS),
        ("SCREEN_INDIRECT_SOURCE_MIPS", SOURCE_MIPS),
        // Ambient occlusion's table, which the gather's noise reads.
        (
            "SCREEN_INDIRECT_HILBERT_WIDTH",
            super::ambient_occlusion::HILBERT_WIDTH,
        ),
    ]
    .map(|(name, value)| {
        crate::shading::layout_tests::Constant::new(
            "screen_indirect_pass",
            name,
            naga::Literal::U32(value),
        )
    })
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 2] {
    [
        crate::shading::layout_tests::mirror!(
            "screen_indirect_pass",
            "Params",
            Params,
            [
                view,
                size,
                half_size,
                pixel_size,
                ndc_mul,
                ndc_add,
                jitter_delta,
                radius,
                slices,
                steps,
                noise_index,
                reset,
            ]
        ),
        crate::shading::layout_tests::mirror!(
            "screen_indirect_pass",
            "ScreenIndirectView",
            KeptView,
            [rotation, projection_scale, render_size]
        ),
    ]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
