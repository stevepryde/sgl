//! Local exposure (`FrameInput::local_exposure`): the base layer the tone
//! map scales the contrast of, built from the image it is about to read
//! (`local_exposure_passes.wgsl`) and looked up in `tone_map()`
//! (`local_exposure.wgsl`).
//!
//! No engine RD-2 allows has local exposure to port, so this follows
//! published technique (D-63): Jasmin Patry, "Real-Time Samurai Cinema:
//! Lighting, Atmosphere, and Tonemapping in Ghost of Tsushima" (SIGGRAPH
//! 2021 Advances in Real-Time Rendering, slides 130 to 146), whose base is
//! 40% a bilateral grid of log luminance filled with linear slice weights
//! and 60% a wide Gaussian (radius 13 on 64 cells, upsampled by a cubic
//! B-spline), and Chen, Paris and Durand, "Real-time Edge-Aware Image
//! Processing with the Bilateral Grid" (SIGGRAPH 2007, sections 2.3, 3 and
//! 5.3), whose grid samples at its kernel's widths and blurs with five taps
//! an axis, at Durand and Dorsey's (SIGGRAPH 2002) sizes. No code is
//! ported.
//!
//! Passes, one compute pass of dispatches (timing group `local exposure`):
//! the build (one workgroup a cell, every texel visited once), the grid's
//! blur along x, y and slice, the means' Gaussian along x and y, and the
//! upsample. Targets: two RGBA16F grids of cells x slices, two RGBA16F
//! cell-mean images and the upsampled base, under 1 MB at any scene size.
//! They exist from the first frame whose contrasts are below 1 and are
//! released by the first at 1, 1 and at resize. No history.
use crate::shading::gbuffer::COLOR as HDR;

/// The constants, V mapping and lookup the passes and the tone map share.
pub(crate) static LOCAL_EXPOSURE: crate::shading::Module = crate::shading::Module {
    name: "local_exposure",
    source: include_str!("local_exposure.wgsl"),
    deps: &[&crate::shading::LUMINANCE],
};
/// The passes.
pub(crate) static LOCAL_EXPOSURE_PASSES: crate::shading::Module = crate::shading::Module {
    name: "local_exposure_passes",
    source: include_str!("local_exposure_passes.wgsl"),
    deps: &[&LOCAL_EXPOSURE, &crate::shading::SCENE_COLOR],
};
/// The entry points the passes' pipelines are created with.
pub(crate) const BUILD_ENTRY: &str = "local_exposure_build";
pub(crate) const BLUR_GRID_ENTRY: &str = "local_exposure_blur_grid";
pub(crate) const GAUSSIAN_ENTRY: &str = "local_exposure_gaussian";
pub(crate) const UPSAMPLE_ENTRY: &str = "local_exposure_upsample";

// The constants below are one owner each of the WGSL twins in
// `local_exposure.wgsl` and `local_exposure_passes.wgsl`, which the layout
// test ties to them; those only the shaders use are read by that test alone.

/// Grid cells along the frame's longer side: a spatial scale near 2% of the
/// image, Durand and Dorsey's, at Chen's sampling rate equal to it. Patry
/// used 64 by 32.
pub(crate) const CELLS: u32 = 64;
/// Slices over the range: 4/3 stop apart, Durand and Dorsey's range sigma
/// of 0.4 log10 at Chen's sampling rate.
pub(crate) const SLICES: u32 = 13;
/// The stops about middle grey the grid spans, either way: auto exposure's
/// -8..=8 (SGL3D's choice; Patry gives no range).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const RANGE: f32 = 8.;
/// The pivot large regions are drawn toward: middle grey after the frame's
/// exposure (SGL3D's choice; Patry's M is the midpoint log luminance).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const MIDDLE_GREY: f32 = 0.18;
/// Patry's blend: the bilateral grid's share of the base, the wide
/// Gaussian's the rest, against the grid's ringing on smooth gradients.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const BILATERAL_SHARE: f32 = 0.4;
/// The Gaussian's radius in cells (Patry's 13 taps on 64 by 32) and its
/// sigma, a third of the radius so the kernel ends near zero (SGL3D's
/// choice; Patry states the radius alone).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const GAUSSIAN_RADIUS: i32 = 13;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const GAUSSIAN_SIGMA: f32 = 13. / 3.;
/// The Gaussian's upsample each way (Patry's 64 by 32 to 256 by 128).
pub(crate) const UPSAMPLE: u32 = 4;
/// The build's walk of a cell is capped at `TILE_SIDE` x `TILE_SIDE`
/// texels, shared by its workgroup's `BUILD_THREADS` (AR-12); a larger
/// cell is walked at a uniform stride.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const TILE_SIDE: u32 = 128;
/// The sampled grid weight below which the base is the Gaussian alone.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const MIN_WEIGHT: f32 = 1e-4;
/// The build's invocations a cell, and the halvings that add their sums.
/// At 1920x1080 on an Apple M5 the build took 0.69 ms with one invocation
/// a cell (Patry reports 0.25 ms on a PS4), 0.44 ms with 64, 7.5 ms with
/// 128 (whose 13.8 KB of workgroup memory cut occupancy) and 0.32 to
/// 0.36 ms with 32, one SIMD group there.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const BUILD_THREADS: u32 = 32;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const BUILD_STEPS: u32 = BUILD_THREADS.ilog2();

