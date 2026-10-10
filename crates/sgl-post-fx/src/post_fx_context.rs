//! Port of DiligentFX `PostProcess/Common/{interface/PostFXContext.hpp,
//! src/PostFXContext.cpp}` (revision
//! f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright 2024-2026 Diligent
//! Graphics LLC, licensed under the Apache License, Version 2.0
//! (vendor/DiligentFX/License.txt). Modified: rewritten in Rust over wgpu;
//! origins and implementation notes are recorded in PROVENANCE.md.
//!
//! Per-frame resources shared by the post effects: the camera constant buffer,
//! the blue-noise textures, the depth reprojected into the previous frame and
//! the previous frame's depth. TAA finds the closest motion vectors itself
//! (PROVENANCE.md DFX-13).
use crate::render_technique::{
    DepthStencilStateDesc, PostFXRenderTechnique, Resource, Shader, create_shader,
};
use crate::structures::CameraAttribs;
use std::collections::HashMap;
use wgpu::util::DeviceExt;

/// `PostFXContext::FEATURE_FLAGS`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FeatureFlags(pub u32);

impl FeatureFlags {
    pub const NONE: Self = Self(0);
    pub const REVERSED_DEPTH: Self = Self(1 << 0);
    pub const HALF_PRECISION_DEPTH: Self = Self(1 << 1);
    pub const TEMPORAL_UPSCALING: Self = Self(1 << 2);

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

/// `PostFXContext::FrameDesc`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameDesc {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub output_width: u32,
    pub output_height: u32,
}

/// Timestamp writes for a pass, by the name of the `ScopedDebugGroup` that
/// wraps it upstream: where a profiler reads per-pass GPU time.
pub type PassTimestamps<'a> =
    dyn Fn(&'static str) -> Option<wgpu::RenderPassTimestampWrites<'a>> + 'a;

/// `PostFXContext::RenderAttributes`. The device context is the encoder the
/// passes record into and the queue that uploads constants.
pub struct RenderAttributes<'a, 'p> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub device_context: &'a mut wgpu::CommandEncoder,
    pub curr_depth_buffer_srv: &'a wgpu::TextureView,
    pub prev_depth_buffer_srv: &'a wgpu::TextureView,
    pub curr_camera: Option<&'a CameraAttribs>,
    pub prev_camera: Option<&'a CameraAttribs>,
    /// Two `CameraAttribs`, current then previous, used instead of
    /// `curr_camera` and `prev_camera`.
    pub camera_attribs_cb: Option<&'a wgpu::Buffer>,
    /// Per-pass timestamps (the upstream debug groups), when profiling.
    pub pass_timestamps: Option<&'a PassTimestamps<'p>>,
}

/// `PostFXContext::TextureOperationAttribs`.
pub struct TextureOperationAttribs<'a, 'p> {
    pub device: &'a wgpu::Device,
    pub device_context: &'a mut wgpu::CommandEncoder,
    /// Timestamp writes for the operation's pass, when profiling (the
    /// caller's debug group).
    pub timestamp_writes: Option<wgpu::RenderPassTimestampWrites<'p>>,
}

/// `PostFXContext::BLUE_NOISE_DIMENSION`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlueNoiseDimension {
    Xy = 0,
    Zw = 1,
}

/// `PostFXContext::SupportedDeviceFeatures`: fixed for wgpu (PROVENANCE.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SupportedDeviceFeatures {
    pub transition_subresources: bool,
    pub texture_subresource_views: bool,
    pub copy_depth_to_color: bool,
    /// Indicates whether the Base Vertex is added to the VertexID in the vertex shader.
    pub shader_base_vertex_offset: bool,
}

/// `PostFXContext::CreateInfo`. `EnableAsyncCreation` and `PackMatrixRowMajor`
/// have no wgpu counterpart (PROVENANCE.md).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CreateInfo {
    pub transition_duration: f32,
}

