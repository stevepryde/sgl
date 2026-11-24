//! Current-frame FP32 XeGTAO scalar visibility. See ambient_occlusion/README.md.
mod temporal;
use crate::{
    settings::AmbientOcclusionQuality, shading::uniforms::ViewUniform,
    view::cached_group::CachedGroup,
};

/// XeGTAO's pure kernels, which ambient occlusion and screen-space
/// indirect light both take.
pub(crate) static XE_GTAO_KERNELS: crate::shading::Module = crate::shading::Module {
    name: "xegtao_kernels",
    source: sgl_post_fx::embed_wgsl!("xegtao_kernels.wgsl"),
    deps: &[],
};
/// XeGTAO's passes.
pub(crate) static XE_GTAO: crate::shading::Module = crate::shading::Module {
    name: "ambient_occlusion",
    source: sgl_post_fx::embed_wgsl!("ambient_occlusion.wgsl"),
    deps: &[&crate::shading::GBUFFER, &XE_GTAO_KERNELS],
};
/// The entry points XeGTAO's pipelines are created with.
pub(crate) const PREFILTER_ENTRY: &str = "prefilter_depths";
pub(crate) const PREFILTER_MIP4_ENTRY: &str = "prefilter_depth4";
pub(crate) const MAIN_PASS_ENTRY: &str = "main_pass";
pub(crate) const DENOISE_ENTRY: &str = "denoise";

