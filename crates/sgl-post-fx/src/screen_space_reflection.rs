//! Port of DiligentFX `PostProcess/ScreenSpaceReflection/{interface/ScreenSpaceReflection.hpp,
//! src/ScreenSpaceReflection.cpp}` (revision
//! f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright 2023-2026 Diligent
//! Graphics LLC, licensed under the Apache License, Version 2.0
//! (vendor/DiligentFX/License.txt). Modified: rewritten in Rust over wgpu;
//! origins and implementation notes are recorded in PROVENANCE.md.
//!
//! Implements [screen-space reflection post-process effect](https://github.com/DiligentGraphics/DiligentFX/tree/master/PostProcess/ScreenSpaceReflection).
use crate::post_fx_context::{self, BlueNoiseDimension, PostFXContext, TextureOperationAttribs};
use crate::render_technique::{
    DepthStencilStateDesc, PostFXRenderTechnique, Resource, create_shader,
};
use crate::structures::ScreenSpaceReflectionAttribs;
use std::collections::HashMap;

/// Maximum mip level of depth buffer used in the Hi-Z tracing
/// (`SSR_DEPTH_HIERARCHY_MAX_MIP`).
pub const SSR_DEPTH_HIERARCHY_MAX_MIP: u32 = 6;

/// `ScreenSpaceReflection::FEATURE_FLAGS`: flags that control the behavior of
/// the effect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FeatureFlags(pub u32);

impl FeatureFlags {
    /// No feature flags are set.
    pub const NONE: Self = Self(0);
    /// When using this flag, you only need to pass the color buffer of the previous frame.
    /// We find the intersection using the depth buffer of the current frame, and when an intersection is found,
    /// we make the corresponding offset by the velocity vector at the intersection point, for sampling from the color buffer.
    pub const PREVIOUS_FRAME: Self = Self(1 << 0);
    /// When this flag is used, ray tracing step is executed at half resolution
    pub const HALF_RESOLUTION: Self = Self(1 << 1);

    pub fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 != 0
    }
}

impl std::ops::BitOr for FeatureFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// `POST_FX_EXECUTION_STATUS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostFxExecutionStatus {
    Ready,
    Pending,
}

/// Render attributes. The device context is the encoder the passes record
/// into and the queue that uploads constants.
pub struct RenderAttributes<'a, 'p> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub device_context: &'a mut wgpu::CommandEncoder,
    /// PostFX context
    pub post_fx_context: &'a mut PostFXContext,
    /// Shader resource view of the source color
    pub color_buffer_srv: &'a wgpu::TextureView,
    /// Shader resource view of the source depth (a depth texture).
    pub depth_buffer_srv: &'a wgpu::TextureView,
    /// Shader resource view of the source normal buffer
    pub normal_buffer_srv: &'a wgpu::TextureView,
    /// Shader resource view of the source roughness buffer
    pub material_buffer_srv: &'a wgpu::TextureView,
    /// Shader resource view of the source motion buffer
    pub motion_vectors_srv: &'a wgpu::TextureView,
    /// SSR settings
    pub ssr_attribs: &'a ScreenSpaceReflectionAttribs,
    /// Per-pass timestamps (the upstream debug groups), when profiling.
    pub pass_timestamps: Option<&'a post_fx_context::PassTimestamps<'p>>,
    /// Discard the radiance and variance history this frame, as
    /// `TemporalAntiAliasingAttribs::ResetAccumulation` does for TAA (not in
    /// upstream SSR; PROVENANCE.md DFX-12).
    pub reset_accumulation: bool,
    /// Seconds since the previous execution, which time the transition
    /// alpha instead of a clock (not in upstream; PROVENANCE.md DFX-24).
    pub frame_time: f32,
}

// The upstream RENDER_TECH_COMPUTE_* names.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RenderTech {
    ComputeHierarchicalDepthBuffer,
    ComputeStencilMaskAndExtractRoughness,
    ComputeDownsampledStencilMask,
    ComputeIntersection,
    ClassifyDenoiserTiles,
    DilateDenoiserTiles,
    ComputeSpatialReconstruction,
    ComputeTemporalAccumulation,
    ComputeBilateralCleanup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RenderTechniqueKey {
    render_tech: RenderTech,
    feature_flags: FeatureFlags,
    use_reverse_depth: bool,
}

const DEPTH_HIERARCHY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const ROUGHNESS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
/// DFX-29: the side of a denoiser tile in pixels, the shaders'
/// `SSR_DENOISER_TILE_SIZE`. The classification marks blocks of half that
/// side (`SSR_DENOISER_BLOCK_SIZE`).
const DENOISER_TILE_SIZE: u32 = 8;
const DENOISER_BLOCK_SIZE: u32 = DENOISER_TILE_SIZE / 2;
const DENOISER_TILE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
const DEPTH_STENCIL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth16Unorm;
const RADIANCE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const VARIANCE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;
const RESOLVED_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;

