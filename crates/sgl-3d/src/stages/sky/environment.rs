//! The sky's environment atlas: Three r185's PMREM atlas (CubeUV) for a cube
//! of 128 texels (Bevy's `AtmosphereEnvironmentMapLight` default size),
//! 384x512 RGBA16F, in the format a map supplies, so every consumer of the
//! frame's environment reads a procedural sky through `pmrem_sample` as it
//! reads a map. Its sharp level is the sky-view table toward each texel
//! (Bevy's `environment.wesl`), its nine others Three's GGX filter of the
//! level before (`shading::pmrem_filter`).
//!
//! It is refreshed in place over a cycle of 16 steps, one a frame (X-5 in
//! `specs/sgl3d-history.md`): the capture; level 1 a face a step; level 2
//! three faces a step; levels 3 to 9 a step each. A step filters at most
//! 4,096 texels (a level-1 face), 1.05 million samples. A cycle starts when
//! the sky-view table has been computed since the last cycle's capture, so
//! a still sky costs nothing once its cycle ends; the whole cycle runs in one
//! frame on a new atlas and in a frame that restarts the view's history. The
//! step and the atlas's revision advance when a frame is finished, so a frame
//! rendered again runs the same step. Bevy refilters its whole cube every
//! frame; Three filters once; Godot's `Sky.PROCESS_MODE_INCREMENTAL` updates
//! its radiance map a roughness layer a frame, as this does (D-62).
use crate::shading::pmrem_filter::{Atlas, PMREM_FILTER};
use std::ops::Range;

/// The capture and the filter.
pub(crate) static SKY_ENVIRONMENT: crate::shading::Module = crate::shading::Module {
    name: "sky_environment",
    source: include_str!("environment.wgsl"),
    deps: &[&super::SKY_COMMON, &PMREM_FILTER],
};
/// The entry points the atlas's pipelines are created with.
pub(crate) const CAPTURE_ENTRY: &str = "sky_environment_capture";
pub(crate) const FILTER_ENTRY: &str = "sky_environment_filter";

/// The atlas's cube size.
pub(crate) const ATLAS: Atlas = Atlas { cube: 128 };
/// Steps in a cycle.
pub(crate) const STEPS: usize = 16;
/// The texels a step's workgroup writes along x and y.
const WORKGROUP: u32 = 8;
/// The spacing of the steps' uniforms (WebGPU's default
/// `min_uniform_buffer_offset_alignment`).
const STEP_STRIDE: u64 = 256;

/// One step of the cycle: level `level`'s faces `first_face` to
/// `first_face + faces - 1`, captured (level 0) or filtered from the level
/// before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Step {
    pub level: u32,
    pub first_face: u32,
    pub faces: u32,
}

impl Step {
    /// The atlas texels it writes: their corner and extent, one rectangle,
    /// since its faces are consecutive in one row or are all six.
    pub fn region(&self) -> ([u32; 2], [u32; 2]) {
        let level = ATLAS.level(self.level);
        let corner = level.face_origin(self.first_face);
        let across = self.faces.min(3);
        let rows = self.faces.div_ceil(3);
        (corner, [across * level.size, rows * level.size])
    }
}

/// The cycle's steps, in order.
pub(crate) fn steps() -> [Step; STEPS] {
    let step = |level, first_face, faces| Step {
        level,
        first_face,
        faces,
    };
    [
        step(0, 0, 6),
        step(1, 0, 1),
        step(1, 1, 1),
        step(1, 2, 1),
        step(1, 3, 1),
        step(1, 4, 1),
        step(1, 5, 1),
        step(2, 0, 3),
        step(2, 3, 3),
        step(3, 0, 6),
        step(4, 0, 6),
        step(5, 0, 6),
        step(6, 0, 6),
        step(7, 0, 6),
        step(8, 0, 6),
        step(9, 0, 6),
    ]
}

/// The atlas's cycle as a frame leaves it: the next step of a cycle under
/// way, if one is, and whether the sky-view table has been computed since the
/// last cycle's capture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cycle {
    pub next: Option<u8>,
    pub stale: bool,
}

/// The cycle kept from frame to frame (X-5): as the last finished frame left
/// it, and the frame being rendered's until it is settled.
#[derive(Default)]
pub(crate) struct AtlasCycle {
    /// None where the atlas holds nothing a finished frame completed: a new
    /// atlas, until a frame that ran its whole cycle is finished.
    committed: Option<Cycle>,
    /// The atlas's revision as the last finished frame left it.
    revision: u64,
    /// The frame being rendered's cycle and revision.
    rendered: Option<(Cycle, u64)>,
}

