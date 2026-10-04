//! Source completion: ambient occlusion of opaque receivers' ambient diffuse,
//! and their environment and probe specular from the probes that tiled
//! culling finds for each screen tile; and the composition that blends a
//! screen-space method's reflections over it.
use super::cached_group::CachedGroup;
use crate::shading;
use crate::view::bindings::FogVolume;
use crate::view::reflection_camera;
use wgpu::util::DeviceExt;

/// Source completion (`main`) and screen-space composition
/// (`fullscreen_vs`, `compose_screen_space`).
pub(crate) static COMPLETION: shading::Module = shading::Module {
    name: "reflection_source",
    source: include_str!("source.wgsl"),
    deps: &[
        &shading::GBUFFER,
        &shading::PROBE_SAMPLING,
        &shading::SPECULAR_LOBES,
        &shading::PROBE_COLLECTION,
        &shading::FULLSCREEN,
        &shading::FOG,
    ],
};
pub(crate) static PROBE_CULLING: shading::Module = shading::Module {
    name: "probe_culling",
    source: include_str!("probe_culling.wgsl"),
    deps: &[&shading::PROBE_SAMPLING],
};

/// Completion's and composition's reflection environment; matches
/// `ReflectionEnvironment` in source.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ReflectionEnvironment {
    /// The prefiltered sky's yaw.
    pub yaw: f32,
    /// The prefiltered sky's intensity.
    pub intensity: f32,
    /// The width of the screen-space method's fade below its cutoff, in
    /// perceptual roughness.
    pub fade: f32,
    /// The alpha roughness below which the screen-space method traces a
    /// lobe; 0 traces none.
    pub traced: f32,
}

/// A reflection environment of `rotation` and `strength` with no lobe traced.
pub(crate) fn environment_uniform(
    device: &wgpu::Device,
    rotation: f32,
    strength: f32,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("application reflection environment intensity"),
        contents: bytemuck::bytes_of(&ReflectionEnvironment {
            yaw: rotation,
            intensity: strength,
            fade: 0.,
            traced: 0.,
        }),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

pub(crate) struct Environment<'a> {
    pub sky: &'a wgpu::TextureView,
    pub sampler: &'a wgpu::Sampler,
    pub parameters: &'a wgpu::Buffer,
    pub material: &'a wgpu::TextureView,
    pub baked: &'a wgpu::TextureView,
    pub collection: &'a wgpu::Buffer,
}
pub(crate) struct Inputs<'a> {
    /// The frame's fog volume and its sampler.
    pub fog: FogVolume<'a>,
    /// How completion and composition fog, while the frame has fog.
    pub frame_fog: Option<SourceFog>,
    pub camera: reflection_camera::Camera,
    pub scene: &'a wgpu::TextureView,
    /// The ambient diffuse within `scene` before occlusion.
    pub ambient: &'a wgpu::TextureView,
    /// Complete opaque beauty.
    pub output: &'a wgpu::TextureView,
    pub normal: &'a wgpu::TextureView,
    pub anisotropy: &'a wgpu::TextureView,
    pub f0: &'a wgpu::TextureView,
    pub depth: &'a wgpu::TextureView,
    /// Lit group 0's lookup tables, for the DFG table.
    pub lookup_tables: &'a wgpu::TextureView,
    /// Ambient visibility that occludes environment and probe specular,
    /// and ambient diffuse in the `diffuse_occlusion` variant.
    pub ambient_occlusion: &'a wgpu::TextureView,
    pub environment: Environment<'a>,
}

/// Source completion's camera; matches `SourceCamera` in source.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SourceCamera {
    inverse: [[f32; 4]; 4],
    eye: [f32; 4],
    fog: [f32; 4],
    forward: [f32; 3],
    flags: u32,
}
/// `SourceCamera::flags`: completion and composition apply the fog.
const SOURCE_FOG: u32 = 1;

/// The frame's fog as completion and composition apply it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SourceFog {
    /// One over the fog volume's length and over its detail spread.
    pub inverse_length: f32,
    pub inverse_detail_spread: f32,
    /// The share of its fog the sky takes, 0..=1 (`Fog::sky_affect`).
    pub sky_affect: f32,
}

