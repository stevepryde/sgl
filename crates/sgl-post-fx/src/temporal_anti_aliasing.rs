//! Port of DiligentFX `PostProcess/TemporalAntiAliasing/{interface/TemporalAntiAliasing.hpp,
//! src/TemporalAntiAliasing.cpp}` (revision
//! f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright 2024-2026 Diligent
//! Graphics LLC, licensed under the Apache License, Version 2.0
//! (vendor/DiligentFX/License.txt). Modified: rewritten in Rust over wgpu;
//! origins and implementation notes are recorded in PROVENANCE.md.
//!
//! Implements [temporal anti-aliasing post-process effect](https://github.com/DiligentGraphics/DiligentFX/tree/master/PostProcess/TemporalAntiAliasing).
use crate::post_fx_context::{self, PostFXContext, TextureOperationAttribs, draw};
use crate::render_technique::{
    DepthStencilStateDesc, PostFXRenderTechnique, Resource, create_shader,
};
use crate::screen_space_reflection::PostFxExecutionStatus;
use crate::structures::TemporalAntiAliasingAttribs;
use std::collections::HashMap;
use wgpu::util::DeviceExt;

/// `TemporalAntiAliasing::FEATURE_FLAGS`: feature flags that control the
/// behavior of the effect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FeatureFlags(pub u32);

impl FeatureFlags {
    pub const NONE: Self = Self(0);
    /// Use Gaussian weighting in the variance clipping step.
    pub const GAUSSIAN_WEIGHTING: Self = Self(1 << 0);
    /// Use Catmull-Rom filter to sample the history buffer.
    pub const BICUBIC_FILTER: Self = Self(1 << 1);
    /// Use YCoCg color space for color clipping.
    pub const YCOCG_COLOR_SPACE: Self = Self(1 << 2);

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

/// Render attributes that are passed to the effect. The device context is
/// the encoder the pass records into and the queue that uploads constants.
pub struct RenderAttributes<'a, 'p> {
    /// Render device that may be used to create new objects needed for this frame, if any.
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    /// Device context that will record the rendering commands.
    pub device_context: &'a mut wgpu::CommandEncoder,
    /// PostFX context.
    pub post_fx_context: &'a mut PostFXContext,
    /// Shader resource view of the source color.
    pub color_buffer_srv: &'a wgpu::TextureView,
    /// Shader resource view of the depth buffer, the context's
    /// `curr_depth_buffer_srv`, in which the closest motion vectors are
    /// found (PROVENANCE.md DFX-13).
    pub depth_buffer_srv: &'a wgpu::TextureView,
    /// Shader resource view of the motion vectors, in a filterable float
    /// format such as `Rg16Float`.
    pub motion_vectors_srv: &'a wgpu::TextureView,
    /// TAA settings.
    pub taa_attribs: &'a TemporalAntiAliasingAttribs,
    /// Accumulation buffer index.
    pub accumulation_buffer_idx: u32,
    /// Per-pass timestamps (the upstream debug groups), when profiling.
    pub pass_timestamps: Option<&'a post_fx_context::PassTimestamps<'p>>,
}

// The upstream RENDER_TECH_COMPUTE_* names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RenderTech {
    ComputeTemporalAccumulation,
}

const RENDER_TECHS: [RenderTech; 1] = [RenderTech::ComputeTemporalAccumulation];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RenderTechniqueKey {
    render_tech: RenderTech,
    feature_flags: FeatureFlags,
    /// The context's `REVERSED_DEPTH`, for the closest-motion search
    /// (PROVENANCE.md DFX-13).
    reversed_depth: bool,
}

const ACCUMULATED_BUFFER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The closest motion vectors the resolve finds and keeps for the next frame
/// (PROVENANCE.md DFX-13, DFX-14), in the format of DiligentFX's
/// `PostFXContext`.
const CLOSEST_MOTION_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;

