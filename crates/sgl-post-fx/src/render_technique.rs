//! Port of DiligentFX `PostProcess/Common/{interface/PostFXRenderTechnique.hpp,
//! src/PostFXRenderTechnique.cpp}` (revision
//! f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright 2024-2026 Diligent
//! Graphics LLC, licensed under the Apache License, Version 2.0
//! (vendor/DiligentFX/License.txt). Modified: rewritten in Rust over wgpu.
//!
//! A technique is one graphics pipeline and the layout of its shader
//! resources. Diligent binds resources through a shader resource binding
//! (SRB) that outlives frames; wgpu bind groups are cheap, so each pass
//! builds its group when it draws.
use crate::shaders::shader_source;
use std::any::Any;
use std::sync::{Arc, OnceLock};

/// One shader resource of a pipeline's single bind group, at the WGSL
/// binding number the ported shader declares. Diligent's resource layout
/// names variables; WGSL additionally needs each texture's sample type.
#[derive(Clone, Copy, Debug)]
pub enum Resource {
    /// `cbuffer`.
    ConstantBuffer,
    /// `Texture2D<float*>` read with `Load` only: unfilterable, so any float
    /// format binds (R32F is not filterable in WebGPU).
    Texture,
    /// `Texture2D<float*>` sampled through a filtering sampler.
    FilterableTexture,
    /// The depth buffer (`Texture2D<float>` on a depth texture).
    DepthTexture,
    /// `Texture2D<uint>`.
    UintTexture,
    /// `SamplerState`: linear (filtering) or point.
    Sampler { filtering: bool },
}

/// `CommonlyUsedStates.h` depth-stencil states the post effects use. Diligent's
/// default depth function is `COMPARISON_FUNC_LESS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepthStencilStateDesc {
    /// `DSS_DisableDepth`.
    DisableDepth,
    /// `ScreenSpaceReflection.cpp` `DSS_WriteAlways`: enabled, writes, `ALWAYS`.
    WriteAlways,
    /// `DSS_EnableDepthNoWrites`: enabled, no writes, `LESS`.
    EnableDepthNoWrites,
}

/// A shader stage: its file's WGSL with its macros, and its entry point.
/// Its module is made where a pipeline first needs it, on the thread that
/// creates that pipeline. Clones share the module.
#[derive(Clone)]
pub struct Shader {
    file_name: String,
    macros: Vec<(String, String)>,
    module: Arc<OnceLock<wgpu::ShaderModule>>,
    pub entry_point: &'static str,
}

impl Shader {
    /// Another entry point of the same module.
    pub fn with_entry_point(&self, entry_point: &'static str) -> Self {
        Self {
            entry_point,
            ..self.clone()
        }
    }

    /// The module, compiled here if no pipeline has needed it yet.
    pub fn module(&self, device: &wgpu::Device) -> &wgpu::ShaderModule {
        self.module.get_or_init(|| {
            let macros: Vec<_> = self
                .macros
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(self.entry_point),
                source: wgpu::ShaderSource::Wgsl(shader_source(&self.file_name, &macros).into()),
            })
        })
    }
}