impl Default for CreateInfo {
    fn default() -> Self {
        Self {
            transition_duration: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RenderTech {
    ComputeBlueNoiseTexture,
    ComputeReprojectedDepth,
    ComputePreviousDepth,
    CopyDepth,
    CopyColor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RenderTechniqueKey {
    render_tech: RenderTech,
    feature_flags: FeatureFlags,
    texture_format: Option<wgpu::TextureFormat>,
}

/// The two tables of `SamplerBlueNoiseErrorDistribution_128x128_OptimizedFor_2d2d2d2d_1spp.cpp`,
/// read from the vendored source: `Sobol_256d` (256 bytes) and
/// `ScramblingTile` (128 × 4 × 128 × 2 bytes).
fn noise_buffers() -> (Vec<u8>, Vec<u8>) {
    const SOURCE: &str = include_str!(
        "../vendor/DiligentFX/PostProcess/Common/src/SamplerBlueNoiseErrorDistribution_128x128_OptimizedFor_2d2d2d2d_1spp.cpp"
    );
    let table = |name: &str| -> Vec<u8> {
        let start = SOURCE.find(name).expect("noise table");
        let body = &SOURCE[start..];
        let open = body.find('{').unwrap();
        let close = body.find('}').unwrap();
        body[open + 1..close]
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.parse().expect("noise table value"))
            .collect()
    };
    (table("Sobol_256d["), table("ScramblingTile["))
}

pub struct PostFXContext {
    settings: CreateInfo,
    supported_features: SupportedDeviceFeatures,
    frame_desc: FrameDesc,
    feature_flags: FeatureFlags,
    pso_ready: bool,
    render_tech: HashMap<RenderTechniqueKey, PostFXRenderTechnique>,
    vs_copy_texture: Shader,

    sobol_buffer: wgpu::TextureView,
    scrambling_tile_buffer: wgpu::TextureView,
    blue_noise_textures: [wgpu::TextureView; 2],
    constant_buffer: Option<wgpu::Buffer>,
    reprojected_depth: Option<wgpu::TextureView>,
    previous_depth: Option<wgpu::TextureView>,
    point_clamp: wgpu::Sampler,
    linear_clamp: wgpu::Sampler,
}

fn texture_2d(
    device: &wgpu::Device,
    name: &str,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(name),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}

fn view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&Default::default())
}

impl PostFXContext {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, ci: CreateInfo) -> Self {
        let (sobol, scrambling) = noise_buffers();
        let upload = |name, width, height, data: &[u8]| {
            // We use RESOURCE_DIM_TEX_2D, because WebGL doesn't support glTexStorage1D()
            view(&device.create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: Some(name),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Uint,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                data,
            ))
        };
        let blue_noise = || {
            view(&texture_2d(
                device,
                "PostFXContext::BlueNoiseTexture",
                128,
                128,
                wgpu::TextureFormat::Rg8Unorm,
            ))
        };
        let sampler = |name, filter, mipmap_filter| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(name),
                mag_filter: filter,
                min_filter: filter,
                mipmap_filter,
                ..Default::default()
            })
        };
        Self {
            settings: ci,
            // wgpu transitions resources itself, always has texture
            // subresource views, cannot copy depth to color and adds the base
            // vertex to vertex_index.
            supported_features: SupportedDeviceFeatures {
                transition_subresources: false,
                texture_subresource_views: true,
                copy_depth_to_color: false,
                shader_base_vertex_offset: true,
            },
            frame_desc: FrameDesc::default(),
            feature_flags: FeatureFlags::NONE,
            pso_ready: false,
            render_tech: HashMap::new(),
            vs_copy_texture: create_shader(device, "PostFXContext_ScreenTriangleVS", "main", &[]),
            sobol_buffer: upload("PostFXContext::SobolBuffer", 256, 1, &sobol),
            scrambling_tile_buffer: upload(
                "PostFXContext::ScramblingTileBuffer",
                128 * 4,
                128 * 2,
                &scrambling,
            ),
            blue_noise_textures: [blue_noise(), blue_noise()],
            constant_buffer: None,
            reprojected_depth: None,
            previous_depth: None,
            point_clamp: sampler(
                "Sam_PointClamp",
                wgpu::FilterMode::Nearest,
                wgpu::MipmapFilterMode::Nearest,
            ),
            linear_clamp: sampler(
                "Sam_LinearClamp",
                wgpu::FilterMode::Linear,
                wgpu::MipmapFilterMode::Linear,
            ),
        }
    }

    pub fn prepare_resources(
        &mut self,
        device: &wgpu::Device,
        desc: &FrameDesc,
        feature_flags: FeatureFlags,
    ) {
        self.frame_desc.index = desc.index;
        self.feature_flags = feature_flags;

        if self.frame_desc.width == desc.width && self.frame_desc.height == desc.height {
            return;
        }

        self.frame_desc = *desc;

        let depth_format = if feature_flags.contains(FeatureFlags::HALF_PRECISION_DEPTH) {
            wgpu::TextureFormat::R16Unorm
        } else {
            wgpu::TextureFormat::R32Float
        };
        let (width, height) = (self.frame_desc.width, self.frame_desc.height);
        self.reprojected_depth = Some(view(&texture_2d(
            device,
            "PostFXContext::ReprojectedDepth",
            width,
            height,
            depth_format,
        )));
        self.previous_depth = Some(view(&texture_2d(
            device,
            "PostFXContext::PreviousDepth",
            width,
            height,
            depth_format,
        )));
    }

    pub fn execute(&mut self, render_attribs: &mut RenderAttributes<'_, '_>) {
        if let Some(buffer) = render_attribs.camera_attribs_cb {
            self.constant_buffer = Some(buffer.clone());
        } else {
            let curr = render_attribs
                .curr_camera
                .expect("RenderAttribs.pCurrCamera must not be null");
            let prev = render_attribs
                .prev_camera
                .expect("RenderAttribs.pPrevCamera must not be null");
            let buffer = self.constant_buffer.get_or_insert_with(|| {
                render_attribs
                    .device
                    .create_buffer(&wgpu::BufferDescriptor {
                        label: Some("PostFXContext::CameraAttibsConstantBuffer"),
                        size: 2 * size_of::<CameraAttribs>() as u64,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    })
            });
            render_attribs
                .queue
                .write_buffer(buffer, 0, bytemuck::cast_slice(&[*curr, *prev]));
        }

        self.pso_ready = self.prepare_shaders_and_pso(render_attribs.device, self.feature_flags);

        if self.pso_ready {
            self.compute_blue_noise_texture(render_attribs);
            self.compute_reprojected_depth(render_attribs);
            self.compute_previous_depth(render_attribs);
        }
    }

    pub fn is_psos_ready(&self) -> bool {
        self.pso_ready
    }

    pub fn get_transition_alpha(&self, elapsed_time: f32) -> f32 {
        if self.settings.transition_duration <= 0.0 {
            return 1.0;
        }

        (elapsed_time / self.settings.transition_duration).clamp(0.0, 1.0)
    }

    pub fn get_2d_blue_noise_srv(&self, dimension: BlueNoiseDimension) -> &wgpu::TextureView {
        &self.blue_noise_textures[dimension as usize]
    }

    pub fn get_reprojected_depth(&self) -> &wgpu::TextureView {
        self.reprojected_depth.as_ref().expect("prepared resources")
    }

    pub fn get_previous_depth(&self) -> &wgpu::TextureView {
        self.previous_depth.as_ref().expect("prepared resources")
    }

    pub fn get_camera_attribs_cb(&self) -> &wgpu::Buffer {
        self.constant_buffer.as_ref().expect("executed context")
    }

    pub fn get_supported_features(&self) -> &SupportedDeviceFeatures {
        &self.supported_features
    }

    pub fn get_feature_flags(&self) -> FeatureFlags {
        self.feature_flags
    }

    pub fn get_frame_desc(&self) -> &FrameDesc {
        &self.frame_desc
    }

    pub fn clear_render_target(
        &self,
        attribs: &mut TextureOperationAttribs<'_, '_>,
        texture: &wgpu::TextureView,
        clear_color: [f32; 4],
    ) {
        let [r, g, b, a] = clear_color.map(f64::from);
        attribs
            .device_context
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("PostFXContext::ClearRenderTarget"),
                timestamp_writes: attribs.timestamp_writes.take(),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: texture,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
    }

    pub fn copy_texture_depth(
        &mut self,
        attribs: &mut TextureOperationAttribs<'_, '_>,
        srv: &wgpu::TextureView,
        rtv: &wgpu::TextureView,
        rtv_format: wgpu::TextureFormat,
    ) {
        self.copy_texture(attribs, RenderTech::CopyDepth, srv, rtv, rtv_format);
    }

    pub fn copy_texture_color(
        &mut self,
        attribs: &mut TextureOperationAttribs<'_, '_>,
        srv: &wgpu::TextureView,
        rtv: &wgpu::TextureView,
        rtv_format: wgpu::TextureFormat,
    ) {
        self.copy_texture(attribs, RenderTech::CopyColor, srv, rtv, rtv_format);
    }

    fn copy_texture(
        &mut self,
        attribs: &mut TextureOperationAttribs<'_, '_>,
        tech: RenderTech,
        srv: &wgpu::TextureView,
        rtv: &wgpu::TextureView,
        rtv_format: wgpu::TextureFormat,
    ) {
        let depth = tech == RenderTech::CopyDepth;
        let key = RenderTechniqueKey {
            render_tech: tech,
            feature_flags: FeatureFlags::NONE,
            texture_format: Some(rtv_format),
        };
        let vs_copy_texture = &self.vs_copy_texture;
        let render_tech = self.render_tech.entry(key).or_default();
        if !render_tech.is_initialized_pso() {
            let ps = create_shader(
                attribs.device,
                "PostFXContext_CopyTexturePS",
                "main",
                &[("COPY_TEXTURE_DEPTH", if depth { "1" } else { "0" })],
            );
            render_tech.initialize_pso(
                attribs.device,
                if depth {
                    "PostFXContext::CopyTextureDepth"
                } else {
                    "PostFXContext::CopyTextureColor"
                },
                vs_copy_texture,
                &ps,
                &[
                    (
                        0,
                        if depth {
                            Resource::DepthTexture
                        } else {
                            Resource::FilterableTexture
                        },
                    ),
                    (1, Resource::Sampler { filtering: !depth }),
                ],
                &[rtv_format],
                None,
                DepthStencilStateDesc::DisableDepth,
            );
        }
        let group = render_tech.bind_group(
            attribs.device,
            "PostFXContext::CopyTexture",
            &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(srv),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(if depth {
                        &self.point_clamp
                    } else {
                        &self.linear_clamp
                    }),
                },
            ],
        );
        draw(
            attribs.device_context,
            "PostFXContext::CopyTexture",
            &[rtv],
            render_tech,
            &group,
            0..3,
            attribs.timestamp_writes.take(),
        );
    }

    fn render_technique(
        &mut self,
        render_tech: RenderTech,
        feature_flags: FeatureFlags,
    ) -> &mut PostFXRenderTechnique {
        self.render_tech
            .entry(RenderTechniqueKey {
                render_tech,
                feature_flags,
                texture_format: None,
            })
            .or_default()
    }

    fn prepare_shaders_and_pso(
        &mut self,
        device: &wgpu::Device,
        feature_flags: FeatureFlags,
    ) -> bool {
        let vs = || {
            create_shader(
                device,
                "FullScreenTriangleVS.fx",
                "FullScreenTriangleVS",
                &[],
            )
        };
        {
            let tech = self.render_technique(RenderTech::ComputeBlueNoiseTexture, feature_flags);
            if !tech.is_initialized_pso() {
                let ps = create_shader(
                    device,
                    "ComputeBlueNoiseTexture.fx",
                    "ComputeBlueNoiseTexturePS",
                    &[],
                );
                tech.initialize_pso(
                    device,
                    "PreparePostFX::ComputeBlueNoiseTexture",
                    &vs(),
                    &ps,
                    &[(0, Resource::UintTexture), (1, Resource::UintTexture)],
                    &[wgpu::TextureFormat::Rg8Unorm, wgpu::TextureFormat::Rg8Unorm],
                    None,
                    DepthStencilStateDesc::DisableDepth,
                );
            }
        }
        // The targets' formats, as upstream reads them from the textures.
        let reprojected_depth_format = self.get_reprojected_depth().texture().format();
        let previous_depth_format = self.get_previous_depth().texture().format();
        {
            let tech = self.render_technique(RenderTech::ComputeReprojectedDepth, feature_flags);
            if !tech.is_initialized_pso() {
                let ps = create_shader(
                    device,
                    "ComputeReprojectedDepth.fx",
                    "ComputeReprojectedDepthPS",
                    &[],
                );
                tech.initialize_pso(
                    device,
                    "PreparePostFX::ComputeReprojectedDepth",
                    &vs(),
                    &ps,
                    &[(0, Resource::ConstantBuffer), (1, Resource::DepthTexture)],
                    &[reprojected_depth_format],
                    None,
                    DepthStencilStateDesc::DisableDepth,
                );
            }
        }
        {
            let tech = self
                .render_tech
                .entry(RenderTechniqueKey {
                    render_tech: RenderTech::ComputePreviousDepth,
                    feature_flags,
                    texture_format: None,
                })
                .or_default();
            if !tech.is_initialized_pso() {
                // The input is the previous depth buffer: the depth
                // permutation of CopyTexturePS, sampled at texel centres.
                let ps = create_shader(
                    device,
                    "PostFXContext_CopyTexturePS",
                    "main",
                    &[("COPY_TEXTURE_DEPTH", "1")],
                );
                tech.initialize_pso(
                    device,
                    "PostFXContext::ComputePreviousDepth",
                    &self.vs_copy_texture,
                    &ps,
                    &[
                        (0, Resource::DepthTexture),
                        (1, Resource::Sampler { filtering: false }),
                    ],
                    &[previous_depth_format],
                    None,
                    DepthStencilStateDesc::DisableDepth,
                );
            }
        }
        true
    }

    fn compute_blue_noise_texture(&mut self, render_attribs: &mut RenderAttributes<'_, '_>) {
        let device = render_attribs.device;
        let tech = &self.render_tech[&RenderTechniqueKey {
            render_tech: RenderTech::ComputeBlueNoiseTexture,
            feature_flags: self.feature_flags,
            texture_format: None,
        }];
        let group = tech.bind_group(
            device,
            "ComputeBlueNoiseTexture",
            &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.sobol_buffer),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.scrambling_tile_buffer),
                },
            ],
        );
        draw(
            render_attribs.device_context,
            "ComputeBlueNoiseTexture",
            &[&self.blue_noise_textures[0], &self.blue_noise_textures[1]],
            tech,
            &group,
            blue_noise_vertices(self.frame_desc.index),
            render_attribs
                .pass_timestamps
                .and_then(|timestamps| timestamps("ComputeBlueNoiseTexture")),
        );
    }

    fn compute_reprojected_depth(&mut self, render_attribs: &mut RenderAttributes<'_, '_>) {
        let device = render_attribs.device;
        let tech = &self.render_tech[&RenderTechniqueKey {
            render_tech: RenderTech::ComputeReprojectedDepth,
            feature_flags: self.feature_flags,
            texture_format: None,
        }];
        let group = tech.bind_group(
            device,
            "ComputeReprojectedDepth",
            &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.get_camera_attribs_cb().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        render_attribs.curr_depth_buffer_srv,
                    ),
                },
            ],
        );
        draw(
            render_attribs.device_context,
            "ComputeReprojectedDepth",
            &[self.get_reprojected_depth()],
            tech,
            &group,
            0..3,
            render_attribs
                .pass_timestamps
                .and_then(|timestamps| timestamps("ComputeReprojectedDepth")),
        );
    }

    fn compute_previous_depth(&mut self, render_attribs: &mut RenderAttributes<'_, '_>) {
        let device = render_attribs.device;
        let tech = &self.render_tech[&RenderTechniqueKey {
            render_tech: RenderTech::ComputePreviousDepth,
            feature_flags: self.feature_flags,
            texture_format: None,
        }];
        let group = tech.bind_group(
            device,
            "ComputePreviousDepth",
            &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        render_attribs.prev_depth_buffer_srv,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.point_clamp),
                },
            ],
        );
        draw(
            render_attribs.device_context,
            "ComputePreviousDepth",
            &[self.get_previous_depth()],
            tech,
            &group,
            0..3,
            render_attribs
                .pass_timestamps
                .and_then(|timestamps| timestamps("ComputePreviousDepth")),
        );
    }
}