impl AtlasCycle {
    /// The steps a frame runs and leaves as its own: all of them where the
    /// atlas holds nothing a finished frame completed or the view's history
    /// restarts (`restart`); else the next of a cycle under way, or the
    /// capture where the sky-view table has been computed since the last
    /// capture (`view_computed`: in this frame, before its step), or none.
    pub fn plan(&mut self, view_computed: bool, restart: bool) -> Range<usize> {
        let (steps, cycle) = match self.committed.filter(|_| !restart) {
            None => (0..STEPS, Cycle::default()),
            Some(cycle) => {
                let stale = cycle.stale || view_computed;
                match cycle.next.map(usize::from) {
                    Some(step) => {
                        let next = (step + 1 < STEPS).then(|| (step + 1) as u8);
                        (step..step + 1, Cycle { next, stale })
                    }
                    None if stale => (
                        0..1,
                        Cycle {
                            next: Some(1),
                            stale: false,
                        },
                    ),
                    None => (0..0, cycle),
                }
            }
        };
        let revision = self.revision + u64::from(!steps.is_empty());
        self.rendered = Some((cycle, revision));
        steps
    }

    /// The atlas's revision as the frame being rendered leaves it, else as
    /// the last finished frame did: it changes whenever a frame writes the
    /// atlas.
    pub fn revision(&self) -> u64 {
        self.rendered
            .map_or(self.revision, |(_, revision)| revision)
    }

    /// Settles the last render: a finished one's cycle and revision become
    /// the last finished frame's; an unfinished one's are dropped, so the
    /// next frame runs its step again.
    pub fn settle(&mut self, finished: bool) {
        if let Some((cycle, revision)) = self.rendered.take()
            && finished
        {
            self.committed = Some(cycle);
            self.revision = revision;
        }
    }

    /// A new atlas: it holds nothing, and its next frame runs the whole
    /// cycle. The revision keeps counting.
    pub fn forget(&mut self) {
        self.committed = None;
        self.rendered = None;
    }
}

/// `SkyEnvironmentStep` in `environment.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SkyEnvironmentStepUniform {
    pub origin: [u32; 2],
    pub size: u32,
    pub first_face: u32,
    pub roughness: f32,
    pub mip_in: f32,
}

impl SkyEnvironmentStepUniform {
    fn of(step: &Step) -> Self {
        let level = ATLAS.level(step.level);
        Self {
            origin: level.origin,
            size: level.size,
            first_face: step.first_face,
            roughness: if step.level == 0 {
                0.
            } else {
                ATLAS.filter_roughness(step.level)
            },
            mip_in: if step.level == 0 {
                0.
            } else {
                ATLAS.mip(step.level - 1)
            },
        }
    }
}

/// The pipelines and layouts of the capture and the filter.
pub(super) struct EnvironmentPipelines {
    capture: wgpu::ComputePipeline,
    filter: wgpu::ComputePipeline,
    capture_layout: wgpu::BindGroupLayout,
    filter_layout: wgpu::BindGroupLayout,
}

impl EnvironmentPipelines {
    pub fn new(device: &wgpu::Device) -> Self {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let step = entry(
            5,
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: wgpu::BufferSize::new(
                    size_of::<SkyEnvironmentStepUniform>() as u64
                ),
            },
        );
        let sampled = |binding, view_dimension| {
            entry(
                binding,
                wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension,
                    multisampled: false,
                },
            )
        };
        let sampler = entry(
            3,
            wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        );
        let output = entry(
            8,
            wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: super::FORMAT,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
        );
        let uniform = entry(
            0,
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(size_of::<super::SkyUniform>() as u64),
            },
        );
        let layout = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            })
        };
        // One sampled texture and one storage texture a pass.
        let capture_layout = layout(
            "sky environment capture",
            &[
                uniform,
                sampler,
                step,
                sampled(6, wgpu::TextureViewDimension::D2),
                output,
            ],
        );
        let filter_layout = layout(
            "sky environment filter",
            &[
                sampler,
                step,
                sampled(7, wgpu::TextureViewDimension::D2Array),
                output,
            ],
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(SKY_ENVIRONMENT.name),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&SKY_ENVIRONMENT]).into()),
        });
        let pipeline = |entry, layout: &wgpu::BindGroupLayout| {
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(entry),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            capture: pipeline(CAPTURE_ENTRY, &capture_layout),
            filter: pipeline(FILTER_ENTRY, &filter_layout),
            capture_layout,
            filter_layout,
        }
    }
}