/// `TemporalAntiAliasing::AccumulationBufferInfo`.
struct AccumulationBufferInfo {
    /// `RESOURCE_ID_CONSTANT_BUFFER`.
    constant_buffer: Option<wgpu::Buffer>,
    /// `RESOURCE_ID_ACCUMULATED_BUFFER0`, `RESOURCE_ID_ACCUMULATED_BUFFER1`.
    accumulated_buffers: Option<[wgpu::TextureView; 2]>,
    /// Each accumulated buffer's frame's closest motion vectors, which the
    /// next frame reads (PROVENANCE.md DFX-14).
    closest_motion: Option<[wgpu::TextureView; 2]>,

    width: u32,
    height: u32,
    current_frame_idx: u32,
    last_frame_idx: u32,
    feature_flags: FeatureFlags,

    shader_attribs: TemporalAntiAliasingAttribs,
}

impl Default for AccumulationBufferInfo {
    fn default() -> Self {
        Self {
            constant_buffer: None,
            accumulated_buffers: None,
            closest_motion: None,
            width: 0,
            height: 0,
            current_frame_idx: 0,
            last_frame_idx: !0,
            feature_flags: FeatureFlags::NONE,
            shader_attribs: TemporalAntiAliasingAttribs::default(),
        }
    }
}

impl AccumulationBufferInfo {
    #[allow(clippy::too_many_arguments)]
    fn prepare(
        &mut self,
        post_fx_context: &PostFXContext,
        device: &wgpu::Device,
        ctx: &mut wgpu::CommandEncoder,
        width: u32,
        height: u32,
        curr_frame_idx: u32,
        feature_flags: FeatureFlags,
    ) {
        self.feature_flags = feature_flags;
        self.current_frame_idx = curr_frame_idx;

        if self.width == width && self.height == height {
            return;
        }

        self.width = width;
        self.height = height;

        if self.constant_buffer.is_none() {
            self.constant_buffer = Some(device.create_buffer_init(
                &wgpu::util::BufferInitDescriptor {
                    label: Some("TemporalAntiAliasing::ConstantBuffer"),
                    contents: bytemuck::bytes_of(&self.shader_attribs),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                },
            ));
        }

        let mut texture = |label, format| {
            let texture = device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    // BIND_SHADER_RESOURCE | BIND_RENDER_TARGET; COPY_SRC lets
                    // callers capture the result.
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
                .create_view(&Default::default());

            let clear_color = [0.0, 0.0, 0.0, 0.0];

            post_fx_context.clear_render_target(
                &mut TextureOperationAttribs {
                    device,
                    device_context: ctx,
                    timestamp_writes: None,
                },
                &texture,
                clear_color,
            );
            texture
        };
        self.accumulated_buffers = Some(std::array::from_fn(|_| {
            texture(
                "TemporalAntiAliasing::AccumulatedBuffer",
                ACCUMULATED_BUFFER_FORMAT,
            )
        }));
        self.closest_motion = Some(std::array::from_fn(|_| {
            texture("TemporalAntiAliasing::ClosestMotion", CLOSEST_MOTION_FORMAT)
        }));
    }

    fn update_constant_buffer(
        &mut self,
        queue: &wgpu::Queue,
        attribs: &TemporalAntiAliasingAttribs,
    ) {
        let reset_accumulation = self.last_frame_idx == !0 // No history on the first frame
            || self.current_frame_idx != self.last_frame_idx.wrapping_add(1) // Reset history if frames were skipped
            || attribs.reset_accumulation != 0; // Reset history if requested

        let update_required = reset_accumulation != (self.shader_attribs.reset_accumulation != 0)
            || bytemuck::bytes_of(&self.shader_attribs) != bytemuck::bytes_of(attribs);

        if update_required {
            self.shader_attribs = *attribs;
            self.shader_attribs.reset_accumulation = u32::from(reset_accumulation);
            queue.write_buffer(
                self.constant_buffer.as_ref().expect("prepared buffer"),
                0,
                bytemuck::bytes_of(&self.shader_attribs),
            );
        }

        self.last_frame_idx = self.current_frame_idx;
    }

    fn accumulated_buffer(&self, index: u32) -> &wgpu::TextureView {
        &self
            .accumulated_buffers
            .as_ref()
            .expect("TemporalAntiAliasing::PrepareResources")[index as usize]
    }