/// The grid's and mean images' format: storage-writable and filterable on
/// every WebGPU device.
const FORMAT: wgpu::TextureFormat = HDR;

/// `LocalExposure` in `local_exposure.wgsl`: what the tone map's lookup
/// takes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct LocalExposureUniform {
    highlight_contrast: f32,
    shadow_contrast: f32,
    /// 0 while local exposure is off: the tone map takes no sample.
    enabled: u32,
    _padding: u32,
}

impl LocalExposureUniform {
    /// The lookup at `contrasts` (highlight, shadow), or off.
    pub fn new(contrasts: Option<[f32; 2]>) -> Self {
        let [highlight_contrast, shadow_contrast] = contrasts.unwrap_or([1., 1.]);
        Self {
            highlight_contrast,
            shadow_contrast,
            enabled: u32::from(contrasts.is_some()),
            _padding: 0,
        }
    }
}

/// The layout this module mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "local_exposure_passes",
        "LocalExposure",
        LocalExposureUniform,
        [highlight_contrast, shadow_contrast, enabled, _padding]
    )]
}

/// The constants this module shares with its shaders.
#[cfg(test)]
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    use crate::shading::layout_tests::Constant;
    use naga::Literal::{F32, I32, U32};
    let program = "local_exposure_passes";
    vec![
        Constant::new(program, "LOCAL_EXPOSURE_CELLS", U32(CELLS)),
        Constant::new(program, "LOCAL_EXPOSURE_SLICES", U32(SLICES)),
        Constant::new(program, "LOCAL_EXPOSURE_RANGE", F32(RANGE)),
        Constant::new(program, "LOCAL_EXPOSURE_MIDDLE_GREY", F32(MIDDLE_GREY)),
        Constant::new(
            program,
            "LOCAL_EXPOSURE_BILATERAL_SHARE",
            F32(BILATERAL_SHARE),
        ),
        Constant::new(
            program,
            "LOCAL_EXPOSURE_GAUSSIAN_RADIUS",
            I32(GAUSSIAN_RADIUS),
        ),
        Constant::new(
            program,
            "LOCAL_EXPOSURE_GAUSSIAN_SIGMA",
            F32(GAUSSIAN_SIGMA),
        ),
        Constant::new(program, "LOCAL_EXPOSURE_UPSAMPLE", U32(UPSAMPLE)),
        Constant::new(program, "LOCAL_EXPOSURE_TILE_SIDE", U32(TILE_SIDE)),
        Constant::new(program, "LOCAL_EXPOSURE_MIN_WEIGHT", F32(MIN_WEIGHT)),
        Constant::new(program, "LOCAL_EXPOSURE_BUILD_THREADS", U32(BUILD_THREADS)),
        Constant::new(program, "LOCAL_EXPOSURE_BUILD_STEPS", U32(BUILD_STEPS)),
    ]
}