/// The atlas, the scratch atlas each filtered step is written into before it
/// is copied back, the steps' uniforms and the passes' groups.
pub(super) struct EnvironmentAtlas {
    atlas: wgpu::Texture,
    /// The atlas as group 0 binds it, a one-layer array.
    pub view: wgpu::TextureView,
    scratch: wgpu::Texture,
    capture: wgpu::BindGroup,
    filter: wgpu::BindGroup,
}

impl EnvironmentAtlas {
    /// The atlas whose capture reads the sky-view table `sky_view` from
    /// radius `uniform`'s camera radius, sampled through `sampler`.
    pub fn new(
        device: &wgpu::Device,
        pipelines: &EnvironmentPipelines,
        (uniform, sky_view, sampler): (&wgpu::Buffer, &wgpu::TextureView, &wgpu::Sampler),
    ) -> Self {
        let [width, height] = ATLAS.size();
        let texture = |label, usage| {
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
                format: super::FORMAT,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC | usage,
                view_formats: &[],
            })
        };
        let atlas = texture(
            "sky environment atlas",
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let scratch = texture("sky environment scratch", wgpu::TextureUsages::empty());
        let view = atlas.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let storage = |texture: &wgpu::Texture| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                ..Default::default()
            })
        };
        let steps: Vec<u8> = steps()
            .iter()
            .flat_map(|step| {
                let mut bytes = bytemuck::bytes_of(&SkyEnvironmentStepUniform::of(step)).to_vec();
                bytes.resize(STEP_STRIDE as usize, 0);
                bytes
            })
            .collect();
        let steps = crate::counters::buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("sky environment steps"),
                contents: &steps,
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let step = wgpu::BindGroupEntry {
            binding: 5,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &steps,
                offset: 0,
                size: wgpu::BufferSize::new(size_of::<SkyEnvironmentStepUniform>() as u64),
            }),
        };
        let sampler = wgpu::BindGroupEntry {
            binding: 3,
            resource: wgpu::BindingResource::Sampler(sampler),
        };
        let texture_entry = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let (atlas_storage, scratch_storage) = (storage(&atlas), storage(&scratch));
        let capture = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sky environment capture"),
            layout: &pipelines.capture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                sampler.clone(),
                step.clone(),
                texture_entry(6, sky_view),
                texture_entry(8, &atlas_storage),
            ],
        });
        let filter = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sky environment filter"),
            layout: &pipelines.filter_layout,
            entries: &[
                sampler,
                step,
                texture_entry(7, &view),
                texture_entry(8, &scratch_storage),
            ],
        });
        Self {
            atlas,
            view,
            scratch,
            capture,
            filter,
        }
    }

    /// Encodes steps `steps` of the cycle, in order: the capture into the
    /// atlas, each filtered step into the scratch atlas and back.
    pub fn encode(
        &self,
        pipelines: &EnvironmentPipelines,
        encoder: &mut wgpu::CommandEncoder,
        steps_run: Range<usize>,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let all = steps();
        for index in steps_run {
            let step = all[index];
            let size = ATLAS.level(step.level).size;
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("sky environment"),
                    timestamp_writes: timing
                        .and_then(|timing| timing.compute_pass("sky environment")),
                });
                let (pipeline, group) = if step.level == 0 {
                    (&pipelines.capture, &self.capture)
                } else {
                    (&pipelines.filter, &self.filter)
                };
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, group, &[(index as u64 * STEP_STRIDE) as u32]);
                let side = size.div_ceil(WORKGROUP);
                pass.dispatch_workgroups(side, side, step.faces);
            }
            if step.level != 0 {
                let ([x, y], [width, height]) = step.region();
                let at = |texture| wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                };
                encoder.copy_texture_to_texture(
                    at(&self.scratch),
                    at(&self.atlas),
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
    }

    /// The atlas texture, which tests read back.
    #[cfg(test)]
    pub fn texture(&self) -> &wgpu::Texture {
        &self.atlas
    }
}

/// The layout this module mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "sky_environment",
        "SkyEnvironmentStep",
        SkyEnvironmentStepUniform,
        [origin, size, first_face, roughness, mip_in]
    )]
}

#[cfg(test)]
mod tests;
