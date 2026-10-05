//! Current-frame FP32 XeGTAO scalar visibility. See ambient_occlusion/README.md.
use crate::{settings::AmbientOcclusionQuality, shading::uniforms::ViewUniform};

/// XeGTAO's passes.
pub(crate) static XE_GTAO: crate::shading::Module = crate::shading::Module {
    name: "ambient_occlusion",
    source: include_str!("ambient_occlusion.wgsl"),
    deps: &[&crate::shading::GBUFFER],
};

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
    mip: u32,
    padding: [u32; 2],
}
struct Targets {
    size: [u32; 2],
    depth: wgpu::TextureView,
    mips: [wgpu::TextureView; 5],
    working: wgpu::TextureView,
    edges: wgpu::TextureView,
    output: wgpu::TextureView,
}
pub(crate) struct AmbientOcclusion {
    prefilter: wgpu::ComputePipeline,
    main: wgpu::ComputePipeline,
    denoise: wgpu::ComputePipeline,
    params: [wgpu::Buffer; 5],
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

    pub(crate) fn new(device: &wgpu::Device) -> Self {
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
            prefilter: pipeline("prefilter"),
            main: pipeline("main_pass"),
            denoise: pipeline("denoise"),
            params: std::array::from_fn(|_| {
                crate::counters::buffer(
                    device,
                    &wgpu::BufferDescriptor {
                        label: Some("XeGTAO constants"),
                        size: std::mem::size_of::<Params>() as u64,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    },
                )
            }),
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
        for mip in 0..5 {
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
                mip: mip as u32,
                padding: [0; 2],
            };
            crate::counters::write_buffer(queue, &self.params[mip], 0, bytemuck::bytes_of(&params));
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("XeGTAO depth mip"),
                layout: &self.prefilter.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.params[mip].as_entire_binding(),
                    },
                    texture_entry(1, depth),
                    texture_entry(2, &targets.mips[if mip == 0 { 4 } else { mip - 1 }]),
                    texture_entry(3, &targets.mips[mip]),
                ],
            });
            dispatch(
                encoder,
                &self.prefilter,
                &group,
                [(size[0] >> mip).max(1), (size[1] >> mip).max(1)],
                timing,
            );
        }
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("XeGTAO horizon integration"),
            layout: &self.main.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params[0].as_entire_binding(),
                },
                texture_entry(2, &targets.depth),
                texture_entry(4, normals),
                texture_entry(5, &targets.working),
                texture_entry(6, &targets.edges),
            ],
        });
        dispatch(encoder, &self.main, &group, size, timing);
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("XeGTAO spatial denoise"),
            layout: &self.denoise.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params[0].as_entire_binding(),
                },
                texture_entry(5, &targets.output),
                texture_entry(7, &targets.working),
                texture_entry(8, &targets.edges),
            ],
        });
        dispatch(encoder, &self.denoise, &group, size, timing);
        &targets.output
    }
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
    size: [u32; 2],
    timing: Option<&crate::timing::GpuTiming>,
) {
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("XeGTAO"),
        timestamp_writes: timing.and_then(|t| t.compute_pass("ambient occlusion")),
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, group, &[]);
    pass.dispatch_workgroups(size[0].div_ceil(8), size[1].div_ceil(8), 1);
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
            5,
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
            edges: make_texture(
                "XeGTAO packed edges",
                size,
                wgpu::TextureFormat::R32Float,
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
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 2] {
    [("MOST_SLICES", MOST_SLICES), ("MOST_STEPS", MOST_STEPS)].map(|(name, value)| {
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
            mip,
            padding,
        ]
    )]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod integration_tests;
