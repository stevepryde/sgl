//! The classification of the world-space reflection rays' tracing grid
//! (world_reflections_classify.wgsl; the architecture's Reflections), as
//! FidelityFX SSSR classifies its tiles: which tracing pixels trace a ray,
//! listed for the trace's indirect dispatch, and which tiles hold a
//! receiver, which the denoise passes read to skip the rest.
use crate::shading;
use crate::view::cached_group::CachedGroup;

/// Tracing pixels on a side of the classification's tiles, its workgroups
/// (WGSL `WORLD_TILE`).
pub(crate) const TILE: u32 = 8;
/// The trace's threads in a workgroup and its workgroups in a row of its
/// indirect dispatch, which the WGSL owns (`WORLD_TRACE_THREADS`,
/// `WORLD_GROUP_ROW`), twins for the tests.
#[cfg(test)]
pub(crate) const TRACE_THREADS: u32 = 64;
#[cfg(test)]
pub(crate) const GROUP_ROW: u32 = 64;

pub(crate) static CLASSIFY: shading::Module = shading::Module {
    name: "world_reflections_classify",
    source: include_str!("world_reflections_classify.wgsl"),
    deps: &[&super::COMMON],
};
/// The entry points the classification's pipelines are created with.
pub(crate) const CLASSIFY_ENTRY: &str = "world_classify";
pub(crate) const PREPARE_RAYS_ENTRY: &str = "world_prepare_rays";

/// `WorldRayCount` in world_reflections_classify.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct RayCount {
    rays: u32,
    groups: [u32; 3],
}
/// Where the ray count's rays and the trace's indirect arguments lie in it.
pub(super) const RAYS_OFFSET: wgpu::BufferAddress = std::mem::offset_of!(RayCount, rays) as u64;
pub(super) const GROUPS_OFFSET: wgpu::BufferAddress = std::mem::offset_of!(RayCount, groups) as u64;

/// The classification's outputs for a tracing grid of `reduced` pixels: the
/// ray list, a texel a ray, at most one a tracing pixel; the rays listed
/// with the trace's indirect arguments; and each tile's flag.
pub(super) struct Lists {
    pub rays: wgpu::TextureView,
    pub count: wgpu::Buffer,
    pub tiles: wgpu::Buffer,
}