impl SourceCamera {
    /// `camera`, fogged by `fog`.
    fn new(camera: reflection_camera::Camera, fog: Option<SourceFog>) -> Self {
        let forward = -glam::Mat4::from_cols_array_2d(&camera.view)
            .row(2)
            .truncate();
        Self {
            inverse: camera.inverse_view_proj,
            eye: camera.camera_position,
            fog: fog.map_or([0.; 4], |fog| {
                [
                    fog.inverse_length,
                    fog.inverse_detail_spread,
                    fog.sky_affect,
                    0.,
                ]
            }),
            forward: forward.to_array(),
            flags: if fog.is_some() { SOURCE_FOG } else { 0 },
        }
    }
}

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
const PROBE_BUCKETS: u32 = crate::baked_specular_probe::MAX_PROBES as u32 / 32;
fn tile_count(size: [u32; 2]) -> [u32; 2] {
    [
        size[0].div_ceil(PROBE_TILE_SIZE),
        size[1].div_ceil(PROBE_TILE_SIZE),
    ]
}
fn tiles(device: &wgpu::Device, size: [u32; 2]) -> wgpu::Buffer {
    let [x, y] = tile_count(size);
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("specular probe tiles"),
        size: u64::from(x * y) * u64::from(PROBE_BUCKETS) * 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

/// What completion and composition are compiled for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Variant {
    /// They add environment and probe specular (off only as a diagnostics
    /// layer).
    pub environment: bool,
    /// Completion writes the incident radiance, which only a screen-space
    /// method reads.
    pub incident: bool,
    /// Completion occludes receivers' ambient diffuse by the ambient
    /// visibility: while ambient occlusion runs.
    pub diffuse_occlusion: bool,
}

/// Source completion and screen-space composition compiled for one variant,
/// with their groups.
struct Completion {
    pipeline: wgpu::ComputePipeline,
    group: CachedGroup,
    /// Adds a screen-space method's traced lobes into the complete beauty.
    compose: wgpu::RenderPipeline,
    compose_group: CachedGroup,
}