    fn closest_motion(&self, index: u32) -> &wgpu::TextureView {
        &self
            .closest_motion
            .as_ref()
            .expect("TemporalAntiAliasing::PrepareResources")[index as usize]
    }
}

/// Whether the context's depth is reversed, which the closest-motion search
/// follows (PROVENANCE.md DFX-13).
fn reversed_depth(post_fx_context: &PostFXContext) -> bool {
    post_fx_context
        .get_feature_flags()
        .contains(post_fx_context::FeatureFlags::REVERSED_DEPTH)
}

// https://en.wikipedia.org/wiki/Halton_sequence#Implementation_in_pseudocode
fn halton_sequence(base: u32, mut index: u32) -> f32 {
    let mut result = 0.0f32;
    let mut f = 1.0f32;
    while index > 0 {
        f /= base as f32;
        result += f * (index % base) as f32;
        index = (index as f32 / base as f32).floor() as u32;
    }
    result
}

pub struct TemporalAntiAliasing {
    render_tech: HashMap<RenderTechniqueKey, PostFXRenderTechnique>,
    accumulation_buffers: HashMap<u32, AccumulationBufferInfo>,

    all_psos_ready: bool,
    linear_clamp: wgpu::Sampler,
}

impl TemporalAntiAliasing {
    /// Creates a new instance of the effect.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            render_tech: HashMap::new(),
            accumulation_buffers: HashMap::new(),
            all_psos_ready: false,
            linear_clamp: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("Sam_LinearClamp"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            }),
        }
    }

    /// Returns the jitter offset for the specified accumulation buffer index:
    /// the NDC offset of the current frame's sample.
    pub fn get_jitter_offset(&self, accumulation_buffer_idx: u32) -> [f32; 2] {
        let Some(acc_buffer) = self.accumulation_buffers.get(&accumulation_buffer_idx) else {
            return [0.0, 0.0];
        };

        if acc_buffer.width == 0 || acc_buffer.height == 0 || !self.all_psos_ready {
            return [0.0, 0.0];
        }

        const SAMPLE_COUNT: u32 = 16;
        let jitter_x = (halton_sequence(2, (acc_buffer.current_frame_idx % SAMPLE_COUNT) + 1)
            - 0.5)
            / (0.5 * acc_buffer.width as f32);
        let jitter_y = (halton_sequence(3, (acc_buffer.current_frame_idx % SAMPLE_COUNT) + 1)
            - 0.5)
            / (0.5 * acc_buffer.height as f32);
        [jitter_x, jitter_y]
    }

    /// Prepares the effect for rendering.
    pub fn prepare_resources(
        &mut self,
        device: &wgpu::Device,
        device_context: &mut wgpu::CommandEncoder,
        post_fx_context: &PostFXContext,
        feature_flags: FeatureFlags,
        accumulation_buffer_idx: u32,
    ) {
        let frame_desc = *post_fx_context.get_frame_desc();
        self.accumulation_buffers
            .entry(accumulation_buffer_idx)
            .or_default()
            .prepare(
                post_fx_context,
                device,
                device_context,
                frame_desc.width,
                frame_desc.height,
                frame_desc.index,
                feature_flags,
            );

        // wgpu creates pipelines synchronously, so a created technique is
        // ready (PROVENANCE.md DFX-2).
        let reversed_depth = reversed_depth(post_fx_context);
        self.all_psos_ready = RENDER_TECHS.iter().all(|&render_tech| {
            self.render_tech
                .get(&RenderTechniqueKey {
                    render_tech,
                    feature_flags,
                    reversed_depth,
                })
                .is_some_and(PostFXRenderTechnique::is_initialized_pso)
        });
    }

    /// Executes the effect.
    pub fn execute(
        &mut self,
        render_attribs: &mut RenderAttributes<'_, '_>,
    ) -> PostFxExecutionStatus {
        let Some(acc_buffer) = self
            .accumulation_buffers
            .get(&render_attribs.accumulation_buffer_idx)
        else {
            panic!(
                "Accumulation buffer with index {} is not found, which indicates that PrepareResources() method was not called.",
                render_attribs.accumulation_buffer_idx
            );
        };
        let feature_flags = acc_buffer.feature_flags;
        self.prepare_shaders_and_pso(
            render_attribs.device,
            feature_flags,
            reversed_depth(render_attribs.post_fx_context),
        );

        let acc_buffer = self
            .accumulation_buffers
            .get_mut(&render_attribs.accumulation_buffer_idx)
            .unwrap();
        acc_buffer.update_constant_buffer(render_attribs.queue, render_attribs.taa_attribs);

        let all_psos_ready = self.all_psos_ready && render_attribs.post_fx_context.is_psos_ready();
        let acc_buffer = &self.accumulation_buffers[&render_attribs.accumulation_buffer_idx];
        if all_psos_ready {
            self.compute_temporal_accumulation(render_attribs, acc_buffer);
        } else {
            Self::compute_placeholder_texture(render_attribs, acc_buffer);
        }

        if all_psos_ready {
            PostFxExecutionStatus::Ready
        } else {
            PostFxExecutionStatus::Pending
        }
    }

    /// Returns the shader resource view of the accumulated frame.
    pub fn get_accumulated_frame_srv(
        &self,
        is_prev_frame: bool,
        accumulation_buffer_idx: u32,
    ) -> &wgpu::TextureView {
        let acc_buffer = self
            .accumulation_buffers
            .get(&accumulation_buffer_idx)
            .unwrap_or_else(|| {
                panic!("Accumulation buffer with index {accumulation_buffer_idx} is not found.")
            });

        let buff_idx = (acc_buffer.current_frame_idx + u32::from(is_prev_frame)) & 0x01;
        acc_buffer.accumulated_buffer(buff_idx)
    }

    /// Computes the jittered projection matrix. `proj` is a Diligent
    /// `float4x4` (row-major, row vectors): the `m_proj` layout of
    /// `CameraAttribs`.
    pub fn get_jittered_proj_matrix(mut proj: [f32; 16], jitter: [f32; 2]) -> [f32; 16] {
        const M20: usize = 2 * 4;
        const M21: usize = 2 * 4 + 1;
        const M30: usize = 3 * 4;
        const M31: usize = 3 * 4 + 1;
        const M33: usize = 3 * 4 + 3;
        if proj[M33] == 0.0 {
            // Perspective projection.
            // Make jitter proportional to z so that it is constant in screen space.
            proj[M20] += jitter[0];
            proj[M21] += jitter[1];
        } else {
            // Orthographic projection.
            // Apply offsets directly.
            proj[M30] += jitter[0];
            proj[M31] += jitter[1];
        }
        proj
    }

    fn prepare_shaders_and_pso(
        &mut self,
        device: &wgpu::Device,
        feature_flags: FeatureFlags,
        reversed_depth: bool,
    ) {
        let tech = self
            .render_tech
            .entry(RenderTechniqueKey {
                render_tech: RenderTech::ComputeTemporalAccumulation,
                feature_flags,
                reversed_depth,
            })
            .or_default();
        if !tech.is_initialized_pso() {
            let flag = |flag: FeatureFlags| {
                if feature_flags.contains(flag) {
                    "1"
                } else {
                    "0"
                }
            };
            let macros = [
                (
                    "TAA_OPTION_GAUSSIAN_WEIGHTING",
                    flag(FeatureFlags::GAUSSIAN_WEIGHTING),
                ),
                (
                    "TAA_OPTION_BICUBIC_FILTER",
                    flag(FeatureFlags::BICUBIC_FILTER),
                ),
                (
                    "TAA_OPTION_YCOCG_COLOR_SPACE",
                    flag(FeatureFlags::YCOCG_COLOR_SPACE),
                ),
                (
                    "POSTFX_OPTION_INVERTED_DEPTH",
                    if reversed_depth { "1" } else { "0" },
                ),
            ];

            let vs = create_shader(
                device,
                "FullScreenTriangleVS.fx",
                "FullScreenTriangleVS",
                &[],
            );

            let ps = create_shader(
                device,
                "TAA_ComputeTemporalAccumulation.fx",
                "ComputeTemporalAccumulationPS",
                &macros,
            );

            tech.initialize_pso(
                device,
                "TemporalAntiAliasing::ComputeTemporalAccumulation",
                &vs,
                &ps,
                &[
                    (0, Resource::ConstantBuffer),
                    (1, Resource::ConstantBuffer),
                    (2, Resource::Texture),
                    (3, Resource::FilterableTexture),
                    (4, Resource::Texture),
                    (5, Resource::Texture),
                    (6, Resource::Texture),
                    (7, Resource::Sampler { filtering: true }),
                    (8, Resource::Texture),
                    (9, Resource::DepthTexture),
                ],
                &[ACCUMULATED_BUFFER_FORMAT, CLOSEST_MOTION_FORMAT],
                None,
                DepthStencilStateDesc::DisableDepth,
            );
        }
    }

    fn compute_temporal_accumulation(
        &self,
        render_attribs: &mut RenderAttributes<'_, '_>,
        acc_buff: &AccumulationBufferInfo,
    ) {
        let ctx = &*render_attribs.post_fx_context;
        let tech = &self.render_tech[&RenderTechniqueKey {
            render_tech: RenderTech::ComputeTemporalAccumulation,
            feature_flags: acc_buff.feature_flags,
            reversed_depth: reversed_depth(ctx),
        }];

        let frame_index = ctx.get_frame_desc().index;
        let curr_buff_idx = frame_index & 0x01;
        let prev_buff_idx = frame_index.wrapping_add(1) & 0x01;
        let prev_buffer_srv = acc_buff.accumulated_buffer(prev_buff_idx);
        let curr_buffer_rtv = acc_buff.accumulated_buffer(curr_buff_idx);

        let texture = wgpu::BindingResource::TextureView;
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
                    resource: acc_buff
                        .constant_buffer
                        .as_ref()
                        .expect("prepared buffer")
                        .as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: texture(render_attribs.color_buffer_srv),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: texture(prev_buffer_srv),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: texture(render_attribs.motion_vectors_srv),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: texture(ctx.get_reprojected_depth()),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: texture(ctx.get_previous_depth()),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::Sampler(&self.linear_clamp),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: texture(acc_buff.closest_motion(prev_buff_idx)),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: texture(render_attribs.depth_buffer_srv),
                },
            ],
        );

        // PROVENANCE.md DFX-13: the resolve also writes this frame's closest
        // motion vectors, which the next frame reads (DFX-14).
        draw(
            render_attribs.device_context,
            "TemporalAccumulation",
            &[curr_buffer_rtv, acc_buff.closest_motion(curr_buff_idx)],
            tech,
            &group,
            0..3,
            render_attribs
                .pass_timestamps
                .and_then(|timestamps| timestamps("TemporalAccumulation")),
        );
    }

    fn compute_placeholder_texture(
        render_attribs: &mut RenderAttributes<'_, '_>,
        acc_buff: &AccumulationBufferInfo,
    ) {
        let buff_idx = acc_buff.current_frame_idx & 0x01;

        render_attribs.post_fx_context.copy_texture_color(
            &mut TextureOperationAttribs {
                device: render_attribs.device,
                device_context: render_attribs.device_context,
                timestamp_writes: render_attribs
                    .pass_timestamps
                    .and_then(|timestamps| timestamps("TemporalAccumulation")),
            },
            render_attribs.color_buffer_srv,
            acc_buff.accumulated_buffer(buff_idx),
            ACCUMULATED_BUFFER_FORMAT,
        );

        // PROVENANCE.md DFX-14: without the resolve, the next frame's previous
        // closest motion is this frame's motion, copied with
        // `CopyTextureColor`.
        render_attribs.post_fx_context.copy_texture_color(
            &mut TextureOperationAttribs {
                device: render_attribs.device,
                device_context: render_attribs.device_context,
                timestamp_writes: render_attribs
                    .pass_timestamps
                    .and_then(|timestamps| timestamps("TemporalAccumulation")),
            },
            render_attribs.motion_vectors_srv,
            acc_buff.closest_motion(buff_idx),
            CLOSEST_MOTION_FORMAT,
        );
    }
}