/// The grid's cells for a scene of `size`: `CELLS` along the longer side
/// (fewer on a scene narrower than that, so no cell is empty) and as many
/// along the shorter as keep them square, at least one.
pub(crate) fn cells(size: [u32; 2]) -> [u32; 2] {
    let long = size[0].max(size[1]).max(1);
    let along = CELLS.min(long);
    let across = |side: u32| {
        if side == long {
            along
        } else {
            (along * side).div_ceil(long).clamp(1, side.max(1))
        }
    };
    [across(size[0]), across(size[1])]
}

/// The grid, means and base of one scene size.
struct Targets {
    scene_size: [u32; 2],
    cells: [u32; 2],
    /// The build's grid, then the y blur's.
    grid: wgpu::TextureView,
    /// The x blur's, then the slice blur's: the blurred grid.
    grid_blurred: wgpu::TextureView,
    /// The build's cell means, then the y Gaussian's (the x Gaussian's is
    /// held by the groups).
    mean: wgpu::TextureView,
    /// The upsampled base.
    base: wgpu::TextureView,
    /// The fixed passes' groups: grid blur x, y, slice; Gaussian x, y;
    /// upsample.
    groups: [wgpu::BindGroup; 6],
    /// The build's group for the last image and exposure views it read.
    build: Option<(wgpu::TextureView, wgpu::TextureView, wgpu::BindGroup)>,
}

#[derive(Default)]
pub(super) struct LocalExposure {
    pipelines: Option<Pipelines>,
    targets: Option<Targets>,
}

struct Pipelines {
    build: wgpu::ComputePipeline,
    blur_grid: [wgpu::ComputePipeline; 3],
    gaussian: [wgpu::ComputePipeline; 2],
    upsample: wgpu::ComputePipeline,
    build_layout: wgpu::BindGroupLayout,
    grid_layout: wgpu::BindGroupLayout,
    image_layout: wgpu::BindGroupLayout,
}

/// A compute-stage float texture entry of `dimension`.
fn texture_entry(
    binding: u32,
    dimension: wgpu::TextureViewDimension,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: dimension,
            multisampled: false,
        },
        count: None,
    }
}

/// A compute-stage write-only `FORMAT` storage texture entry of `dimension`.
fn storage_entry(
    binding: u32,
    dimension: wgpu::TextureViewDimension,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: FORMAT,
            view_dimension: dimension,
        },
        count: None,
    }
}

/// A group of `layout` binding `views` at their bindings.
fn group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    views: &[(u32, &wgpu::TextureView)],
) -> wgpu::BindGroup {
    let entries: Vec<_> = views
        .iter()
        .map(|&(binding, view)| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        })
        .collect();
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("local exposure"),
        layout,
        entries: &entries,
    })
}

