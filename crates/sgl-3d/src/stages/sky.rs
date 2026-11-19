//! Sky: the procedural sky's tables (`FrameEnvironment::Sky`), Hillaire
//! 2020's as Bevy 9d12036 ports them (`bevy_pbr/src/atmosphere`): the
//! transmittance table (256×128), the multiple-scattering table (32×32) and
//! the sky-view table (400×200), RGBA16F, over `shading/sky.wgsl`'s model
//! (the architecture's Sky). One per view; a probe capture runs one of its
//! own for its position.
//!
//! Each table is computed only when what it depends on differs from what it
//! holds: transmittance and multiple scattering from the sky's medium
//! (`Sky::rayleigh`, `mie`, `ozone` and `ground_albedo`); the sky view from
//! the medium, the frame's directional lights as given (above the air) and
//! the camera's distance from the planet's centre. Bevy computes every table
//! every frame (`atmosphere/node.rs`). What a table holds is recorded when
//! the frame that computed it is finished (S3D-4, D-58); a table a frame
//! that is not finished computed holds what no frame recorded, so the next
//! frame computes it again, whatever its inputs (X-4 in
//! `specs/sgl3d-history.md`).
//!
//! The pipelines are compiled and the tables made on the first frame whose
//! environment is a sky; the tables are dropped on a rendered frame whose
//! environment is not. They are not screen-sized: a resize and a cut do
//! nothing to them.
//!
//! Reads: `FrameInput::environment`'s `Sky`, the frame's directional lights
//! and the camera's eye.
//! Writes: its three tables; the sky-view table is what the opaque stage's
//! backdrop draw shows (unlit group 0's backdrop binding).
//! Honours: `FrameEnvironment::Sky` (its only control: no setting).
//! Timing groups: `sky transmittance`, `sky multiscattering`, `sky view`.
//! History: none kept; X-4.
use crate::FrameInput;
use crate::content::lighting::{DirectionalLight, FrameEnvironment, Sky as SkyLook};
use crate::shading::sky::SkyAtmosphereUniform;
use crate::view::frame::FrameContext;
use glam::Vec3;

/// The stage's bindings and shared marches.
pub(crate) static SKY_COMMON: crate::shading::Module = crate::shading::Module {
    name: "sky_common",
    source: include_str!("sky/common.wgsl"),
    deps: &[&crate::shading::sky::SKY, &crate::shading::UNIFORMS],
};
/// The transmittance table's pass.
pub(crate) static TRANSMITTANCE: crate::shading::Module = crate::shading::Module {
    name: "sky_transmittance",
    source: include_str!("sky/transmittance_lut.wgsl"),
    deps: &[&SKY_COMMON],
};
/// The multiple-scattering table's pass.
pub(crate) static MULTISCATTERING: crate::shading::Module = crate::shading::Module {
    name: "sky_multiscattering",
    source: include_str!("sky/multiscattering_lut.wgsl"),
    deps: &[&SKY_COMMON],
};
/// The sky-view table's pass.
pub(crate) static SKY_VIEW: crate::shading::Module = crate::shading::Module {
    name: "sky_view",
    source: include_str!("sky/sky_view_lut.wgsl"),
    deps: &[&SKY_COMMON],
};
/// The entry points the stage's pipelines are created with.
pub(crate) const TRANSMITTANCE_ENTRY: &str = "transmittance_lut_main";
pub(crate) const MULTISCATTERING_ENTRY: &str = "multiscattering_lut_main";
pub(crate) const SKY_VIEW_ENTRY: &str = "sky_view_lut_main";

/// The tables' sizes (twins of `shading/sky.wgsl`'s).
pub(crate) const TRANSMITTANCE_SIZE: [u32; 2] = [256, 128];
pub(crate) const MULTISCATTERING_SIZE: u32 = 32;
pub(crate) const SKY_VIEW_SIZE: [u32; 2] = [400, 200];
/// The transmittance and sky-view passes' workgroups' side.
const WORKGROUP: u32 = 16;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// `SkyLight` in `sky/common.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SkyLightUniform {
    pub direction_to_light: [f32; 3],
    pub padding_direction: f32,
    /// Colour times illuminance, above the air.
    pub color: [f32; 3],
    /// WGSL rounds `SkyLight` up to its 16-byte alignment.
    pub padding_color: f32,
}