impl Lists {
    pub fn new(device: &wgpu::Device, reduced: [u32; 2]) -> Self {
        let rays = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("world reflection ray list"),
                size: wgpu::Extent3d {
                    width: reduced[0],
                    height: reduced[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Uint,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let count = crate::counters::buffer(
            device,
            &wgpu::BufferDescriptor {
                label: Some("world reflection ray count"),
                size: std::mem::size_of::<RayCount>() as u64,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::INDIRECT
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            },
        );
        let tiles = reduced.map(|side| side.div_ceil(TILE));
        let tiles = crate::counters::buffer(
            device,
            &wgpu::BufferDescriptor {
                label: Some("world reflection tiles"),
                size: 4 * u64::from(tiles[0] * tiles[1]),
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            },
        );
        Self { rays, count, tiles }
    }
}

/// The classification's pipelines and their groups.
pub(super) struct Classify {
    classify: wgpu::ComputePipeline,
    prepare: wgpu::ComputePipeline,
    /// Group 0 and 3 of each.
    classify_groups: [CachedGroup; 2],
    prepare_groups: [CachedGroup; 2],
}

/// What the classification reads and writes: its group 0's targets (the
/// trace's radiance, direction and pdf, and length), list, count and
/// flags; its group 3's receiver depth, material and F0, parameters, the
/// screen-space method's result and the surface depth.
pub(super) struct Bindings<'a> {
    pub targets: [&'a wgpu::TextureView; 3],
    pub lists: &'a Lists,
    pub depth: &'a wgpu::TextureView,
    pub material: &'a wgpu::TextureView,
    pub f0: &'a wgpu::TextureView,
    pub params: &'a wgpu::Buffer,
    pub screen_space: &'a wgpu::TextureView,
    pub surface_depth: &'a wgpu::TextureView,
}

impl Classify {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world-space reflection classification"),
            source: wgpu::ShaderSource::Wgsl(shading::compose(&[&CLASSIFY]).into()),
        });
        let pipeline = |entry_point| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry_point),
                layout: None,
                module: &shader,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let groups = |pipeline: &wgpu::ComputePipeline| {
            [0, 3].map(|index| CachedGroup::new(pipeline.get_bind_group_layout(index)))
        };
        let classify = pipeline(CLASSIFY_ENTRY);
        let prepare = pipeline(PREPARE_RAYS_ENTRY);
        Self {
            classify_groups: groups(&classify),
            prepare_groups: groups(&prepare),
            classify,
            prepare,
        }
    }

    /// Records the classification over a tracing grid of `reduced` pixels
    /// into `encoder`, after clearing the count: the trace then dispatches
    /// over `lists.count`'s arguments, and its count is copied into the
    /// parameters (world.rs).
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        reduced: [u32; 2],
        bindings: Bindings<'_>,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let view = wgpu::BindingResource::TextureView;
        let lists = bindings.lists;
        encoder.clear_buffer(&lists.count, 0, None);
        let [indirect, direction_pdf, length] = bindings.targets;
        let [classify_0, classify_3] = &mut self.classify_groups;
        let [prepare_0, prepare_3] = &mut self.prepare_groups;
        let classify_0 = classify_0.get(
            device,
            "world reflection classification",
            &[
                (10, view(indirect)),
                (11, view(direction_pdf)),
                (12, view(length)),
                (13, view(&lists.rays)),
                (14, lists.count.as_entire_binding()),
                (15, lists.tiles.as_entire_binding()),
            ],
        );
        let classify_3 = classify_3.get(
            device,
            "world reflection classification receivers",
            &[
                (0, view(bindings.depth)),
                (2, view(bindings.material)),
                (3, view(bindings.f0)),
                (4, bindings.params.as_entire_binding()),
                (5, view(bindings.screen_space)),
                (7, view(bindings.surface_depth)),
            ],
        );
        let prepare_0 = prepare_0.get(
            device,
            "world reflection ray arguments",
            &[(14, lists.count.as_entire_binding())],
        );
        let prepare_3 = prepare_3.get(
            device,
            "world reflection ray arguments parameters",
            &[(4, bindings.params.as_entire_binding())],
        );
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("world reflection classification"),
            timestamp_writes: timing.and_then(|t| t.compute_pass("world reflection classify")),
        });
        pass.set_pipeline(&self.classify);
        pass.set_bind_group(0, classify_0, &[]);
        pass.set_bind_group(3, classify_3, &[]);
        pass.dispatch_workgroups(reduced[0].div_ceil(TILE), reduced[1].div_ceil(TILE), 1);
        pass.set_pipeline(&self.prepare);
        pass.set_bind_group(0, prepare_0, &[]);
        pass.set_bind_group(3, prepare_3, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
}

/// The constants with WGSL twins.
#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 4] {
    use crate::shading::layout_tests::Constant;
    [
        Constant::new(
            "world_reflections_classify",
            "WORLD_DOWNSCALE",
            naga::Literal::U32(super::DOWNSCALE),
        ),
        Constant::new(
            "world_reflections_classify",
            "WORLD_TILE",
            naga::Literal::U32(TILE),
        ),
        Constant::new(
            "world_reflections_classify",
            "WORLD_TRACE_THREADS",
            naga::Literal::U32(TRACE_THREADS),
        ),
        Constant::new(
            "world_reflections_classify",
            "WORLD_GROUP_ROW",
            naga::Literal::U32(GROUP_ROW),
        ),
    ]
}

/// `RayCount`'s layout against its WGSL struct.
#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    [crate::shading::layout_tests::mirror!(
        "world_reflections_classify",
        "WorldRayCount",
        RayCount,
        [rays, groups]
    )]
}
