//! Current-frame FP32 XeGTAO scalar visibility. See ambient_occlusion/README.md.
use crate::{settings::AmbientOcclusionQuality, shading::uniforms::ViewUniform};

/// XeGTAO's passes.
pub(crate) static XE_GTAO: crate::shading::Module = crate::shading::Module {
    name: "ambient_occlusion",
    source: include_str!("ambient_occlusion.wgsl"),
    deps: &[&crate::shading::GBUFFER],
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
const HILBERT_WIDTH: u32 = 64;

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
    padding: [u32; 3],
}
struct Targets {
    size: [u32; 2],
    depth: wgpu::TextureView,
    mips: [wgpu::TextureView; DEPTH_MIPS],
    working: wgpu::TextureView,
    output: wgpu::TextureView,
}
pub(crate) struct AmbientOcclusion {
    prefilter: wgpu::ComputePipeline,
    prefilter_mip4: wgpu::ComputePipeline,
    main: wgpu::ComputePipeline,
    denoise: wgpu::ComputePipeline,
    params: wgpu::Buffer,
    hilbert_lut: wgpu::TextureView,
    targets: Option<Targets>,
}
impl AmbientOcclusion {
    /// The last encoded frame's denoised visibility.
    pub(crate) fn output(&self) -> Option<&wgpu::TextureView> {
        self.targets.as_ref().map(|t| &t.output)
    }
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn raw(&self) -> Option<&wgpu::TextureView> {
        self.targets.as_ref().map(|t| &t.working)
    }
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn depth_pyramid(&self) -> Option<&wgpu::Texture> {
        self.targets.as_ref().map(|t| t.depth.texture())
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
        Self {
            prefilter: pipeline(PREFILTER_ENTRY),
            prefilter_mip4: pipeline(PREFILTER_MIP4_ENTRY),
            main: pipeline(MAIN_PASS_ENTRY),
            denoise: pipeline(DENOISE_ENTRY),
            params: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("XeGTAO constants"),
                    size: std::mem::size_of::<Params>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            hilbert_lut: hilbert_lut(device, queue),
            targets: None,
        }
    }
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
        timing: Option<&crate::timing::GpuTiming>,
    ) -> &wgpu::TextureView {
        let size = [depth.texture().width(), depth.texture().height()];
        if self
            .targets
            .as_ref()
            .is_none_or(|targets| targets.size != size)
        {
            self.targets = Some(Targets::new(device, size));
        }
        let targets = self.targets.as_ref().unwrap();
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
        let params = Params {
            view: view.view,
            size,
            pixel_size: [1.0 / size[0] as f32, 1.0 / size[1] as f32],
            depth_unpack: [mul, add],
            ndc_mul: [2.0 * tan_half[0], -2.0 * tan_half[1]],
            ndc_add: [-tan_half[0], tan_half[1]],
            radius,
            slices,
            steps,
            padding: [0; 3],
        };
        crate::counters::write_buffer(queue, &self.params, 0, bytemuck::bytes_of(&params));
        let params_entry = || wgpu::BindGroupEntry {
            binding: 0,
            resource: self.params.as_entire_binding(),
        };
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("XeGTAO depth mips 0-3"),
            layout: &self.prefilter.get_bind_group_layout(0),
            entries: &[
                params_entry(),
                texture_entry(1, depth),
                texture_entry(3, &targets.mips[0]),
                texture_entry(7, &targets.mips[1]),
                texture_entry(8, &targets.mips[2]),
                texture_entry(9, &targets.mips[3]),
            ],
        });
        // An 8x8 group filters a 16x16 tile.
        dispatch(
            encoder,
            &self.prefilter,
            &group,
            [size[0].div_ceil(16), size[1].div_ceil(16)],
            timing,
        );
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("XeGTAO depth mip 4"),
            layout: &self.prefilter_mip4.get_bind_group_layout(0),
            entries: &[
                params_entry(),
                texture_entry(2, &targets.mips[3]),
                texture_entry(10, &targets.mips[4]),
            ],
        });
        dispatch(
            encoder,
            &self.prefilter_mip4,
            &group,
            size.map(|extent| (extent >> (DEPTH_MIPS - 1)).max(1).div_ceil(8)),
            timing,
        );
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("XeGTAO horizon integration"),
            layout: &self.main.get_bind_group_layout(0),
            entries: &[
                params_entry(),
                texture_entry(2, &targets.depth),
                texture_entry(4, normals),
                texture_entry(5, &targets.working),
                texture_entry(11, &self.hilbert_lut),
            ],
        });
        dispatch(
            encoder,
            &self.main,
            &group,
            [size[0].div_ceil(8), size[1].div_ceil(8)],
            timing,
        );
        self.encode_denoise(
            device,
            encoder,
            &targets.working,
            &targets.output,
            size,
            timing,
        );
        &targets.output
    }
    /// XeGTAO_Denoise's final pass from `working` into `output`, both
    /// `size`, with the parameters the frame's `encode` wrote.
    fn encode_denoise(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        working: &wgpu::TextureView,
        output: &wgpu::TextureView,
        size: [u32; 2],
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("XeGTAO spatial denoise"),
            layout: &self.denoise.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params.as_entire_binding(),
                },
                texture_entry(5, output),
                texture_entry(6, working),
            ],
        });
        // Each invocation denoises two horizontally adjacent pixels.
        dispatch(
            encoder,
            &self.denoise,
            &group,
            [size[0].div_ceil(16), size[1].div_ceil(8)],
            timing,
        );
    }
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
fn texture_entry(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
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
        label: Some("XeGTAO"),
        timestamp_writes: timing.and_then(|t| t.compute_pass("ambient occlusion")),
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, group, &[]);
    pass.dispatch_workgroups(workgroups[0], workgroups[1], 1);
}
impl Targets {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let make_texture = |label, extent: [u32; 2], format, mip_level_count| {
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
        };
        let depth = make_texture(
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
            working: make_texture(
                "XeGTAO packed working visibility",
                size,
                wgpu::TextureFormat::R32Uint,
                1,
            )
            .create_view(&Default::default()),
            output: make_texture("XeGTAO visibility", size, wgpu::TextureFormat::R32Uint, 1)
                .create_view(&Default::default()),
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
        ]
    )]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod integration_tests;
