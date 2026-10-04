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

/// A shader stage's module and entry point.
pub struct Shader {
    pub module: wgpu::ShaderModule,
    pub entry_point: &'static str,
}

/// `PostFXRenderTechnique::CreateShader`: the WGSL for `file_name` compiled
/// with `macros`.
pub fn create_shader(
    device: &wgpu::Device,
    file_name: &str,
    entry_point: &'static str,
    macros: &[(&str, &str)],
) -> Shader {
    Shader {
        module: device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(entry_point),
            source: wgpu::ShaderSource::Wgsl(shader_source(file_name, macros).into()),
        }),
        entry_point,
    }
}

#[derive(Default)]
pub struct PostFXRenderTechnique {
    pub pso: Option<wgpu::RenderPipeline>,
    pub layout: Option<wgpu::BindGroupLayout>,
}

impl PostFXRenderTechnique {
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_pso(
        &mut self,
        device: &wgpu::Device,
        pso_name: &str,
        vertex_shader: &Shader,
        pixel_shader: &Shader,
        resource_layout: &[(u32, Resource)],
        rtv_fmts: &[wgpu::TextureFormat],
        dsv_fmt: Option<wgpu::TextureFormat>,
        dss_desc: DepthStencilStateDesc,
    ) {
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
                    Resource::Texture | Resource::FilterableTexture => wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float {
                            filterable: matches!(resource, Resource::FilterableTexture),
                        },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
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
                    Resource::Sampler { filtering } => wgpu::BindingType::Sampler(if filtering {
                        wgpu::SamplerBindingType::Filtering
                    } else {
                        wgpu::SamplerBindingType::NonFiltering
                    }),
                },
                count: None,
            })
            .collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(pso_name),
            entries: &entries,
        });
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
        let pso = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(pso_name),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some(pso_name),
                    bind_group_layouts: &[Some(&layout)],
                    immediate_size: 0,
                }),
            ),
            vertex: wgpu::VertexState {
                module: &vertex_shader.module,
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
                    DepthStencilStateDesc::DisableDepth => (false, wgpu::CompareFunction::Always),
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
                module: &pixel_shader.module,
                entry_point: Some(pixel_shader.entry_point),
                compilation_options: Default::default(),
                targets: &targets,
            }),
            multiview_mask: None,
            cache: None,
        });
        self.pso = Some(pso);
        self.layout = Some(layout);
    }

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