/// `RESOURCE_IDENTIFIER_*` internal resources, created by `prepare_resources`.
struct Resources {
    depth_hierarchy: wgpu::TextureView,
    depth_stencil_mask: wgpu::TextureView,
    depth_stencil_mask_half_res: Option<wgpu::TextureView>,
    roughness: wgpu::TextureView,
    radiance: wgpu::TextureView,
    ray_direction_pdf: wgpu::TextureView,
    /// DFX-29: per 4×4 block of pixels, whether a ray in it found a
    /// confident hit.
    denoiser_hits: wgpu::TextureView,
    /// DFX-29: per denoiser tile, whether it or a neighbour has a hit.
    denoiser_tiles: wgpu::TextureView,
    resolved_radiance: wgpu::TextureView,
    resolved_variance: wgpu::TextureView,
    resolved_depth: wgpu::TextureView,
    radiance_history: [wgpu::TextureView; 2],
    variance_history: [wgpu::TextureView; 2],
    output: wgpu::TextureView,
}

pub struct ScreenSpaceReflection {
    render_tech: HashMap<RenderTechniqueKey, PostFXRenderTechnique>,
    ssr_attribs: ScreenSpaceReflectionAttribs,
    constant_buffer: wgpu::Buffer,
    resources: Option<Resources>,
    hierarchical_depth_mip_map_rtv: Vec<wgpu::TextureView>,
    hierarchical_depth_mip_map_srv: Vec<wgpu::TextureView>,

    back_buffer_width: u32,
    back_buffer_height: u32,

    feature_flags: FeatureFlags,
    use_reverse_depth: bool,

    /// Seconds since the transition restarted, summed from
    /// `RenderAttributes::frame_time` (`m_FrameTimer`; PROVENANCE.md
    /// DFX-24); `None` before the first execution.
    frame_timer: Option<f32>,
    linear_clamp: wgpu::Sampler,
    /// The frame index of the last execution, `!0` before the first, as
    /// `TemporalAntiAliasing::AccumulationBufferInfo::LastFrameIdx`.
    last_frame_idx: u32,
}

/// `ComputeMipLevelsCount(Width, Height)`.
fn compute_mip_levels_count(width: u32, height: u32) -> u32 {
    32 - width.max(height).leading_zeros()
}

fn texture_2d(
    device: &wgpu::Device,
    name: &str,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    mip_levels: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(name),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: mip_levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        // BIND_SHADER_RESOURCE | BIND_RENDER_TARGET (or BIND_DEPTH_STENCIL);
        // COPY_SRC lets callers capture the intermediate results.
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&Default::default())
}

fn texture(view: &wgpu::TextureView) -> wgpu::BindingResource<'_> {
    wgpu::BindingResource::TextureView(view)
}

/// The depth-stencil view of a pass: cleared to 0.0 and written, or read-only.
enum DepthStencil<'a> {
    Clear(&'a wgpu::TextureView),
    ReadOnly(&'a wgpu::TextureView),
}

/// One draw of a technique into `rtvs` (cleared to zero where `clear_rtvs`)
/// and an optional depth-stencil view: upstream's `SetRenderTargets`, clears,
/// `SetPipelineState`, `CommitShaderResources` and `Draw`.
#[allow(clippy::too_many_arguments)]
fn draw_pass(
    device_context: &mut wgpu::CommandEncoder,
    label: &str,
    rtvs: &[&wgpu::TextureView],
    clear_rtvs: bool,
    dsv: Option<DepthStencil<'_>>,
    tech: &PostFXRenderTechnique,
    group: &wgpu::BindGroup,
    timestamp_writes: Option<wgpu::RenderPassTimestampWrites<'_>>,
) {
    let attachments: Vec<_> = rtvs
        .iter()
        .map(|view| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if clear_rtvs {
                        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                },
            })
        })
        .collect();
    let mut pass = device_context.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &attachments,
        depth_stencil_attachment: dsv.map(|dsv| match dsv {
            DepthStencil::Clear(view) => wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            },
            DepthStencil::ReadOnly(view) => wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: None,
                stencil_ops: None,
            },
        }),
        timestamp_writes,
        ..Default::default()
    });
    pass.set_pipeline(tech.pso.as_ref().expect("initialized technique"));
    pass.set_bind_group(0, group, &[]);
    pass.draw(0..3, 0..1);
}