/// Source completion and screen-space composition for `variant`.
fn completion(device: &wgpu::Device, variant: Variant) -> Completion {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("reflection source completion"),
        source: wgpu::ShaderSource::Wgsl(shading::compose(&[&COMPLETION]).into()),
    });
    let constants = [
        (
            "application_environment_enabled",
            f64::from(u8::from(variant.environment)),
        ),
        (
            "incident_radiance_enabled",
            f64::from(u8::from(variant.incident)),
        ),
        (
            "diffuse_occlusion_enabled",
            f64::from(u8::from(variant.diffuse_occlusion)),
        ),
    ];
    let compilation_options = || wgpu::PipelineCompilationOptions {
        constants: &constants,
        ..Default::default()
    };
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("reflection source completion"),
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: compilation_options(),
        cache: None,
    });
    let compose = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("screen-space reflection composition"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("fullscreen_vs"),
            compilation_options: compilation_options(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("compose_screen_space"),
            compilation_options: compilation_options(),
            targets: &[Some(wgpu::ColorTargetState {
                format: crate::shading::gbuffer::COLOR,
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::One,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent::REPLACE,
                }),
                write_mask: wgpu::ColorWrites::COLOR,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    Completion {
        group: CachedGroup::new(pipeline.get_bind_group_layout(0)),
        pipeline,
        compose_group: CachedGroup::new(compose.get_bind_group_layout(0)),
        compose,
    }
}

/// Completes the forward pass by adding each receiver's environment specular.
pub(crate) struct ReflectionSource {
    /// Culls the probe collection per screen tile before completion.
    culling: wgpu::ComputePipeline,
    culling_group: CachedGroup,
    culling_camera: wgpu::Buffer,
    tiles: wgpu::Buffer,
    completion: Completion,
    /// What `completion` is compiled for.
    variant: Variant,
    camera: wgpu::Buffer,
    /// The render size completion covers.
    size: [u32; 2],
    /// Incident radiance for SSR, including source environment/probe
    /// specular: at the render size while completion writes it, else a 1x1
    /// stand-in that completion binds and does not write.
    pub(super) incident: wgpu::TextureView,
    /// Bound as world-space reflections while none are traced.
    no_world: wgpu::TextureView,
}
impl ReflectionSource {
    /// Culling, and completion and composition compiled for `variant`, at
    /// `size`.
    pub fn new(device: &wgpu::Device, size: [u32; 2], variant: Variant) -> Self {
        let culling = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("specular probe tiled culling"),
            source: wgpu::ShaderSource::Wgsl(shading::compose(&[&PROBE_CULLING]).into()),
        });
        let culling = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("specular probe tiled culling"),
            layout: None,
            module: &culling,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let mut source = Self {
            culling_group: CachedGroup::new(culling.get_bind_group_layout(0)),
            culling,
            culling_camera: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("specular probe culling camera"),
                size: std::mem::size_of::<CullingCamera>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            tiles: tiles(device, size),
            completion: completion(device, variant),
            variant,
            camera: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("reflection source camera"),
                size: std::mem::size_of::<SourceCamera>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            size,
            incident: Self::target(device, [1, 1], "incident radiance"),
            no_world: Self::target(device, [1, 1], "no world-space reflections"),
        };
        source.fit_incident(device);
        source
    }
    fn target(device: &wgpu::Device, size: [u32; 2], label: &str) -> wgpu::TextureView {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: crate::shading::gbuffer::COLOR,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
            .create_view(&Default::default())
    }
    /// Rebuilds completion and composition when `variant` changes, and
    /// sizes the incident radiance for it.
    pub fn use_variant(&mut self, device: &wgpu::Device, variant: Variant) {
        if self.variant != variant {
            self.completion = completion(device, variant);
            self.variant = variant;
        }
        self.fit_incident(device);
    }
    pub fn resize(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        self.size = size;
        self.tiles = tiles(device, size);
        self.fit_incident(device);
    }
    /// The incident radiance completion wrote; `None` when its variant
    /// writes none.
    pub fn incident(&self) -> Option<&wgpu::TextureView> {
        self.variant.incident.then_some(&self.incident)
    }
    /// Reallocates the incident radiance when its size differs from the
    /// variant's: the render size while completion writes it, else 1x1.
    fn fit_incident(&mut self, device: &wgpu::Device) {
        let size = if self.variant.incident {
            self.size
        } else {
            [1, 1]
        };
        let texture = self.incident.texture();
        if [texture.width(), texture.height()] != size {
            self.incident = Self::target(device, size, "incident radiance");
        }
    }
    pub fn encode(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: Inputs<'_>,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        queue.write_buffer(
            &self.camera,
            0,
            bytemuck::bytes_of(&SourceCamera::new(input.camera, input.frame_fog)),
        );
        self.cull(
            encoder,
            device,
            queue,
            input.depth,
            input.camera,
            input.environment.collection,
            timing,
        );
        let texture = wgpu::BindingResource::TextureView;
        let fog = input.fog;
        let resources = [
            (0, texture(input.scene)),
            (2, texture(input.output)),
            (3, texture(input.normal)),
            (4, texture(input.f0)),
            (5, texture(input.depth)),
            (6, self.camera.as_entire_binding()),
            (7, texture(input.lookup_tables)),
            (8, texture(input.environment.sky)),
            (
                10,
                wgpu::BindingResource::Sampler(input.environment.sampler),
            ),
            (12, input.environment.parameters.as_entire_binding()),
            (13, texture(input.environment.material)),
            (14, texture(&self.incident)),
            (17, texture(input.anisotropy)),
            (18, texture(input.environment.baked)),
            (19, input.environment.collection.as_entire_binding()),
            (21, self.tiles.as_entire_binding()),
            (22, texture(input.ambient_occlusion)),
            (24, texture(input.ambient)),
            (25, texture(fog.view)),
            (26, wgpu::BindingResource::Sampler(fog.sampler)),
        ];
        let group = self
            .completion
            .group
            .get(device, "reflection source inputs", &resources);
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("reflection source completion"),
            timestamp_writes: timing.and_then(|t| t.compute_pass("reflection source completion")),
        });
        pass.set_pipeline(&self.completion.pipeline);
        pass.set_bind_group(0, group, &[]);
        let [width, height] = self.size;
        pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
    }
}