/// `PostFXRenderTechnique::CreateShader`: the WGSL for `file_name` with
/// `macros`, compiled where a pipeline first needs it (`Shader::module`).
#[cfg_attr(
    target_arch = "wasm32",
    allow(
        clippy::arc_with_non_send_sync,
        reason = "one type for both targets: pipelines compile on any thread on native"
    )
)]
pub fn create_shader(
    file_name: &str,
    entry_point: &'static str,
    macros: &[(&str, &str)],
) -> Shader {
    Shader {
        file_name: file_name.to_owned(),
        macros: macros
            .iter()
            .map(|&(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
        module: Arc::default(),
        entry_point,
    }
}

/// What describes a technique's pipeline: given the device, it makes what
/// the descriptor borrows (its layout and shader modules) and hands the
/// descriptor to the sink it is given, which creates the pipeline the
/// compiler's way (at once, or asynchronously in the browser). It owns what
/// it reads but the device, runs on any thread, and may run more than once.
#[cfg(not(target_arch = "wasm32"))]
pub type DescribePipeline =
    Box<dyn Fn(&wgpu::Device, &mut dyn FnMut(&wgpu::RenderPipelineDescriptor<'_>)) + Send>;
#[cfg(target_arch = "wasm32")]
pub type DescribePipeline =
    Box<dyn Fn(&wgpu::Device, &mut dyn FnMut(&wgpu::RenderPipelineDescriptor<'_>))>;

/// Who creates the techniques' pipelines, and when (PROVENANCE DFX-2):
/// DiligentFX may create a PSO asynchronously, and an effect whose PSOs are
/// not all ready reports `Pending` and leaves its passes out. `InPlace`
/// creates each where its technique is initialized, so every technique is
/// ready at once. A caller with its own way of compiling (a job on another
/// thread, a loading step) implements this and gives it to the effects
/// (`set_pipeline_compiler`).
pub trait PipelineCompiler {
    /// The pipeline `describe` describes for the technique that owns
    /// `slot`, named `label`, or none while it is not ready. Called again
    /// each time the technique is initialized until it is: `slot` is the
    /// technique's storage for whatever the compiler keeps between calls,
    /// and `describe` may be dropped unrun once a pipeline is on its way.
    fn compile(
        &self,
        device: &wgpu::Device,
        slot: &mut PipelineSlot,
        label: &'static str,
        describe: DescribePipeline,
    ) -> Option<wgpu::RenderPipeline>;
}

/// The default compiler: each pipeline is created where its technique is
/// initialized, on the calling thread.
pub struct InPlace;

impl PipelineCompiler for InPlace {
    fn compile(
        &self,
        device: &wgpu::Device,
        _: &mut PipelineSlot,
        _: &'static str,
        describe: DescribePipeline,
    ) -> Option<wgpu::RenderPipeline> {
        let mut pipeline = None;
        describe(device, &mut |desc| {
            pipeline = Some(device.create_render_pipeline(desc));
        });
        pipeline
    }
}

/// The effects' handle on their compiler.
#[cfg(not(target_arch = "wasm32"))]
pub type Pipelines = Arc<dyn PipelineCompiler + Send + Sync>;
#[cfg(target_arch = "wasm32")]
pub type Pipelines = Arc<dyn PipelineCompiler>;

/// A technique's storage for its compiler: what a `PipelineCompiler` keeps
/// for one pipeline between calls.
#[derive(Default)]
pub struct PipelineSlot(Option<Stored>);

#[cfg(not(target_arch = "wasm32"))]
type Stored = Box<dyn Any + Send + Sync>;
#[cfg(target_arch = "wasm32")]
type Stored = Box<dyn Any>;

impl PipelineSlot {
    /// What the compiler stored, `make`'s value the first time.
    ///
    /// # Panics
    ///
    /// If another type was stored: one compiler serves a technique.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn get_or_insert_with<T: Any + Send + Sync>(&mut self, make: impl FnOnce() -> T) -> &mut T {
        self.0
            .get_or_insert_with(|| Box::new(make()))
            .downcast_mut()
            .expect("one pipeline compiler serves a technique")
    }

    /// What the compiler stored, `make`'s value the first time.
    ///
    /// # Panics
    ///
    /// If another type was stored: one compiler serves a technique.
    #[cfg(target_arch = "wasm32")]
    pub fn get_or_insert_with<T: Any>(&mut self, make: impl FnOnce() -> T) -> &mut T {
        self.0
            .get_or_insert_with(|| Box::new(make()))
            .downcast_mut()
            .expect("one pipeline compiler serves a technique")
    }
}

#[derive(Default)]
pub struct PostFXRenderTechnique {
    /// The pipeline, once its compiler has it (`is_initialized_pso`).
    pub pso: Option<wgpu::RenderPipeline>,
    pub layout: Option<wgpu::BindGroupLayout>,
    slot: PipelineSlot,
}

impl PostFXRenderTechnique {
    #[allow(clippy::too_many_arguments)]
    /// Lays the technique's resources out and asks `pipelines` for its
    /// pipeline: ready at once with `InPlace`, else `is_initialized_pso`
    /// stays false and the caller initializes it again on a later frame.
    /// The layout is made at the first call and kept: a technique serves
    /// one resource layout, and another layout takes another technique.
    pub fn initialize_pso(
        &mut self,
        pipelines: &dyn PipelineCompiler,
        device: &wgpu::Device,
        pso_name: &'static str,
        vertex_shader: &Shader,
        pixel_shader: &Shader,
        resource_layout: &[(u32, Resource)],
        rtv_fmts: &[wgpu::TextureFormat],
        dsv_fmt: Option<wgpu::TextureFormat>,
        dss_desc: DepthStencilStateDesc,
    ) {
        if self.layout.is_none() {
            let entries: Vec<_> = resource_layout
                .iter()
                .map(|&(binding, resource)| wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: match resource {
                        Resource::ConstantBuffer => wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        Resource::Texture | Resource::FilterableTexture => {
                            wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float {
                                    filterable: matches!(resource, Resource::FilterableTexture),
                                },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            }
                        }
                        Resource::DepthTexture => wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        Resource::UintTexture => wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Uint,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        Resource::Sampler { filtering } => {
                            wgpu::BindingType::Sampler(if filtering {
                                wgpu::SamplerBindingType::Filtering
                            } else {
                                wgpu::SamplerBindingType::NonFiltering
                            })
                        }
                    },
                    count: None,
                })
                .collect();
            self.layout = Some(
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some(pso_name),
                    entries: &entries,
                }),
            );
        }
        let layout = self.layout.clone().expect("laid out above");
        let targets: Vec<_> = rtv_fmts
            .iter()
            .map(|&format| {
                // BS_Default: no blending, all channels written.
                Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })
            })
            .collect();
        let (vertex_shader, pixel_shader) = (vertex_shader.clone(), pixel_shader.clone());
        let describe: DescribePipeline = Box::new(move |device, create| {
            create(&wgpu::RenderPipelineDescriptor {
                label: Some(pso_name),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some(pso_name),
                        bind_group_layouts: &[Some(&layout)],
                        immediate_size: 0,
                    }),
                ),
                vertex: wgpu::VertexState {
                    module: vertex_shader.module(device),
                    entry_point: Some(vertex_shader.entry_point),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                // FILL_MODE_SOLID, CULL_MODE_BACK, FrontCounterClockwise = false,
                // PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP.
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    front_face: wgpu::FrontFace::Cw,
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: dsv_fmt.map(|format| {
                    let (write, compare) = match dss_desc {
                        DepthStencilStateDesc::DisableDepth => {
                            (false, wgpu::CompareFunction::Always)
                        }
                        DepthStencilStateDesc::WriteAlways => (true, wgpu::CompareFunction::Always),
                        DepthStencilStateDesc::EnableDepthNoWrites => {
                            (false, wgpu::CompareFunction::Less)
                        }
                    };
                    wgpu::DepthStencilState {
                        format,
                        depth_write_enabled: Some(write),
                        depth_compare: Some(compare),
                        stencil: Default::default(),
                        bias: Default::default(),
                    }
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: pixel_shader.module(device),
                    entry_point: Some(pixel_shader.entry_point),
                    compilation_options: Default::default(),
                    targets: &targets,
                }),
                multiview_mask: None,
                cache: None,
            });
        });
        self.pso = pipelines.compile(device, &mut self.slot, pso_name, describe);
    }

    /// Whether the technique has its pipeline: DiligentFX's
    /// `IsInitializedPSO() && IsReady()`.
    pub fn is_initialized_pso(&self) -> bool {
        self.pso.is_some()
    }

    /// A bind group for this technique's layout.
    pub fn bind_group(
        &self,
        device: &wgpu::Device,
        label: &str,
        entries: &[wgpu::BindGroupEntry<'_>],
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: self.layout.as_ref().expect("initialized technique"),
            entries,
        })
    }
}