/// `SkyUniform` in `sky/common.wgsl`: everything the tables are computed
/// from.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SkyUniform {
    pub atmosphere: SkyAtmosphereUniform,
    pub lights: [SkyLightUniform; 2],
    pub camera_radius: f32,
    /// WGSL rounds `SkyUniform` up to its 16-byte alignment.
    pub padding: [f32; 3],
}

impl SkyUniform {
    /// The tables' inputs for `sky`, `input`'s directional lights as given
    /// (an absent or off one black) and a camera at `eye`.
    fn of(sky: &SkyLook, input: &FrameInput, eye: Vec3) -> Self {
        let lights = input.directional_lights.map(|light| {
            light
                .filter(DirectionalLight::is_on)
                .map_or(SkyLightUniform::zeroed_up(), |light| SkyLightUniform {
                    direction_to_light: (-light.direction.normalize()).to_array(),
                    padding_direction: 0.,
                    color: light.color.map(|channel| channel * light.illuminance),
                    padding_color: 0.,
                })
        });
        Self {
            atmosphere: SkyAtmosphereUniform::of(sky),
            lights,
            camera_radius: crate::shading::sky::camera_radius(sky, eye),
            padding: [0.; 3],
        }
    }
}

impl SkyLightUniform {
    /// No light, shining from straight up.
    fn zeroed_up() -> Self {
        Self {
            direction_to_light: [0., 1., 0.],
            ..bytemuck::Zeroable::zeroed()
        }
    }
}

/// Which tables a frame's encode computed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Computed {
    pub transmittance: bool,
    pub multiscattering: bool,
    pub sky_view: bool,
}

struct Pipelines {
    transmittance: wgpu::ComputePipeline,
    multiscattering: wgpu::ComputePipeline,
    sky_view: wgpu::ComputePipeline,
    /// The passes' layouts, in that order.
    layouts: [wgpu::BindGroupLayout; 3],
    /// Linear and clamped (Bevy's `AtmosphereSampler`).
    sampler: wgpu::Sampler,
}

impl Pipelines {
    fn new(device: &wgpu::Device) -> Self {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let uniform = entry(
            0,
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(size_of::<SkyUniform>() as u64),
            },
        );
        let table = |binding| {
            entry(
                binding,
                wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            )
        };
        let sampler = entry(
            3,
            wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        );
        let output = entry(
            4,
            wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: FORMAT,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
        );
        let layout = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            })
        };
        // At most two sampled tables and one storage table a pass.
        let layouts = [
            layout("sky transmittance", &[uniform, output]),
            layout("sky multiscattering", &[uniform, table(1), sampler, output]),
            layout("sky view", &[uniform, table(1), table(2), sampler, output]),
        ];
        let pipeline = |module: &'static crate::shading::Module, entry, layout| {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(module.name),
                source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[module]).into()),
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(module.name),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(module.name),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            transmittance: pipeline(&TRANSMITTANCE, TRANSMITTANCE_ENTRY, &layouts[0]),
            multiscattering: pipeline(&MULTISCATTERING, MULTISCATTERING_ENTRY, &layouts[1]),
            sky_view: pipeline(&SKY_VIEW, SKY_VIEW_ENTRY, &layouts[2]),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("sky tables"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            layouts,
        }
    }
}

/// The tables, their uniform and the passes' groups.
struct Tables {
    uniform: wgpu::Buffer,
    /// The tables, which tests read back.
    #[cfg_attr(not(test), allow(dead_code))]
    transmittance: wgpu::Texture,
    #[cfg_attr(not(test), allow(dead_code))]
    multiscattering: wgpu::Texture,
    #[cfg_attr(not(test), allow(dead_code))]
    sky_view: wgpu::Texture,
    sky_view_view: wgpu::TextureView,
    groups: [wgpu::BindGroup; 3],
}

