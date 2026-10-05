//! The tests' standalone dispatch of the scene ray function set over either
//! path, the portable function set (`SCENE_RAYS_PORTABLE`) or the hardware
//! form's query module (`shading::ray_trace_root`): the scene at group 1,
//! its rays, hits and the function each ray takes at group 3, with the TLAS
//! on the hardware path.

/// The tests' dispatch: each ray through the function `query_limits.z`
/// names (`Function`), the scene at group 1, its rays and hits at group 3.
/// A pipeline composes it with the path's root.
pub(crate) static QUERY: crate::shading::Module = crate::shading::Module {
    name: "scene_rays_query",
    source: include_str!("query.wgsl"),
    deps: &[&crate::shading::SCENE_RAYS_PREDICATE],
};

/// The function a dispatch's rays take, as query.wgsl numbers them.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Function {
    /// `scene_trace_nearest`: its hit.
    Nearest = 0,
    /// `scene_segment_visible` over the ray's interval: a hit word of 1
    /// where it is visible.
    Visible = 1,
    /// `scene_trace_moving_except_receiver`, leaving the bound receiver.
    MovingNearest = 2,
    /// `scene_static_segment_visible_except_receiver`, leaving the bound
    /// receiver: 1 where it is visible.
    StaticVisible = 3,
    /// `scene_trace_nearest` decoded (`scene_decode_hit`): whether it hit,
    /// then its shading normal's bits, then its geometric normal and
    /// distance.
    Decoded = 4,
    /// `scene_trace_nearest_except_receiver`, leaving the bound receiver.
    NearestExceptReceiver = 5,
}

/// The tests' standalone dispatch over one path.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Query {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
    hardware: bool,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct SceneRayBindings {
    group: wgpu::BindGroup,
    count: u32,
}

#[cfg(not(target_arch = "wasm32"))]
impl Query {
    /// The portable path's dispatch.
    pub fn new(device: &wgpu::Device) -> Self {
        Self::with_path(device, None)
    }

    /// The dispatch over the path `form` takes: the hardware form's, or the
    /// portable path's for none.
    pub fn with_path(device: &wgpu::Device, form: Option<crate::shading::RayQueryForm>) -> Self {
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
        let mut entries = vec![
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
        ];
        if form.is_some() {
            entries.push(crate::shading::bind::tlas_entry());
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene ray dispatch IO"),
            entries: &entries,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("material-aware scene query"),
            source: wgpu::ShaderSource::Wgsl(
                crate::shading::compose(&[&QUERY, crate::shading::ray_trace_root(form)]).into(),
            ),
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
        Self {
            layout,
            pipeline,
            hardware: form.is_some(),
        }
    }

    /// `rays`' nearest hits as raster sides them, into `hits`.
    pub fn ray_bind_group(
        &self,
        device: &wgpu::Device,
        rays: &wgpu::Buffer,
        hits: &wgpu::Buffer,
    ) -> SceneRayBindings {
        self.bind(device, (rays, hits), (Function::Nearest, 0, [0; 2]), None)
    }

    /// `rays` through `function`, accepting `sides` (`SCENE_SIDES_*`) and
    /// leaving `receiver` (its instance's index plus one and its triangle's
    /// first index word, zero for none), into `hits`, on the hardware path
    /// through `tlas`.
    pub fn bind(
        &self,
        device: &wgpu::Device,
        (rays, hits): (&wgpu::Buffer, &wgpu::Buffer),
        (function, sides, receiver): (Function, u32, [u32; 2]),
        tlas: Option<&wgpu::Tlas>,
    ) -> SceneRayBindings {
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
        assert_eq!(
            tlas.is_some(),
            self.hardware,
            "the hardware path binds its TLAS"
        );
        let count =
            u32::try_from(rays.size() / 32).expect("ray count exceeds shader address range");
        let limits = crate::counters::buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("scene ray allocation bounds"),
                contents: bytemuck::cast_slice(&[
                    count,
                    sides,
                    function as u32,
                    count.div_ceil(64).min(65535) * 64,
                    receiver[0],
                    receiver[1],
                    0,
                    0,
                ]),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let mut entries = vec![
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
        ];
        if let Some(tlas) = tlas {
            entries.push(wgpu::BindGroupEntry {
                binding: crate::shading::bind::hardware::SCENE_TLAS,
                resource: tlas.as_binding(),
            });
        }
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene ray dispatch IO"),
            layout: &self.layout,
            entries: &entries,
        });
        SceneRayBindings { group, count }
    }

    /// The hits of `bindings`' rays in the scene bound by `scene` (group 1).
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
