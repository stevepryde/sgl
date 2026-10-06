//! Tiled probe culling (`probe_culling.wgsl`, the probe portion of Wicked
//! Engine's tiled light culling): each screen tile's baked specular probes
//! whose influence reaches the geometry it sees, which source completion
//! and composition read.
use crate::shading;
use crate::view::cached_group::CachedGroup;
use crate::view::reflection_camera;

pub(crate) static PROBE_CULLING: shading::Module = shading::Module {
    name: "probe_culling",
    source: include_str!("probe_culling.wgsl"),
    deps: &[&shading::PROBE_SAMPLING],
};
/// The entry point its pipeline is created with.
pub(crate) const MAIN_ENTRY: &str = "main";

/// Tiled probe culling's camera; matches `CullingCamera` in probe_culling.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CullingCamera {
    view: [[f32; 4]; 4],
    inverse_view: [[f32; 4]; 4],
    inverse_projection: [[f32; 4]; 4],
    z_near: [f32; 4],
}
impl CullingCamera {
    /// Wicked's culling works in a left-handed view space with depth forward;
    /// SGL's views are right-handed, so view depth is mirrored.
    fn new(camera: reflection_camera::Camera) -> Self {
        let mirror = glam::Mat4::from_scale(glam::Vec3::new(1., 1., -1.));
        let view = glam::Mat4::from_cols_array_2d(&camera.view);
        let inverse_projection = mirror * glam::Mat4::from_cols_array_2d(&camera.proj).inverse();
        let near = inverse_projection * glam::Vec4::new(0., 0., 1., 1.);
        Self {
            view: (mirror * view).to_cols_array_2d(),
            inverse_view: (view.inverse() * mirror).to_cols_array_2d(),
            inverse_projection: inverse_projection.to_cols_array_2d(),
            z_near: [near.z / near.w, 0., 0., 0.],
        }
    }
}

/// Culling tiles span `PROBE_TILE_SIZE` pixels on each side; each records a
/// 32-probe bucket per 32 probes of a full collection. probe_sampling.wgsl
/// declares both constants' WGSL twins.
const PROBE_TILE_SIZE: u32 = 32;
pub(super) const PROBE_BUCKETS: u32 = crate::baked_specular_probe::MAX_PROBES as u32 / 32;
fn tile_count(size: [u32; 2]) -> [u32; 2] {
    [
        size[0].div_ceil(PROBE_TILE_SIZE),
        size[1].div_ceil(PROBE_TILE_SIZE),
    ]
}
fn tiles(device: &wgpu::Device, size: [u32; 2]) -> wgpu::Buffer {
    let [x, y] = tile_count(size);
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some("specular probe tiles"),
            size: u64::from(x * y) * u64::from(PROBE_BUCKETS) * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        },
    )
}

/// Culls the probe collection per screen tile.
pub(super) struct ProbeCulling {
    pipeline: wgpu::ComputePipeline,
    group: CachedGroup,
    camera: wgpu::Buffer,
    /// Each tile's probe buckets, which completion and composition bind.
    pub tiles: wgpu::Buffer,
    /// The render size the tiles cover.
    size: [u32; 2],
}

impl ProbeCulling {
    /// Culling into the tiles of `size`.
    pub fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("specular probe tiled culling"),
            source: wgpu::ShaderSource::Wgsl(shading::compose(&[&PROBE_CULLING]).into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("specular probe tiled culling"),
            layout: None,
            module: &shader,
            entry_point: Some(MAIN_ENTRY),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            group: CachedGroup::new(pipeline.get_bind_group_layout(0)),
            pipeline,
            camera: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("specular probe culling camera"),
                    size: std::mem::size_of::<CullingCamera>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            tiles: tiles(device, size),
            size,
        }
    }

    pub fn resize(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        self.size = size;
        self.tiles = tiles(device, size);
    }

    /// Records each tile's probes in `tiles` (probe_culling.wgsl).
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        depth: &wgpu::TextureView,
        camera: reflection_camera::Camera,
        collection: &wgpu::Buffer,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        crate::counters::write_buffer(
            queue,
            &self.camera,
            0,
            bytemuck::bytes_of(&CullingCamera::new(camera)),
        );
        let resources = [
            (0, wgpu::BindingResource::TextureView(depth)),
            (1, self.camera.as_entire_binding()),
            (2, collection.as_entire_binding()),
            (3, self.tiles.as_entire_binding()),
        ];
        let group = self
            .group
            .get(device, "specular probe tiled culling", &resources);
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("specular probe tiled culling"),
            timestamp_writes: timing.and_then(|t| t.compute_pass("probe culling")),
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, group, &[]);
        let [x, y] = tile_count(self.size);
        pass.dispatch_workgroups(x, y, 1);
    }
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    use crate::shading::layout_tests::mirror;
    [mirror!(
        "probe_culling",
        "CullingCamera",
        CullingCamera,
        [view, inverse_view, inverse_projection, z_near]
    )]
}

/// The constants with WGSL twins.
#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 2] {
    use crate::shading::layout_tests::Constant;
    use naga::Literal::U32;
    [
        Constant::new("probe_culling", "PROBE_BUCKETS", U32(PROBE_BUCKETS)),
        Constant::new("probe_culling", "PROBE_TILE_SIZE", U32(PROBE_TILE_SIZE)),
    ]
}