impl Tables {
    fn new(device: &wgpu::Device, pipelines: &Pipelines) -> Self {
        let table = |label, [width, height]: [u32; 2]| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let transmittance = table("sky transmittance table", TRANSMITTANCE_SIZE);
        let multiscattering = table("sky multiscattering table", [MULTISCATTERING_SIZE; 2]);
        let sky_view = table("sky view table", SKY_VIEW_SIZE);
        let uniform = crate::counters::buffer(
            device,
            &wgpu::BufferDescriptor {
                label: Some("sky"),
                size: size_of::<SkyUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            },
        );
        let views = [&transmittance, &multiscattering, &sky_view]
            .map(|texture| texture.create_view(&Default::default()));
        let view = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let uniform_entry = wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform.as_entire_binding(),
        };
        let sampler = wgpu::BindGroupEntry {
            binding: 3,
            resource: wgpu::BindingResource::Sampler(&pipelines.sampler),
        };
        let group = |label, layout, entries: &[wgpu::BindGroupEntry]| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries,
            })
        };
        let [transmittance_view, multiscattering_view, sky_view_view] = views;
        let groups = [
            group(
                "sky transmittance",
                &pipelines.layouts[0],
                &[uniform_entry.clone(), view(4, &transmittance_view)],
            ),
            group(
                "sky multiscattering",
                &pipelines.layouts[1],
                &[
                    uniform_entry.clone(),
                    view(1, &transmittance_view),
                    sampler.clone(),
                    view(4, &multiscattering_view),
                ],
            ),
            group(
                "sky view",
                &pipelines.layouts[2],
                &[
                    uniform_entry,
                    view(1, &transmittance_view),
                    view(2, &multiscattering_view),
                    sampler,
                    view(4, &sky_view_view),
                ],
            ),
        ];
        Self {
            uniform,
            transmittance,
            multiscattering,
            sky_view,
            sky_view_view,
            groups,
        }
    }
}

/// What the tables hold, as the last finished frame left them, and what the
/// frame being rendered computes, until it is settled (X-4). The tables are
/// single copies, so a render that computes one overwrites it whether or
/// not its frame is finished: an unfinished render that computed a table
/// leaves its contents unknown, and the next frame computes it again.
#[derive(Default)]
struct Held {
    /// The medium the transmittance and multiple-scattering tables hold;
    /// none where unknown.
    medium: Option<SkyAtmosphereUniform>,
    /// The inputs the sky-view table holds; none where unknown.
    view: Option<SkyUniform>,
    /// The frame being rendered: its inputs and the tables it computed.
    rendered: Option<(SkyUniform, Computed)>,
}

impl Held {
    /// The tables a frame of `inputs` computes: those whose inputs differ
    /// from what they hold or whose contents are unknown; the sky view also
    /// whenever the tables it reads are computed.
    fn computes(&self, inputs: &SkyUniform) -> Computed {
        let medium = self.medium != Some(inputs.atmosphere);
        Computed {
            transmittance: medium,
            multiscattering: medium,
            sky_view: medium || self.view.as_ref() != Some(inputs),
        }
    }

    /// The frame being rendered computes `computed` from `inputs`.
    fn leave(&mut self, inputs: SkyUniform, computed: Computed) {
        self.rendered = Some((inputs, computed));
    }

    /// Settles the last render: a finished one's inputs become what the
    /// tables hold; an unfinished one leaves the tables it computed unknown.
    fn settle(&mut self, finished: bool) {
        let Some((inputs, computed)) = self.rendered.take() else {
            return;
        };
        if finished {
            self.medium = Some(inputs.atmosphere);
            self.view = Some(inputs);
            return;
        }
        if computed.transmittance || computed.multiscattering {
            self.medium = None;
        }
        if computed.sky_view {
            self.view = None;
        }
    }
}

/// The stage of one view (or one probe capture).
#[derive(Default)]
pub(crate) struct Sky {
    pipelines: Option<Pipelines>,
    tables: Option<Tables>,
    /// What the tables hold.
    held: Held,
    /// The tables the last encode computed.
    computed: Computed,
}

impl Sky {
    /// Readies the stage for a frame whose environment is `environment`:
    /// with a sky, its pipelines and tables, made once; without, the tables
    /// are dropped.
    pub fn prepare(&mut self, device: &wgpu::Device, environment: &FrameEnvironment) {
        if !matches!(environment, FrameEnvironment::Sky(_)) {
            if self.tables.take().is_some() {
                self.held = Held::default();
            }
            return;
        }
        let pipelines = self.pipelines.get_or_insert_with(|| Pipelines::new(device));
        if self.tables.is_none() {
            self.tables = Some(Tables::new(device, pipelines));
            self.held = Held::default();
        }
    }