impl ReflectionSource {
    /// Records each tile's probes in `tiles` (probe_culling.wgsl).
    #[allow(clippy::too_many_arguments)]
    fn cull(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        depth: &wgpu::TextureView,
        camera: reflection_camera::Camera,
        collection: &wgpu::Buffer,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        queue.write_buffer(
            &self.culling_camera,
            0,
            bytemuck::bytes_of(&CullingCamera::new(camera)),
        );
        let resources = [
            (0, wgpu::BindingResource::TextureView(depth)),
            (1, self.culling_camera.as_entire_binding()),
            (2, collection.as_entire_binding()),
            (3, self.tiles.as_entire_binding()),
        ];
        let group = self
            .culling_group
            .get(device, "specular probe tiled culling", &resources);
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("specular probe tiled culling"),
            timestamp_writes: timing.and_then(|t| t.compute_pass("probe culling")),
        });
        pass.set_pipeline(&self.culling);
        pass.set_bind_group(0, group, &[]);
        let [x, y] = tile_count(self.size);
        pass.dispatch_workgroups(x, y, 1);
    }

    /// Adds each traced lobe's specular into `input.output`: its split-sum
    /// response times `screen_space` (rgb radiance, a confidence) blended over
    /// the receiver's environment and probe specular, fogged as completion
    /// fogs, with `world_space` filling its misses when world-space rays ran;
    /// where `surface_depth` is nearer than the opaque depth (under a blended
    /// receiver, whose `screen_space` is), the environment and probe
    /// specular alone. `encode` must have run this frame with the same
    /// inputs.
    #[allow(clippy::too_many_arguments)]
    pub fn compose(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        input: Inputs<'_>,
        surface_depth: &wgpu::TextureView,
        screen_space: &wgpu::TextureView,
        world_space: Option<&wgpu::TextureView>,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let world_space = world_space.unwrap_or(&self.no_world);
        let texture = wgpu::BindingResource::TextureView;
        let fog = input.fog;
        let resources = [
            (3, texture(input.normal)),
            (4, texture(input.f0)),
            (5, texture(input.depth)),
            (6, self.camera.as_entire_binding()),
            (7, texture(input.lookup_tables)),
            (8, texture(input.environment.sky)),
            (
                10,
                wgpu::BindingResource::Sampler(input.environment.sampler),
            ),
            (12, input.environment.parameters.as_entire_binding()),
            (13, texture(input.environment.material)),
            (17, texture(input.anisotropy)),
            (18, texture(input.environment.baked)),
            (19, input.environment.collection.as_entire_binding()),
            (20, texture(screen_space)),
            (21, self.tiles.as_entire_binding()),
            (22, texture(input.ambient_occlusion)),
            (23, texture(world_space)),
            (25, texture(fog.view)),
            (26, wgpu::BindingResource::Sampler(fog.sampler)),
            (27, texture(surface_depth)),
        ];
        let group = self.completion.compose_group.get(
            device,
            "screen-space reflection composition",
            &resources,
        );
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("screen-space reflection composition"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: input.output,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            timestamp_writes: timing.and_then(|t| t.render_pass("reflection composition")),
            ..Default::default()
        });
        pass.set_pipeline(&self.completion.compose);
        pass.set_bind_group(0, group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 3] {
    use crate::shading::layout_tests::mirror;
    [
        mirror!(
            "reflection_source",
            "SourceCamera",
            SourceCamera,
            [inverse, eye, fog, forward, flags]
        ),
        mirror!(
            "reflection_source",
            "ReflectionEnvironment",
            ReflectionEnvironment,
            [yaw, intensity, fade, traced]
        ),
        mirror!(
            "probe_culling",
            "CullingCamera",
            CullingCamera,
            [view, inverse_view, inverse_projection, z_near]
        ),
    ]
}

/// The constants with WGSL twins.
#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 3] {
    use crate::shading::layout_tests::Constant;
    use naga::Literal::U32;
    [
        Constant::new("probe_culling", "PROBE_BUCKETS", U32(PROBE_BUCKETS)),
        Constant::new("probe_culling", "PROBE_TILE_SIZE", U32(PROBE_TILE_SIZE)),
        Constant::new("reflection_source", "SOURCE_FOG", U32(SOURCE_FOG)),
    ]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod incident_tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod receiver_tests;