impl ScreenSpaceReflection {
    /// Creates a new instance of the ScreenSpaceReflection class.
    pub fn new(device: &wgpu::Device) -> Self {
        let ssr_attribs = ScreenSpaceReflectionAttribs::default();
        let constant_buffer = {
            use wgpu::util::DeviceExt;
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ScreenSpaceReflection::ConstantBuffer"),
                contents: bytemuck::bytes_of(&ssr_attribs),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
        };
        Self {
            render_tech: HashMap::new(),
            ssr_attribs,
            constant_buffer,
            resources: None,
            hierarchical_depth_mip_map_rtv: Vec::new(),
            hierarchical_depth_mip_map_srv: Vec::new(),
            back_buffer_width: 0,
            back_buffer_height: 0,
            feature_flags: FeatureFlags::NONE,
            use_reverse_depth: false,
            frame_timer: None,
            last_frame_idx: !0,
            linear_clamp: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("Sam_LinearClamp"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            }),
        }
    }

    /// Prepares resources for the effect.
    pub fn prepare_resources(
        &mut self,
        device: &wgpu::Device,
        device_context: &mut wgpu::CommandEncoder,
        post_fx_context: &mut PostFXContext,
        feature_flags: FeatureFlags,
    ) {
        let frame_desc = *post_fx_context.get_frame_desc();
        let use_reverse_depth = post_fx_context
            .get_feature_flags()
            .contains(post_fx_context::FeatureFlags::REVERSED_DEPTH);
        if self.feature_flags != feature_flags || self.use_reverse_depth != use_reverse_depth {
            if self.feature_flags.contains(FeatureFlags::HALF_RESOLUTION)
                != feature_flags.contains(FeatureFlags::HALF_RESOLUTION)
            {
                self.back_buffer_width = 0;
                self.back_buffer_height = 0;
            }

            self.feature_flags = feature_flags;
            self.use_reverse_depth = use_reverse_depth;
        }

        if self.back_buffer_width == frame_desc.width
            && self.back_buffer_height == frame_desc.height
        {
            return;
        }

        self.back_buffer_width = frame_desc.width;
        self.back_buffer_height = frame_desc.height;

        let (width, height) = (self.back_buffer_width, self.back_buffer_height);
        let half_res = feature_flags.contains(FeatureFlags::HALF_RESOLUTION);
        let (trace_width, trace_height) = if half_res {
            (width / 2, height / 2)
        } else {
            (width, height)
        };

        let depth_hierarchy_mip_count = SSR_DEPTH_HIERARCHY_MAX_MIP + 1;
        let depth_hierarchy = texture_2d(
            device,
            "ScreenSpaceReflection::DepthHierarchy",
            width,
            height,
            DEPTH_HIERARCHY_FORMAT,
            compute_mip_levels_count(width, height).min(depth_hierarchy_mip_count),
        );
        let mip_view = |mip_level| {
            depth_hierarchy.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: mip_level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        // TextureSubresourceViews: one render target and one shader resource
        // view per mip.
        self.hierarchical_depth_mip_map_rtv = (0..depth_hierarchy.mip_level_count())
            .map(mip_view)
            .collect();
        self.hierarchical_depth_mip_map_srv = (0..depth_hierarchy.mip_level_count())
            .map(mip_view)
            .collect();

        let mut history = |name| {
            let history_texture =
                view(&texture_2d(device, name, width, height, RADIANCE_FORMAT, 1));
            post_fx_context.clear_render_target(
                &mut TextureOperationAttribs {
                    device,
                    device_context,
                    timestamp_writes: None,
                },
                &history_texture,
                [0.0, 0.0, 0.0, 0.0],
            );
            history_texture
        };
        let radiance_history = [
            history("ScreenSpaceReflection::RadianceHistory"),
            history("ScreenSpaceReflection::RadianceHistory"),
        ];
        let variance_history = std::array::from_fn(|_| {
            let history_texture = view(&texture_2d(
                device,
                "ScreenSpaceReflection::VarianceHistory",
                width,
                height,
                VARIANCE_FORMAT,
                1,
            ));
            post_fx_context.clear_render_target(
                &mut TextureOperationAttribs {
                    device,
                    device_context,
                    timestamp_writes: None,
                },
                &history_texture,
                [0.0, 0.0, 0.0, 0.0],
            );
            history_texture
        });
        let output = view(&texture_2d(
            device,
            "ScreenSpaceReflection::Output",
            width,
            height,
            RADIANCE_FORMAT,
            1,
        ));
        post_fx_context.clear_render_target(
            &mut TextureOperationAttribs {
                device,
                device_context,
                timestamp_writes: None,
            },
            &output,
            [0.0, 0.0, 0.0, 0.0],
        );

        self.resources = Some(Resources {
            depth_hierarchy: view(&depth_hierarchy),
            roughness: view(&texture_2d(
                device,
                "ScreenSpaceReflection::Roughness",
                width,
                height,
                ROUGHNESS_FORMAT,
                1,
            )),
            depth_stencil_mask: view(&texture_2d(
                device,
                "ScreenSpaceReflection::DepthStencilMask",
                width,
                height,
                DEPTH_STENCIL_FORMAT,
                1,
            )),
            depth_stencil_mask_half_res: half_res.then(|| {
                view(&texture_2d(
                    device,
                    "ScreenSpaceReflection::DepthStencilMaskHalfRes",
                    width / 2,
                    height / 2,
                    DEPTH_STENCIL_FORMAT,
                    1,
                ))
            }),
            radiance: view(&texture_2d(
                device,
                "ScreenSpaceReflection::Radiance",
                trace_width,
                trace_height,
                RADIANCE_FORMAT,
                1,
            )),
            ray_direction_pdf: view(&texture_2d(
                device,
                "ScreenSpaceReflection::RayDirectionPDF",
                trace_width,
                trace_height,
                RADIANCE_FORMAT,
                1,
            )),
            denoiser_hits: view(&texture_2d(
                device,
                "ScreenSpaceReflection::DenoiserHits",
                width.div_ceil(DENOISER_BLOCK_SIZE),
                height.div_ceil(DENOISER_BLOCK_SIZE),
                DENOISER_TILE_FORMAT,
                1,
            )),
            denoiser_tiles: view(&texture_2d(
                device,
                "ScreenSpaceReflection::DenoiserTiles",
                width.div_ceil(DENOISER_TILE_SIZE),
                height.div_ceil(DENOISER_TILE_SIZE),
                DENOISER_TILE_FORMAT,
                1,
            )),
            resolved_radiance: view(&texture_2d(
                device,
                "ScreenSpaceReflection::ResolvedRadiance",
                width,
                height,
                RADIANCE_FORMAT,
                1,
            )),
            resolved_variance: view(&texture_2d(
                device,
                "ScreenSpaceReflection::ResolvedVariance",
                width,
                height,
                VARIANCE_FORMAT,
                1,
            )),
            resolved_depth: view(&texture_2d(
                device,
                "ScreenSpaceReflection::ResolvedDepth",
                width,
                height,
                RESOLVED_DEPTH_FORMAT,
                1,
            )),
            radiance_history,
            variance_history,
            output,
        });
    }

    /// Executes the screen-space reflection effect.
    pub fn execute(
        &mut self,
        render_attribs: &mut RenderAttributes<'_, '_>,
    ) -> PostFxExecutionStatus {
        let all_psos_ready = self.prepare_shaders_and_pso(render_attribs.device)
            && render_attribs.post_fx_context.is_psos_ready();
        self.update_constant_buffer(render_attribs, !all_psos_ready);
        self.reset_history(render_attribs);
        if all_psos_ready {
            self.compute_hierarchical_depth_buffer(render_attribs);
            self.compute_stencil_mask_and_extract_roughness(render_attribs);
            self.compute_downsampled_stencil_mask(render_attribs);
            self.compute_intersection(render_attribs);
            self.classify_denoiser_tiles(render_attribs);
            self.compute_spatial_reconstruction(render_attribs);
            self.compute_temporal_accumulation(render_attribs);
            self.compute_bilateral_cleanup(render_attribs);
        } else {
            self.compute_placeholder_texture(render_attribs);
        }

        if all_psos_ready {
            PostFxExecutionStatus::Ready
        } else {
            PostFxExecutionStatus::Pending
        }
    }

    /// Returns the shader resource view of the screen-space reflection texture.
    pub fn get_ssr_radiance_srv(&self) -> &wgpu::TextureView {
        &self.resources().output
    }

    fn resources(&self) -> &Resources {
        self.resources
            .as_ref()
            .expect("ScreenSpaceReflection::PrepareResources")
    }

    fn render_technique(&mut self, render_tech: RenderTech) -> &mut PostFXRenderTechnique {
        self.render_tech
            .entry(RenderTechniqueKey {
                render_tech,
                feature_flags: self.feature_flags,
                use_reverse_depth: self.use_reverse_depth,
            })
            .or_default()
    }

    fn technique(&self, render_tech: RenderTech) -> &PostFXRenderTechnique {
        &self.render_tech[&RenderTechniqueKey {
            render_tech,
            feature_flags: self.feature_flags,
            use_reverse_depth: self.use_reverse_depth,
        }]
    }

    fn prepare_shaders_and_pso(&mut self, device: &wgpu::Device) -> bool {
        let flag = |set: bool| if set { "1" } else { "0" };
        let tile_size = DENOISER_TILE_SIZE.to_string();
        let macros = [
            // TextureSubresourceViews
            ("SUPPORTED_SHADER_SRV", "1"),
            ("SSR_OPTION_INVERTED_DEPTH", flag(self.use_reverse_depth)),
            (
                "SSR_OPTION_PREVIOUS_FRAME",
                flag(self.feature_flags.contains(FeatureFlags::PREVIOUS_FRAME)),
            ),
            (
                "SSR_OPTION_HALF_RESOLUTION",
                flag(self.feature_flags.contains(FeatureFlags::HALF_RESOLUTION)),
            ),
            ("SSR_DENOISER_TILE_SIZE", tile_size.as_str()),
        ];
        let previous_frame = self.feature_flags.contains(FeatureFlags::PREVIOUS_FRAME);

        // We clear depth to 0.0 and then write 1.0 to mask pixels with reflection.
        let triangle_depth_05 = [("TRIANGLE_DEPTH", "0.5")];
        let triangle_depth_10 = [("TRIANGLE_DEPTH", "1.0")];
        let vs = |macros: &[(&str, &str)]| {
            create_shader(
                device,
                "FullScreenTriangleVS.fx",
                "FullScreenTriangleVS",
                macros,
            )
        };
        let ps = |file: &str, entry: &'static str| create_shader(device, file, entry, &macros);

        {
            let tech = self.render_technique(RenderTech::ComputeHierarchicalDepthBuffer);
            if !tech.is_initialized_pso() {
                tech.initialize_pso(
                    device,
                    "ScreenSpaceReflection::ComputeHierarchicalDepthBuffer",
                    &vs(&[]),
                    &ps(
                        "SSR_ComputeHierarchicalDepthBuffer.fx",
                        "ComputeHierarchicalDepthBufferPS",
                    ),
                    &[(0, Resource::Texture)],
                    &[DEPTH_HIERARCHY_FORMAT],
                    None,
                    DepthStencilStateDesc::DisableDepth,
                );
            }
        }
        {
            let tech = self.render_technique(RenderTech::ComputeStencilMaskAndExtractRoughness);
            if !tech.is_initialized_pso() {
                tech.initialize_pso(
                    device,
                    "ScreenSpaceReflection::ComputeStencilMaskAndExtractRoughness",
                    &vs(&triangle_depth_10),
                    &ps(
                        "SSR_ComputeStencilMaskAndExtractRoughness.fx",
                        "ComputeStencilMaskAndExtractRoughnessPS",
                    ),
                    &[
                        (0, Resource::ConstantBuffer),
                        (1, Resource::Texture),
                        (2, Resource::DepthTexture),
                    ],
                    &[ROUGHNESS_FORMAT],
                    Some(DEPTH_STENCIL_FORMAT),
                    DepthStencilStateDesc::WriteAlways,
                );
            }
        }
        {
            let tech = self.render_technique(RenderTech::ComputeDownsampledStencilMask);
            if !tech.is_initialized_pso() {
                tech.initialize_pso(
                    device,
                    "ScreenSpaceReflection::ComputeDownsampledStencilMask",
                    &vs(&triangle_depth_10),
                    &ps(
                        "SSR_ComputeDownsampledStencilMask.fx",
                        "ComputeDownsampledStencilMaskPS",
                    ),
                    &[
                        (0, Resource::ConstantBuffer),
                        (1, Resource::Texture),
                        (2, Resource::DepthTexture),
                    ],
                    &[],
                    Some(DEPTH_STENCIL_FORMAT),
                    DepthStencilStateDesc::WriteAlways,
                );
            }
        }
        {
            let mut layout = vec![
                (0, Resource::ConstantBuffer),
                (1, Resource::ConstantBuffer),
                (2, Resource::Texture),
                (3, Resource::Texture),
                (4, Resource::Texture),
                (6, Resource::Texture),
                (7, Resource::Texture),
            ];
            if previous_frame {
                layout.push((5, Resource::Texture));
            }
            let tech = self.render_technique(RenderTech::ComputeIntersection);
            if !tech.is_initialized_pso() {
                tech.initialize_pso(
                    device,
                    "ScreenSpaceReflection::ComputeIntersection",
                    &vs(&triangle_depth_05),
                    &ps("SSR_ComputeIntersection.fx", "ComputeIntersectionPS"),
                    &layout,
                    &[RADIANCE_FORMAT, RADIANCE_FORMAT],
                    Some(DEPTH_STENCIL_FORMAT),
                    DepthStencilStateDesc::EnableDepthNoWrites,
                );
            }
        }
        {
            let classify: &[_] = &[
                (1, Resource::FilterableTexture),
                (3, Resource::Sampler { filtering: true }),
            ];
            let dilate: &[_] = &[
                (0, Resource::ConstantBuffer),
                (2, Resource::FilterableTexture),
                (3, Resource::Sampler { filtering: true }),
            ];
            for (render_tech, entry, layout) in [
                (
                    RenderTech::ClassifyDenoiserTiles,
                    "ClassifyDenoiserTilesPS",
                    classify,
                ),
                (
                    RenderTech::DilateDenoiserTiles,
                    "DilateDenoiserTilesPS",
                    dilate,
                ),
            ] {
                let tech = self.render_technique(render_tech);
                if !tech.is_initialized_pso() {
                    tech.initialize_pso(
                        device,
                        "ScreenSpaceReflection::DenoiserTiles",
                        &vs(&[]),
                        &ps("SSR_ComputeDenoiserTiles.fx", entry),
                        layout,
                        &[DENOISER_TILE_FORMAT],
                        None,
                        DepthStencilStateDesc::DisableDepth,
                    );
                }
            }
        }
        {
            let tech = self.render_technique(RenderTech::ComputeSpatialReconstruction);
            if !tech.is_initialized_pso() {
                tech.initialize_pso(
                    device,
                    "ScreenSpaceReflection::ComputeSpatialReconstruction",
                    &vs(&triangle_depth_05),
                    &ps(
                        "SSR_ComputeSpatialReconstruction.fx",
                        "ComputeSpatialReconstructionPS",
                    ),
                    &[
                        (0, Resource::ConstantBuffer),
                        (1, Resource::ConstantBuffer),
                        (2, Resource::Texture),
                        (3, Resource::Texture),
                        (4, Resource::DepthTexture),
                        (5, Resource::Texture),
                        (6, Resource::Texture),
                        (7, Resource::Texture),
                    ],
                    &[RADIANCE_FORMAT, VARIANCE_FORMAT, RESOLVED_DEPTH_FORMAT],
                    Some(DEPTH_STENCIL_FORMAT),
                    DepthStencilStateDesc::EnableDepthNoWrites,
                );
            }
        }
        {
            let tech = self.render_technique(RenderTech::ComputeTemporalAccumulation);
            if !tech.is_initialized_pso() {
                tech.initialize_pso(
                    device,
                    "ScreenSpaceReflection::ComputeTemporalAccumulation",
                    &vs(&triangle_depth_05),
                    &ps(
                        "SSR_ComputeTemporalAccumulation.fx",
                        "ComputeTemporalAccumulationPS",
                    ),
                    &[
                        (0, Resource::ConstantBuffer),
                        (1, Resource::ConstantBuffer),
                        (2, Resource::Texture),
                        (3, Resource::Texture),
                        (4, Resource::Texture),
                        (5, Resource::Texture),
                        (6, Resource::Texture),
                        (7, Resource::Texture),
                        (8, Resource::FilterableTexture),
                        (9, Resource::FilterableTexture),
                        (10, Resource::Sampler { filtering: true }),
                        (11, Resource::Sampler { filtering: true }),
                        (12, Resource::Texture),
                    ],
                    &[RADIANCE_FORMAT, VARIANCE_FORMAT],
                    Some(DEPTH_STENCIL_FORMAT),
                    DepthStencilStateDesc::EnableDepthNoWrites,
                );
            }
        }
        {
            let tech = self.render_technique(RenderTech::ComputeBilateralCleanup);
            if !tech.is_initialized_pso() {
                tech.initialize_pso(
                    device,
                    "ScreenSpaceReflection::ComputeBilateralCleanup",
                    &vs(&triangle_depth_05),
                    &ps(
                        "SSR_ComputeBilateralCleanup.fx",
                        "ComputeBilateralCleanupPS",
                    ),
                    &[
                        (0, Resource::ConstantBuffer),
                        (1, Resource::ConstantBuffer),
                        (2, Resource::DepthTexture),
                        (3, Resource::Texture),
                        (4, Resource::Texture),
                        (5, Resource::Texture),
                        (6, Resource::Texture),
                        (7, Resource::Texture),
                    ],
                    &[RADIANCE_FORMAT],
                    Some(DEPTH_STENCIL_FORMAT),
                    DepthStencilStateDesc::EnableDepthNoWrites,
                );
            }
        }
        // wgpu creates pipelines synchronously.
        true
    }

    /// DFX-12: DiligentFX's TAA rule for when history is invalid
    /// (`TemporalAntiAliasing::AccumulationBufferInfo::UpdateConstantBuffer`),
    /// applied to SSR's histories by clearing them as `PrepareResources`
    /// does when it creates them.
    fn reset_history(&mut self, render_attribs: &mut RenderAttributes<'_, '_>) {
        let current_frame_idx = render_attribs.post_fx_context.get_frame_desc().index;
        let reset_accumulation = self.last_frame_idx == !0 // No history on the first frame
            || current_frame_idx != self.last_frame_idx.wrapping_add(1) // Reset history if frames were skipped
            || render_attribs.reset_accumulation; // Reset history if requested
        self.last_frame_idx = current_frame_idx;
        if !reset_accumulation {
            return;
        }
        let r = self.resources();
        for history in r.radiance_history.iter().chain(&r.variance_history) {
            render_attribs.post_fx_context.clear_render_target(
                &mut TextureOperationAttribs {
                    device: render_attribs.device,
                    device_context: render_attribs.device_context,
                    timestamp_writes: None,
                },
                history,
                [0.0, 0.0, 0.0, 0.0],
            );
        }
    }

    fn update_constant_buffer(
        &mut self,
        render_attribs: &RenderAttributes<'_, '_>,
        reset_timer: bool,
    ) {
        let elapsed = match self.frame_timer {
            Some(elapsed) if !reset_timer => elapsed + render_attribs.frame_time,
            _ => 0.0,
        };
        self.frame_timer = Some(elapsed);

        let alpha = render_attribs.post_fx_context.get_transition_alpha(elapsed);
        let mut attribs = *render_attribs.ssr_attribs;
        attribs.alpha_interpolation = alpha;
        let update_required = self.ssr_attribs.alpha_interpolation != alpha
            || bytemuck::bytes_of(render_attribs.ssr_attribs)
                != bytemuck::bytes_of(&self.ssr_attribs);
        if update_required {
            self.ssr_attribs = attribs;
            render_attribs.queue.write_buffer(
                &self.constant_buffer,
                0,
                bytemuck::bytes_of(&self.ssr_attribs),
            );
        }
    }

    fn compute_hierarchical_depth_buffer(&mut self, render_attribs: &mut RenderAttributes<'_, '_>) {
        // CopyDepthToColor is false on wgpu: the context copies with a draw.
        render_attribs.post_fx_context.copy_texture_depth(
            &mut TextureOperationAttribs {
                device: render_attribs.device,
                device_context: render_attribs.device_context,
                timestamp_writes: render_attribs
                    .pass_timestamps
                    .and_then(|timestamps| timestamps("ComputeHierarchicalDepthBuffer")),
            },
            render_attribs.depth_buffer_srv,
            &self.hierarchical_depth_mip_map_rtv[0],
            DEPTH_HIERARCHY_FORMAT,
        );

        let tech = self.technique(RenderTech::ComputeHierarchicalDepthBuffer);
        for mip_level in 1..self.hierarchical_depth_mip_map_rtv.len() {
            let group = tech.bind_group(
                render_attribs.device,
                "ComputeHierarchicalDepthBuffer",
                &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: texture(&self.hierarchical_depth_mip_map_srv[mip_level - 1]),
                }],
            );
            draw_pass(
                render_attribs.device_context,
                "ComputeHierarchicalDepthBuffer",
                &[&self.hierarchical_depth_mip_map_rtv[mip_level]],
                false,
                None,
                tech,
                &group,
                render_attribs
                    .pass_timestamps
                    .and_then(|timestamps| timestamps("ComputeHierarchicalDepthBuffer")),
            );
        }
    }

    fn compute_stencil_mask_and_extract_roughness(
        &mut self,
        render_attribs: &mut RenderAttributes<'_, '_>,
    ) {
        let tech = self.technique(RenderTech::ComputeStencilMaskAndExtractRoughness);
        let r = self.resources();
        let group = tech.bind_group(
            render_attribs.device,
            "ComputeStencilMaskAndExtractRoughness",
            &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.constant_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: texture(render_attribs.material_buffer_srv),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: texture(render_attribs.depth_buffer_srv),
                },
            ],
        );
        // The shader writes current roughness for every pixel and a 0/1
        // reflection eligibility depth mask (DFX-21).
        draw_pass(
            render_attribs.device_context,
            "ComputeStencilMaskAndExtractRoughness",
            &[&r.roughness],
            false,
            Some(DepthStencil::Clear(&r.depth_stencil_mask)),
            tech,
            &group,
            render_attribs
                .pass_timestamps
                .and_then(|timestamps| timestamps("ComputeStencilMaskAndExtractRoughness")),
        );
    }

    fn compute_downsampled_stencil_mask(&mut self, render_attribs: &mut RenderAttributes<'_, '_>) {
        if !self.feature_flags.contains(FeatureFlags::HALF_RESOLUTION) {
            return;
        }

        let tech = self.technique(RenderTech::ComputeDownsampledStencilMask);
        let r = self.resources();
        let group = tech.bind_group(
            render_attribs.device,
            "ComputeDownsampledStencilMask",
            &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.constant_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: texture(&r.roughness),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: texture(render_attribs.depth_buffer_srv),
                },
            ],
        );
        // Clear depth to 0.0. Pixels that are not discarded write 1.0.
        draw_pass(
            render_attribs.device_context,
            "ComputeDownsampledStencilMask",
            &[],
            false,
            Some(DepthStencil::Clear(
                r.depth_stencil_mask_half_res.as_ref().unwrap(),
            )),
            tech,
            &group,
            render_attribs
                .pass_timestamps
                .and_then(|timestamps| timestamps("ComputeDownsampledStencilMask")),
        );
    }

    fn compute_intersection(&mut self, render_attribs: &mut RenderAttributes<'_, '_>) {
        let tech = self.technique(RenderTech::ComputeIntersection);
        let r = self.resources();
        let ctx = &*render_attribs.post_fx_context;
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: ctx.get_camera_attribs_cb().as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: self.constant_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: texture(render_attribs.color_buffer_srv),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: texture(render_attribs.normal_buffer_srv),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: texture(&r.roughness),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: texture(ctx.get_2d_blue_noise_srv(BlueNoiseDimension::Xy)),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: texture(&r.depth_hierarchy),
            },
        ];
        if self.feature_flags.contains(FeatureFlags::PREVIOUS_FRAME) {
            entries.push(wgpu::BindGroupEntry {
                binding: 5,
                resource: texture(render_attribs.motion_vectors_srv),
            });
        }
        let group = tech.bind_group(render_attribs.device, "ComputeIntersection", &entries);
        let dsv = if self.feature_flags.contains(FeatureFlags::HALF_RESOLUTION) {
            r.depth_stencil_mask_half_res.as_ref().unwrap()
        } else {
            &r.depth_stencil_mask
        };
        draw_pass(
            render_attribs.device_context,
            "ComputeIntersection",
            &[&r.radiance, &r.ray_direction_pdf],
            true,
            Some(DepthStencil::ReadOnly(dsv)),
            tech,
            &group,
            render_attribs
                .pass_timestamps
                .and_then(|timestamps| timestamps("ComputeIntersection")),
        );
    }

    /// DFX-29: the tiles the denoiser passes work on, as AMD's ClassifyTiles
    /// lists them. Timed with spatial reconstruction, the first pass to use
    /// them.
    fn classify_denoiser_tiles(&mut self, render_attribs: &mut RenderAttributes<'_, '_>) {
        let r = self.resources();
        let sampler = wgpu::BindGroupEntry {
            binding: 3,
            resource: wgpu::BindingResource::Sampler(&self.linear_clamp),
        };
        for (render_tech, entries, output) in [
            (
                RenderTech::ClassifyDenoiserTiles,
                vec![
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: texture(&r.radiance),
                    },
                    sampler.clone(),
                ],
                &r.denoiser_hits,
            ),
            (
                RenderTech::DilateDenoiserTiles,
                vec![
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.constant_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: texture(&r.denoiser_hits),
                    },
                    sampler.clone(),
                ],
                &r.denoiser_tiles,
            ),
        ] {
            let tech = self.technique(render_tech);
            let group = tech.bind_group(render_attribs.device, "DenoiserTiles", &entries);
            draw_pass(
                render_attribs.device_context,
                "DenoiserTiles",
                &[output],
                false,
                None,
                tech,
                &group,
                render_attribs
                    .pass_timestamps
                    .and_then(|timestamps| timestamps("SpatialReconstruction")),
            );
        }
    }

    fn compute_spatial_reconstruction(&mut self, render_attribs: &mut RenderAttributes<'_, '_>) {
        let tech = self.technique(RenderTech::ComputeSpatialReconstruction);
        let r = self.resources();
        let ctx = &*render_attribs.post_fx_context;
        let group = tech.bind_group(
            render_attribs.device,
            "SpatialReconstruction",
            &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ctx.get_camera_attribs_cb().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.constant_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: texture(&r.roughness),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: texture(render_attribs.normal_buffer_srv),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: texture(render_attribs.depth_buffer_srv),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: texture(&r.ray_direction_pdf),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: texture(&r.radiance),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: texture(&r.denoiser_tiles),
                },
            ],
        );
        draw_pass(
            render_attribs.device_context,
            "SpatialReconstruction",
            &[
                &r.resolved_radiance,
                &r.resolved_variance,
                &r.resolved_depth,
            ],
            // DFX-23: temporal neighbourhood statistics read outside the
            // active mask, so skipped pixels must contain this frame's zero.
            true,
            Some(DepthStencil::ReadOnly(&r.depth_stencil_mask)),
            tech,
            &group,
            render_attribs
                .pass_timestamps
                .and_then(|timestamps| timestamps("SpatialReconstruction")),
        );
    }

    fn compute_temporal_accumulation(&mut self, render_attribs: &mut RenderAttributes<'_, '_>) {
        let tech = self.technique(RenderTech::ComputeTemporalAccumulation);
        let r = self.resources();
        let ctx = &*render_attribs.post_fx_context;
        let frame_index = ctx.get_frame_desc().index;
        let curr_frame_idx = (frame_index & 0x01) as usize;
        let prev_frame_idx = (frame_index.wrapping_add(1) & 0x01) as usize;
        let group = tech.bind_group(
            render_attribs.device,
            "ComputeTemporalAccumulation",
            &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ctx.get_camera_attribs_cb().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.constant_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: texture(render_attribs.motion_vectors_srv),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: texture(&r.resolved_depth),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: texture(ctx.get_reprojected_depth()),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: texture(&r.resolved_radiance),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: texture(&r.resolved_variance),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: texture(ctx.get_previous_depth()),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: texture(&r.radiance_history[prev_frame_idx]),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: texture(&r.variance_history[prev_frame_idx]),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::Sampler(&self.linear_clamp),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::Sampler(&self.linear_clamp),
                },
                wgpu::BindGroupEntry {
                    binding: 12,
                    resource: texture(&r.denoiser_tiles),
                },
            ],
        );
        draw_pass(
            render_attribs.device_context,
            "ComputeTemporalAccumulation",
            &[
                &r.radiance_history[curr_frame_idx],
                &r.variance_history[curr_frame_idx],
            ],
            false,
            Some(DepthStencil::ReadOnly(&r.depth_stencil_mask)),
            tech,
            &group,
            render_attribs
                .pass_timestamps
                .and_then(|timestamps| timestamps("ComputeTemporalAccumulation")),
        );
    }

    fn compute_bilateral_cleanup(&mut self, render_attribs: &mut RenderAttributes<'_, '_>) {
        let tech = self.technique(RenderTech::ComputeBilateralCleanup);
        let r = self.resources();
        let ctx = &*render_attribs.post_fx_context;
        let curr_frame_idx = (ctx.get_frame_desc().index & 0x1) as usize;
        let group = tech.bind_group(
            render_attribs.device,
            "ComputeBilateralCleanup",
            &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ctx.get_camera_attribs_cb().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.constant_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: texture(render_attribs.depth_buffer_srv),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: texture(render_attribs.normal_buffer_srv),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: texture(&r.roughness),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: texture(&r.radiance_history[curr_frame_idx]),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: texture(&r.variance_history[curr_frame_idx]),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: texture(&r.denoiser_tiles),
                },
            ],
        );
        draw_pass(
            render_attribs.device_context,
            "ComputeBilateralCleanup",
            &[&r.output],
            true,
            Some(DepthStencil::ReadOnly(&r.depth_stencil_mask)),
            tech,
            &group,
            render_attribs
                .pass_timestamps
                .and_then(|timestamps| timestamps("ComputeBilateralCleanup")),
        );
    }

    fn compute_placeholder_texture(&mut self, render_attribs: &mut RenderAttributes<'_, '_>) {
        let output = &self.resources().output;
        render_attribs.post_fx_context.clear_render_target(
            &mut TextureOperationAttribs {
                device: render_attribs.device,
                device_context: render_attribs.device_context,
                timestamp_writes: None,
            },
            output,
            [0.0, 0.0, 0.0, 0.0],
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "ssr_roughness_tests.rs"]
mod roughness_tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "ssr_temporal_neighborhood_tests.rs"]
mod temporal_neighborhood_tests;