    /// The sky-view table, while the stage holds its tables.
    pub fn sky_view(&self) -> Option<&wgpu::TextureView> {
        self.tables.as_ref().map(|tables| &tables.sky_view_view)
    }

    /// The frame's tables, each where its inputs differ from what it holds:
    /// in the frame's Sky item, first after prepare.
    pub fn encode(&mut self, ctx: &mut FrameContext<'_>) {
        self.encode_from(
            (ctx.queue, ctx.encoder),
            ctx.input,
            ctx.input.camera.eye,
            ctx.timing,
        );
    }

    /// The tables of `input`'s sky seen from `eye`, each where its inputs
    /// differ from what the last finished frame left it holding or an
    /// unfinished render left it unknown. Nothing without a sky or before
    /// `prepare`.
    pub fn encode_from(
        &mut self,
        (queue, encoder): (&wgpu::Queue, &mut wgpu::CommandEncoder),
        input: &FrameInput,
        eye: Vec3,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        self.computed = Computed::default();
        let FrameEnvironment::Sky(sky) = &input.environment else {
            return;
        };
        let (Some(pipelines), Some(tables)) = (&self.pipelines, &self.tables) else {
            return;
        };
        let inputs = SkyUniform::of(sky, input, eye);
        let computed = self.held.computes(&inputs);
        self.held.leave(inputs, computed);
        self.computed = computed;
        if computed == Computed::default() {
            return;
        }
        crate::counters::write_buffer(queue, &tables.uniform, 0, bytemuck::bytes_of(&inputs));
        let mut pass = |name, pipeline, group, [x, y]: [u32; 2]| {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(name),
                timestamp_writes: timing.and_then(|timing| timing.compute_pass(name)),
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(x, y, 1);
        };
        let workgroups = |size: [u32; 2]| size.map(|side| side.div_ceil(WORKGROUP));
        if computed.transmittance {
            pass(
                "sky transmittance",
                &pipelines.transmittance,
                &tables.groups[0],
                workgroups(TRANSMITTANCE_SIZE),
            );
        }
        if computed.multiscattering {
            // One workgroup a texel, its depth the directions.
            pass(
                "sky multiscattering",
                &pipelines.multiscattering,
                &tables.groups[1],
                [MULTISCATTERING_SIZE; 2],
            );
        }
        if computed.sky_view {
            pass(
                "sky view",
                &pipelines.sky_view,
                &tables.groups[2],
                workgroups(SKY_VIEW_SIZE),
            );
        }
    }

    /// Settles the last render (`Held::settle`): a finished frame's inputs
    /// become what the tables hold; an unfinished one's computed tables are
    /// unknown.
    pub fn settle(&mut self, finished: bool) {
        self.held.settle(finished);
    }

    /// The tables the last encode computed.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn computed(&self) -> Computed {
        self.computed
    }

    /// The transmittance, multiple-scattering and sky-view tables, while the
    /// stage holds them.
    #[cfg(test)]
    pub fn tables(&self) -> Option<[&wgpu::Texture; 3]> {
        self.tables.as_ref().map(|tables| {
            [
                &tables.transmittance,
                &tables.multiscattering,
                &tables.sky_view,
            ]
        })
    }
}

/// The layouts this stage mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![
        mirror!(
            "sky_view",
            "SkyLight",
            SkyLightUniform,
            [direction_to_light, color]
        ),
        mirror!(
            "sky_view",
            "SkyUniform",
            SkyUniform,
            [atmosphere, lights, camera_radius]
        ),
    ]
}

/// The constants this stage shares with its shaders.
#[cfg(test)]
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    use crate::shading::layout_tests::Constant;
    let u = |program, name, value| Constant::new(program, name, naga::Literal::U32(value));
    vec![
        u(
            "sky_transmittance",
            "SKY_TRANSMITTANCE_WIDTH",
            TRANSMITTANCE_SIZE[0],
        ),
        u(
            "sky_transmittance",
            "SKY_TRANSMITTANCE_HEIGHT",
            TRANSMITTANCE_SIZE[1],
        ),
        u(
            "sky_multiscattering",
            "SKY_MULTISCATTERING_SIZE",
            MULTISCATTERING_SIZE,
        ),
        u("sky_view", "SKY_VIEW_WIDTH", SKY_VIEW_SIZE[0]),
        u("sky_view", "SKY_VIEW_HEIGHT", SKY_VIEW_SIZE[1]),
    ]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
