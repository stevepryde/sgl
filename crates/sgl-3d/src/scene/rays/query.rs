//! The tests' standalone nearest-hit dispatch over `SCENE_RAYS_PORTABLE`:
//! the oracle for the BVH, with the scene at group 1 and its rays and hits at
//! group 3.

/// The tests' nearest-hit dispatch over `SCENE_RAYS_PORTABLE`: the scene at
/// group 1, its rays and hits at group 3.
pub(crate) static QUERY: crate::shading::Module = crate::shading::Module {
    name: "scene_rays_portable_query",
    source: include_str!("query.wgsl"),
    deps: &[&crate::shading::SCENE_RAYS_PORTABLE],
};

/// The tests' standalone nearest-hit dispatch: the oracle for the BVH.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Query {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct SceneRayBindings {
    group: wgpu::BindGroup,
    count: u32,
}

#[cfg(not(target_arch = "wasm32"))]
impl Query {
    pub fn new(device: &wgpu::Device) -> Self {
        let scene = crate::shading::bind::scene(device);
        let storage = |read_only| wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene ray dispatch IO"),
            entries: &[
                entry(0, storage(true)),
                entry(1, storage(false)),
                entry(
                    2,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("material-aware scene query"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&QUERY]).into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("nearest accepted scene face"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("scene query"),
                    bind_group_layouts: &[None, Some(&scene), None, Some(&layout)],
                    immediate_size: 0,
                }),
            ),
            module: &shader,
            entry_point: Some("scene_intersect"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { layout, pipeline }
    }

    pub fn ray_bind_group(
        &self,
        device: &wgpu::Device,
        rays: &wgpu::Buffer,
        hits: &wgpu::Buffer,
    ) -> SceneRayBindings {
        use wgpu::util::DeviceExt;
        assert_eq!(
            rays.size() % 32,
            0,
            "scene ray allocation must contain complete ray records"
        );
        assert_eq!(
            rays.size(),
            hits.size(),
            "one complete hit allocation per ray is required"
        );
        let count =
            u32::try_from(rays.size() / 32).expect("ray count exceeds shader address range");
        let limits = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scene ray allocation bounds"),
            contents: bytemuck::cast_slice(&[count, 0, 0, count.div_ceil(64).min(65535) * 64]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene ray dispatch IO"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: rays.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: hits.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: limits.as_entire_binding(),
                },
            ],
        });
        SceneRayBindings { group, count }
    }

    /// The nearest hits of `bindings`' rays in the scene bound by `scene`
    /// (group 1).
    pub fn trace(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        scene: &wgpu::BindGroup,
        bindings: &SceneRayBindings,
        count: u32,
    ) {
        assert_eq!(
            count, bindings.count,
            "ray dispatch must cover its bound allocation"
        );
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("current scene nearest accepted faces"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(1, scene, &[]);
        pass.set_bind_group(3, &bindings.group, &[]);
        let columns = count.div_ceil(64).min(65535);
        pass.dispatch_workgroups(columns, count.div_ceil(64).div_ceil(columns), 1);
    }
}