impl Pipelines {
    fn new(device: &wgpu::Device) -> Self {
        use wgpu::TextureViewDimension::{D2, D3};
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("local exposure"),
            source: wgpu::ShaderSource::Wgsl(
                crate::shading::compose(&[&LOCAL_EXPOSURE_PASSES]).into(),
            ),
        });
        let layout = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            })
        };
        let build_layout = layout(
            "local exposure build",
            &[
                texture_entry(0, D2),
                texture_entry(1, D2),
                storage_entry(2, D3),
                storage_entry(3, D2),
            ],
        );
        let grid_layout = layout(
            "local exposure grid blur",
            &[texture_entry(4, D3), storage_entry(5, D3)],
        );
        let image_layout = layout(
            "local exposure image",
            &[texture_entry(6, D2), storage_entry(7, D2)],
        );
        let pipeline = |layout: &wgpu::BindGroupLayout, entry, axis: Option<u32>| {
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(entry),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            let constants = axis.map(|axis| [("LOCAL_EXPOSURE_AXIS", f64::from(axis))]);
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: constants.as_ref().map_or(&[], |c| c.as_slice()),
                    ..Default::default()
                },
                cache: None,
            })
        };
        Self {
            build: pipeline(&build_layout, BUILD_ENTRY, None),
            blur_grid: [0, 1, 2].map(|axis| pipeline(&grid_layout, BLUR_GRID_ENTRY, Some(axis))),
            gaussian: [0, 1].map(|axis| pipeline(&image_layout, GAUSSIAN_ENTRY, Some(axis))),
            upsample: pipeline(&image_layout, UPSAMPLE_ENTRY, None),
            build_layout,
            grid_layout,
            image_layout,
        }
    }

    fn targets(&self, device: &wgpu::Device, scene_size: [u32; 2]) -> Targets {
        let cells = cells(scene_size);
        let texture = |label, [width, height]: [u32; 2], depth, dimension| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: depth,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension,
                    format: FORMAT,
                    usage: wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        use wgpu::TextureDimension::{D2, D3};
        let grid = texture("local exposure grid", cells, SLICES, D3);
        let grid_blurred = texture("local exposure blurred grid", cells, SLICES, D3);
        let mean = texture("local exposure cell means", cells, 1, D2);
        let mean_blurred = texture("local exposure blurred cell means", cells, 1, D2);
        let base = texture("local exposure base", cells.map(|c| c * UPSAMPLE), 1, D2);
        let groups = [
            group(device, &self.grid_layout, &[(4, &grid), (5, &grid_blurred)]),
            group(device, &self.grid_layout, &[(4, &grid_blurred), (5, &grid)]),
            group(device, &self.grid_layout, &[(4, &grid), (5, &grid_blurred)]),
            group(
                device,
                &self.image_layout,
                &[(6, &mean), (7, &mean_blurred)],
            ),
            group(
                device,
                &self.image_layout,
                &[(6, &mean_blurred), (7, &mean)],
            ),
            group(device, &self.image_layout, &[(6, &mean), (7, &base)]),
        ];
        Targets {
            scene_size,
            cells,
            grid,
            grid_blurred,
            mean,
            base,
            groups,
            build: None,
        }
    }
}

impl LocalExposure {
    /// Whether any of its targets is held.
    #[cfg(test)]
    pub fn holds_targets(&self) -> bool {
        self.targets.is_some()
    }

    /// Releases size-dependent targets and keeps the compiled pipelines.
    pub fn release(&mut self) {
        self.targets = None;
    }

    /// The base layer of `scene`, the image the tone map is about to read
    /// at `scene_size`, under the frame's `exposure`: the blurred grid and
    /// the upsampled Gaussian, which the tone map's lookup samples.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        scene: &wgpu::TextureView,
        exposure: &wgpu::TextureView,
        scene_size: [u32; 2],
        timing: Option<&crate::timing::GpuTiming>,
    ) -> (wgpu::TextureView, wgpu::TextureView) {
        let pipelines = self.pipelines.get_or_insert_with(|| Pipelines::new(device));
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.scene_size != scene_size)
        {
            self.targets = None;
        }
        let targets = self
            .targets
            .get_or_insert_with(|| pipelines.targets(device, scene_size));
        let build = match &targets.build {
            Some((s, e, group)) if s == scene && e == exposure => group.clone(),
            _ => {
                let build = group(
                    device,
                    &pipelines.build_layout,
                    &[
                        (0, scene),
                        (1, exposure),
                        (2, &targets.grid),
                        (3, &targets.mean),
                    ],
                );
                targets.build = Some((scene.clone(), exposure.clone(), build.clone()));
                build
            }
        };
        let [x, y] = targets.cells;
        let base = [x * UPSAMPLE, y * UPSAMPLE];
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("local exposure"),
            timestamp_writes: timing.and_then(|t| t.compute_pass("local exposure")),
        });
        pass.set_pipeline(&pipelines.build);
        pass.set_bind_group(0, &build, &[]);
        pass.dispatch_workgroups(x, y, 1);
        for (pipeline, group) in pipelines.blur_grid.iter().zip(&targets.groups[..3]) {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(x.div_ceil(8), y.div_ceil(8), SLICES);
        }
        for (pipeline, group) in pipelines.gaussian.iter().zip(&targets.groups[3..5]) {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(x.div_ceil(8), y.div_ceil(8), 1);
        }
        pass.set_pipeline(&pipelines.upsample);
        pass.set_bind_group(0, &targets.groups[5], &[]);
        pass.dispatch_workgroups(base[0].div_ceil(8), base[1].div_ceil(8), 1);
        drop(pass);
        (targets.grid_blurred.clone(), targets.base.clone())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