/// `SetRenderTargets`, `SetPipelineState`, `CommitShaderResources` and `Draw`
/// without a depth-stencil view: one render pass that keeps the targets'
/// contents (no clear).
pub(crate) fn draw(
    device_context: &mut wgpu::CommandEncoder,
    label: &str,
    rtvs: &[&wgpu::TextureView],
    tech: &PostFXRenderTechnique,
    group: &wgpu::BindGroup,
    vertices: std::ops::Range<u32>,
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
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })
        })
        .collect();
    let mut pass = device_context.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &attachments,
        timestamp_writes,
        ..Default::default()
    });
    pass.set_pipeline(tech.pso.as_ref().expect("initialized technique"));
    pass.set_bind_group(0, group, &[]);
    pass.draw(vertices, 0..1);
}

/// The frame indices the blue noise draws cycle through (PROVENANCE.md
/// DFX-44): a power of two, so the low bits the shader masks (`& 0xFF`,
/// `& 127`) run on across the wrap, small enough that the last frame's three
/// vertices fit in `u32`.
const BLUE_NOISE_FRAME_PERIOD: u32 = 1 << 30;

/// The blue-noise triangle's vertices for frame `index`
/// (`ShaderBaseVertexOffset`): the frame index reaches the shader as
/// `VertexId / 3`.
fn blue_noise_vertices(index: u32) -> std::ops::Range<u32> {
    let start = 3 * (index % BLUE_NOISE_FRAME_PERIOD);
    start..start + 3
}

#[cfg(test)]
mod tests {
    use super::blue_noise_vertices;

    // #346: the frame indices at which `3 × index` wraps to just below
    // `u32::MAX` or past it, and the last index.
    #[test]
    fn blue_noise_draws_three_vertices_at_every_frame_index() {
        for index in [0, 1, 1_431_655_765, 2_863_311_530, u32::MAX] {
            let vertices = blue_noise_vertices(index);
            assert_eq!(vertices.len(), 3, "frame {index}: {vertices:?}");
            // The shader's frame index keeps the frame's low bits.
            assert_eq!((vertices.start / 3) & 0xFF, index & 0xFF, "frame {index}");
        }
    }
}