/// The least search radius in metres: the low end of XeGTAO's expected
/// range (`XeGTAO.h` `GTAOImGuiSettings`) and of Godot's
/// `Environment::ssao_radius` (b130438 `scene/resources/environment.cpp`).
/// The pass divides by the radius, so zero has no defined visibility.
const MIN_RADIUS: f32 = 0.01;
/// The greatest search radius in metres, to which XeGTAO's
/// `GTAOImGuiSettings` clamps it.
const MAX_RADIUS: f32 = 10000.;
/// The most slices and steps a pixel takes: XeGTAO's Ultra preset
/// (`MOST_SLICES` and `MOST_STEPS` in ambient_occlusion.wgsl).
const MOST_SLICES: u32 = 9;
const MOST_STEPS: u32 = 3;
/// The working depth's mips: XeGTAO_PrefilterDepths16x16's five.
const DEPTH_MIPS: usize = 5;
/// The Hilbert curve's tile, texels across (`XE_HILBERT_WIDTH`), over which
/// the main pass's noise repeats.
pub(crate) const HILBERT_WIDTH: u32 = 64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    view: [[f32; 4]; 4],
    size: [u32; 2],
    pixel_size: [f32; 2],
    depth_unpack: [f32; 2],
    ndc_mul: [f32; 2],
    ndc_add: [f32; 2],
    radius: f32,
    slices: u32,
    steps: u32,
    noise_index: u32,
    padding: [u32; 2],
}
/// The working depth's pyramid, which ambient occlusion and screen-space
/// indirect light both read.
struct Pyramid {
    size: [u32; 2],
    depth: wgpu::TextureView,
    mips: [wgpu::TextureView; DEPTH_MIPS],
}
/// Ambient occlusion's own targets.
struct Targets {
    size: [u32; 2],
    working: wgpu::TextureView,
    output: wgpu::TextureView,
}
pub(crate) struct AmbientOcclusion {
    prefilter: wgpu::ComputePipeline,
    prefilter_mip4: wgpu::ComputePipeline,
    main: wgpu::ComputePipeline,
    denoise: wgpu::ComputePipeline,
    /// One group per pass, retaining its inferred layout and bound identities.
    groups: [CachedGroup; 4],
    /// The prefilter's parameters, apart from the main pass's so the
    /// pyramid can be built for screen-space indirect light alone, at its
    /// radius, in a frame whose ambient occlusion is off.
    pyramid_params: wgpu::Buffer,
    params: wgpu::Buffer,
    hilbert_lut: wgpu::TextureView,
    pyramid: Option<Pyramid>,
    targets: Option<Targets>,
    temporal: temporal::Temporal,
}
impl AmbientOcclusion {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resolve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        depth: &wgpu::TextureView,
        motion: &wgpu::TextureView,
        history: crate::view::history::HistoryFrame,
        quality: AmbientOcclusionQuality,
        radius: f32,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        self.temporal.encode(
            device,
            queue,
            encoder,
            &self.targets.as_ref().unwrap().output,
            depth,
            motion,
            history,
            quality,
            radius,
            timing,
        );
    }
    /// Settles the last render's accumulation: see `Kept::settle`.
    pub(crate) fn settle(&mut self, finished: bool) {
        self.temporal.settle(finished);
    }
    /// The last encoded frame's denoised visibility.
    pub(crate) fn output(&self) -> Option<&wgpu::TextureView> {
        self.temporal
            .output()
            .or_else(|| self.targets.as_ref().map(|t| &t.output))
    }
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn raw(&self) -> Option<&wgpu::TextureView> {
        self.targets.as_ref().map(|t| &t.working)
    }
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn depth_pyramid(&self) -> Option<&wgpu::Texture> {
        self.pyramid.as_ref().map(|p| p.depth.texture())
    }
    /// The last encoded frame's working depth: view depth in metres, its
    /// five mips filtered as XeGTAO filters them (`encode_depth_pyramid`).
    pub(crate) fn depth_view(&self) -> Option<&wgpu::TextureView> {
        self.pyramid.as_ref().map(|p| &p.depth)
    }
    /// Whether ambient occlusion has run here: its own targets exist, not
    /// only the depth pyramid another pass asked for.
    pub(crate) fn ran(&self) -> bool {
        self.targets.is_some()
    }
    /// The Hilbert index table the searches' noise reads (`hilbert_lut`).
    pub(crate) fn hilbert(&self) -> &wgpu::TextureView {
        &self.hilbert_lut
    }

    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("XeGTAO FP32"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&XE_GTAO]).into()),
        });
        let pipeline = |name| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(name),
                layout: None,
                module: &module,
                entry_point: Some(name),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let prefilter = pipeline(PREFILTER_ENTRY);
        let prefilter_mip4 = pipeline(PREFILTER_MIP4_ENTRY);
        let main = pipeline(MAIN_PASS_ENTRY);
        let denoise = pipeline(DENOISE_ENTRY);
        let groups = [&prefilter, &prefilter_mip4, &main, &denoise]
            .map(|pipeline| CachedGroup::new(pipeline.get_bind_group_layout(0)));
        Self {
            prefilter,
            prefilter_mip4,
            main,
            denoise,
            groups,
            pyramid_params: params_buffer(device, "XeGTAO depth prefilter constants"),
            params: params_buffer(device, "XeGTAO constants"),
            hilbert_lut: hilbert_lut(device, queue),
            pyramid: None,
            targets: None,
            temporal: temporal::Temporal::new(device),
        }
    }
    /// The depth pyramid and visibility of a frame, as the opaque stage
    /// encodes them in turn (`encode_depth_pyramid`, `encode_visibility`).
    #[cfg(all(test, not(target_arch = "wasm32")))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        depth: &wgpu::TextureView,
        normals: &wgpu::TextureView,
        view: &ViewUniform,
        quality: AmbientOcclusionQuality,
        radius: f32,
        noise_index: u32,
        timing: Option<&crate::timing::GpuTiming>,
    ) -> &wgpu::TextureView {
        self.encode_depth_pyramid(
            device,
            queue,
            encoder,
            depth,
            view,
            radius,
            (timing, TIMING_GROUP),
        );
        self.encode_visibility(
            device,
            queue,
            encoder,
            normals,
            view,
            quality,
            radius,
            noise_index,
            timing,
        )
    }

    /// XeGTAO_PrefilterDepths16x16 over `depth`, seen by `view`, its mips
    /// filtered for a search of `radius` metres: once a frame, for ambient
    /// occlusion at its radius, or for screen-space indirect light at its
    /// own where ambient occlusion is off, timed in that pass's group
    /// (`timing_group`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode_depth_pyramid(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        depth: &wgpu::TextureView,
        view: &ViewUniform,
        radius: f32,
        (timing, timing_group): (Option<&crate::timing::GpuTiming>, &'static str),
    ) {
        let size = [depth.texture().width(), depth.texture().height()];
        if self.pyramid.as_ref().is_none_or(|p| p.size != size) {
            self.pyramid = Some(Pyramid::new(device, size));
        }
        let params = params(size, view, AmbientOcclusionQuality::Off, radius, 0);
        crate::counters::write_buffer(queue, &self.pyramid_params, 0, bytemuck::bytes_of(&params));
        let pyramid = self.pyramid.as_ref().unwrap();
        let group = self.groups[0].get(
            device,
            "XeGTAO depth mips 0-3",
            &[
                (0, self.pyramid_params.as_entire_binding()),
                texture_entry(1, depth),
                texture_entry(3, &pyramid.mips[0]),
                texture_entry(7, &pyramid.mips[1]),
                texture_entry(8, &pyramid.mips[2]),
                texture_entry(9, &pyramid.mips[3]),
            ],
        );
        // An 8x8 group filters a 16x16 tile.
        dispatch_in(
            encoder,
            &self.prefilter,
            group,
            [size[0].div_ceil(16), size[1].div_ceil(16)],
            (timing, timing_group),
        );
        let group = self.groups[1].get(
            device,
            "XeGTAO depth mip 4",
            &[
                (0, self.pyramid_params.as_entire_binding()),
                texture_entry(2, &pyramid.mips[3]),
                texture_entry(10, &pyramid.mips[4]),
            ],
        );
        dispatch_in(
            encoder,
            &self.prefilter_mip4,
            group,
            size.map(|extent| (extent >> (DEPTH_MIPS - 1)).max(1).div_ceil(8)),
            (timing, timing_group),
        );
    }

    /// XeGTAO's horizon integration and denoise over the depth pyramid this
    /// frame built (`encode_depth_pyramid`, at the same `radius`) and
    /// `normals`, seen by `view`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode_visibility(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        normals: &wgpu::TextureView,
        view: &ViewUniform,
        quality: AmbientOcclusionQuality,
        radius: f32,
        noise_index: u32,
        timing: Option<&crate::timing::GpuTiming>,
    ) -> &wgpu::TextureView {
        let pyramid = self
            .pyramid
            .as_ref()
            .expect("the frame built the depth pyramid");
        let size = pyramid.size;
        if self
            .targets
            .as_ref()
            .is_none_or(|targets| targets.size != size)
        {
            self.targets = Some(Targets::new(device, size));
        }
        let targets = self.targets.as_ref().unwrap();
        let params = params(size, view, quality, radius, noise_index);
        crate::counters::write_buffer(queue, &self.params, 0, bytemuck::bytes_of(&params));
        let group = self.groups[2].get(
            device,
            "XeGTAO horizon integration",
            &[
                (0, self.params.as_entire_binding()),
                texture_entry(2, &pyramid.depth),
                texture_entry(4, normals),
                texture_entry(5, &targets.working),
                texture_entry(11, &self.hilbert_lut),
            ],
        );
        dispatch(
            encoder,
            &self.main,
            group,
            [size[0].div_ceil(8), size[1].div_ceil(8)],
            timing,
        );
        // Split the stage fields so the denoiser can update only its own cache.
        encode_denoise(
            &self.denoise,
            &self.params,
            &mut self.groups[3],
            device,
            encoder,
            &targets.working,
            &targets.output,
            size,
            timing,
        );
        &targets.output
    }
}
/// A uniform buffer holding one `Params`.
fn params_buffer(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some(label),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
    )
}
/// The passes' parameters for a frame of `size` seen by `view`, at
/// `quality`, with a search radius of `radius` metres, clamped to the radii
/// XeGTAO takes.
fn params(
    size: [u32; 2],
    view: &ViewUniform,
    quality: AmbientOcclusionQuality,
    radius: f32,
    noise_index: u32,
) -> Params {
    let (slices, steps) = match quality {
        AmbientOcclusionQuality::Off | AmbientOcclusionQuality::Low => (1, 2),
        AmbientOcclusionQuality::Medium => (2, 2),
        AmbientOcclusionQuality::High => (3, 3),
        AmbientOcclusionQuality::Ultra => (MOST_SLICES, MOST_STEPS),
    };
    let projection = view.projection;
    let mul = -projection[3][2];
    let mut add = projection[2][2];
    if mul * add < 0.0 {
        add = -add;
    }
    let tan_half = [1.0 / projection[0][0], 1.0 / projection[1][1]];
    let radius = if radius.is_nan() {
        MIN_RADIUS
    } else {
        radius.clamp(MIN_RADIUS, MAX_RADIUS)
    };
    Params {
        view: view.view,
        size,
        pixel_size: [1.0 / size[0] as f32, 1.0 / size[1] as f32],
        depth_unpack: [mul, add],
        ndc_mul: [2.0 * tan_half[0], -2.0 * tan_half[1]],
        // Depth and normals rasterize with the view's subpixel offset.
        // Undo that offset before reconstructing their view-space rays,
        // as a full inverse projection does (also post-fx's DFX-15).
        // ViewUniform::jitter stores half the NDC translation.
        ndc_add: [
            -tan_half[0] * (1.0 + 2.0 * view.jitter[0]),
            tan_half[1] * (1.0 - 2.0 * view.jitter[1]),
        ],
        radius,
        slices,
        steps,
        noise_index,
        padding: [0; 2],
    }
}
/// XeGTAO_Denoise's final pass from `working` into `output`, both
/// `size`, with the parameters the frame's `encode` wrote.
#[allow(clippy::too_many_arguments)]
fn encode_denoise(
    pipeline: &wgpu::ComputePipeline,
    params: &wgpu::Buffer,
    group: &mut CachedGroup,
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    working: &wgpu::TextureView,
    output: &wgpu::TextureView,
    size: [u32; 2],
    timing: Option<&crate::timing::GpuTiming>,
) {
    let group = group.get(
        device,
        "XeGTAO spatial denoise",
        &[
            (0, params.as_entire_binding()),
            texture_entry(5, output),
            texture_entry(6, working),
        ],
    );
    // Each invocation denoises two horizontally adjacent pixels.
    dispatch(
        encoder,
        pipeline,
        group,
        [size[0].div_ceil(16), size[1].div_ceil(8)],
        timing,
    );
}
/// `XeGTAO.h` `HilbertIndex`: the cell's place along the Hilbert curve
/// through the tile.
fn hilbert_index(mut x: u32, mut y: u32) -> u32 {
    let mut index = 0;
    let mut level = HILBERT_WIDTH / 2;
    while level > 0 {
        let region_x = u32::from(x & level > 0);
        let region_y = u32::from(y & level > 0);
        index += level * level * ((3 * region_x) ^ region_y);
        if region_y == 0 {
            if region_x == 1 {
                x = HILBERT_WIDTH - 1 - x;
                y = HILBERT_WIDTH - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        level /= 2;
    }
    index
}
/// The main pass's noise takes each pixel's Hilbert index from this R16Uint
/// table of `hilbert_index`, row-major, as vaGTAO's
/// `XE_GTAO_HILBERT_LUT_AVAILABLE` path does.
fn hilbert_lut(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let indices: Vec<u16> = (0..HILBERT_WIDTH * HILBERT_WIDTH)
        .map(|cell| hilbert_index(cell % HILBERT_WIDTH, cell / HILBERT_WIDTH) as u16)
        .collect();
    crate::counters::texture_init(
        device,
        queue,
        &wgpu::TextureDescriptor {
            label: Some("XeGTAO Hilbert indices"),
            size: wgpu::Extent3d {
                width: HILBERT_WIDTH,
                height: HILBERT_WIDTH,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(&indices),
    )
    .create_view(&Default::default())
}
fn texture_entry(binding: u32, view: &wgpu::TextureView) -> (u32, wgpu::BindingResource<'_>) {
    (binding, wgpu::BindingResource::TextureView(view))
}
/// Ambient occlusion's timing group.
pub(crate) const TIMING_GROUP: &str = "ambient occlusion";
fn dispatch(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    group: &wgpu::BindGroup,
    workgroups: [u32; 2],
    timing: Option<&crate::timing::GpuTiming>,
) {
    dispatch_in(encoder, pipeline, group, workgroups, (timing, TIMING_GROUP));
}
/// `dispatch`, timed in `timing_group`.
fn dispatch_in(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    group: &wgpu::BindGroup,
    workgroups: [u32; 2],
    (timing, timing_group): (Option<&crate::timing::GpuTiming>, &'static str),
) {
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("XeGTAO"),
        timestamp_writes: timing.and_then(|t| t.compute_pass(timing_group)),
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, group, &[]);
    pass.dispatch_workgroups(workgroups[0], workgroups[1], 1);
}
fn make_texture(
    device: &wgpu::Device,
    label: &str,
    extent: [u32; 2],
    format: wgpu::TextureFormat,
    mip_level_count: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: extent[0],
            height: extent[1],
            depth_or_array_layers: 1,
        },
        mip_level_count,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}
impl Pyramid {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let depth = make_texture(
            device,
            "XeGTAO depth pyramid",
            [size[0].max(16), size[1].max(16)],
            wgpu::TextureFormat::R32Float,
            DEPTH_MIPS as u32,
        );
        let mips = std::array::from_fn(|mip| {
            depth.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: mip as u32,
                mip_level_count: Some(1),
                ..Default::default()
            })
        });
        Self {
            size,
            depth: depth.create_view(&Default::default()),
            mips,
        }
    }
}
impl Targets {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let make = |label, format| {
            make_texture(device, label, size, format, 1).create_view(&Default::default())
        };
        Self {
            size,
            working: make(
                "XeGTAO packed working visibility",
                wgpu::TextureFormat::R32Uint,
            ),
            output: make("XeGTAO visibility", wgpu::TextureFormat::R32Uint),
        }
    }
}

/// The constants with WGSL twins.
#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 3] {
    [
        ("MOST_SLICES", MOST_SLICES),
        ("MOST_STEPS", MOST_STEPS),
        ("HILBERT_WIDTH", HILBERT_WIDTH),
    ]
    .map(|(name, value)| {
        crate::shading::layout_tests::Constant::new(
            "ambient_occlusion",
            name,
            naga::Literal::U32(value),
        )
    })
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    [crate::shading::layout_tests::mirror!(
        "ambient_occlusion",
        "Params",
        Params,
        [
            view,
            size,
            pixel_size,
            depth_unpack,
            ndc_mul,
            ndc_add,
            radius,
            slices,
            steps,
            noise_index,
        ]
    )]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod integration_tests;
