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
    DepthStencilStateDesc, InPlace, Pipelines, PostFXRenderTechnique, Resource, create_shader,
};
use crate::screen_space_reflection::PostFxExecutionStatus;
use crate::structures::{CameraAttribs, TemporalAntiAliasingAttribs};
use std::collections::HashMap;
use std::sync::Arc;
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
    /// Filament-derived partial rectification and adaptive feedback at rest,
    /// with camera/local-motion holds and jitter-aware depth disocclusion.
    /// Feedback is `1 - TemporalStabilityFactor`; SGL3D uses 0.12, paired
    /// with the reference's twelve-frame hold. Retains an RGBA16F motion
    /// history instead of RG16F; default/legacy flags are unchanged.
    /// Requires depth motion in the motion vectors' Z and W
    /// ([`RenderAttributes::motion_vectors_srv`]), so the depth test follows
    /// surfaces that move in depth.
    pub const MOTION_ADAPTIVE_HISTORY: Self = Self(1 << 3);

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
    /// Shader resource view of the source color, in any float format: it is
    /// read with loads, so it need not be filterable.
    pub color_buffer_srv: &'a wgpu::TextureView,
    /// Shader resource view of the depth buffer, the context's
    /// `curr_depth_buffer_srv`, in which the closest motion vectors are
    /// found (PROVENANCE.md DFX-13).
    pub depth_buffer_srv: &'a wgpu::TextureView,
    /// Shader resource view of the motion vectors, in a filterable float
    /// format: NDC motion (current − previous) in XY. Under
    /// [`FeatureFlags::MOTION_ADAPTIVE_HISTORY`] it must have four channels
    /// (such as `Rgba16Float`): Z holds the surface's previous minus
    /// current camera-space Z (the cameras' view units, positive in front),
    /// motion of the surface and the camera included, and W its validity
    /// (above 0 valid, 0 where there is none, such as the sky); `execute`
    /// panics on fewer channels. Other flags read XY only.
    pub motion_vectors_srv: &'a wgpu::TextureView,
    /// Up to two responsive masks: single-channel views at the input size
    /// holding in their first channel, in `0..=1`, how much of a pixel's
    /// content changes without a motion vector of its own (blended
    /// surfaces, effects, animated shading). Read only under
    /// [`FeatureFlags::MOTION_ADAPTIVE_HISTORY`]: the resolve takes the
    /// largest value of either over the pixel's 3x3 neighbourhood, as FSR
    /// dilates them, and, while a pixel is so marked, rectifies and feeds
    /// back toward full strength in proportion to it; it arms a hold of its
    /// own, at full strength once no pixel of the neighbourhood is marked.
    /// Empty for none; `execute` panics on more than two.
    pub responsive_masks: &'a [&'a wgpu::TextureView],
    /// The motion within reach of each pixel, read only under
    /// [`FeatureFlags::MOTION_ADAPTIVE_HISTORY`]; `None` for none.
    pub neighborhood_motion: Option<NeighborhoodMotion<'a>>,
    /// TAA settings.
    pub taa_attribs: &'a TemporalAntiAliasingAttribs,
    /// Accumulation buffer index.
    pub accumulation_buffer_idx: u32,
    /// Per-pass timestamps (the upstream debug groups), when profiling.
    pub pass_timestamps: Option<&'a post_fx_context::PassTimestamps<'p>>,
}

/// The largest motion within reach of each pixel: where a surface that moves
/// could have been a frame ago, so a pixel it has just left is still
/// resolved as moving. Where it is at least the motion threshold (a pixel a
/// frame, the reach of the resolve's own closest-motion search) it arms the
/// recovery hold; slower, it changes nothing. It is never used for
/// reprojection or the depth test.
#[derive(Clone, Copy, Debug)]
pub struct NeighborhoodMotion<'a> {
    /// A view in a float format, at least the input size over `tile_size`
    /// rounded up: texel `(x / tile_size, y / tile_size)` holds in its first
    /// two channels a motion, in pixels of the input a frame, at least as
    /// long as any that reaches the pixel `(x, y)`. Only its length is read.
    pub srv: &'a wgpu::TextureView,
    /// The input pixels along each side of a texel's tile; positive.
    pub tile_size: u32,
}

// The upstream RENDER_TECH_COMPUTE_* names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RenderTech {
    ComputeTemporalAccumulation,
    /// The output while the effect is pending (PROVENANCE.md DFX-2,
    /// DFX-47), created with the resolve, so readiness does not wait on it.
    ComputePlaceholder,
}

const RENDER_TECHS: [RenderTech; 1] = [RenderTech::ComputeTemporalAccumulation];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RenderTechniqueKey {
    render_tech: RenderTech,
    feature_flags: FeatureFlags,
    /// The context's `REVERSED_DEPTH`, for the closest-motion search
    /// (PROVENANCE.md DFX-13).
    reversed_depth: bool,
    /// The responsive inputs the resolve binds.
    responsive: Responsive,
}

/// The responsive inputs a resolve binds: how many masks, and the
/// neighbourhood motion's tile, 0 for none. None outside
/// `MOTION_ADAPTIVE_HISTORY`, which alone reads them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct Responsive {
    masks: u32,
    neighborhood_tile: u32,
}

impl Responsive {
    fn new(flags: FeatureFlags, masks: usize, neighborhood_tile: Option<u32>) -> Self {
        if !flags.contains(FeatureFlags::MOTION_ADAPTIVE_HISTORY) {
            return Self::default();
        }
        Self {
            masks: masks as u32,
            neighborhood_tile: neighborhood_tile.unwrap_or(0),
        }
    }

    fn of(attribs: &RenderAttributes<'_, '_>, flags: FeatureFlags) -> Self {
        Self::new(
            flags,
            attribs.responsive_masks.len(),
            attribs.neighborhood_motion.map(|n| n.tile_size),
        )
    }
}

const ACCUMULATED_BUFFER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The closest motion vectors the resolve finds and keeps for the next frame
/// (PROVENANCE.md DFX-13, DFX-14), in the format of DiligentFX's
/// `PostFXContext`.
fn closest_motion_format(flags: FeatureFlags) -> wgpu::TextureFormat {
    if flags.contains(FeatureFlags::MOTION_ADAPTIVE_HISTORY) {
        wgpu::TextureFormat::Rgba16Float
    } else {
        wgpu::TextureFormat::Rg16Float
    }
}

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
    last_frame_idx: Option<u32>,
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
            last_frame_idx: None,
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
        let motion_format_changed =
            closest_motion_format(self.feature_flags) != closest_motion_format(feature_flags);
        self.feature_flags = feature_flags;
        self.current_frame_idx = curr_frame_idx;

        if self.width == width && self.height == height && !motion_format_changed {
            return;
        }

        self.width = width;
        self.height = height;
        // DFX-53: the buffers created below hold no history, so the next
        // frame is a reset, as the first is.
        self.last_frame_idx = None;

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
            texture(
                "TemporalAntiAliasing::ClosestMotion",
                closest_motion_format(feature_flags),
            )
        }));
    }

    fn update_constant_buffer(
        &mut self,
        queue: &wgpu::Queue,
        attribs: &TemporalAntiAliasingAttribs,
    ) {
        // No history on the first frame, after the buffers' recreation, nor
        // if frames were skipped (PROVENANCE.md DFX-50, DFX-53). A repeated
        // index repeats that frame, its reset included: it reads the same
        // history and writes the same buffer (DFX-12). `shader_attribs`
        // holds the last execution's reset.
        let reset_accumulation = match self.last_frame_idx {
            Some(last) if self.current_frame_idx == last => {
                self.shader_attribs.reset_accumulation != 0
            }
            Some(last) => self.current_frame_idx != last.wrapping_add(1),
            None => true,
        } || attribs.reset_accumulation != 0; // Reset history if requested

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

        self.last_frame_idx = Some(self.current_frame_idx);
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

/// The rounding steps by which a camera's matrices may differ and the
/// camera still be at rest (`TemporalAntiAliasing::camera_moved`;
/// PROVENANCE.md, "Motion-adaptive TAA"), each step taken at the magnitude
/// the element's rounding scales with (at least 1). A rotation or
/// projection element is computed from terms of its own size, so its noise
/// is a few of its own steps. A view translation element is
/// `-dot(axis, eye)`, a sum of terms of the eye's magnitude that may cancel
/// to near zero: its noise is a few steps of the camera's distance from the
/// render origin whatever its own size (7.6e-6 at 100 m on an element of
/// 0.024 in Hyperdrive's chase camera), so it is judged at that distance.
/// A view recomputed each frame from float state accumulates a few
/// half-steps per term; eight covers that. A larger tolerance is safe
/// because the view-wide camera signal only has to catch real motion: a
/// camera change that moves any pixel by 0.01 px or more still arms that
/// pixel through its own motion vector (the still-pixel threshold, DFX-19),
/// and real camera motion is orders of magnitude above eight steps.
const CAMERA_REST_STEPS: f32 = 8.;

/// The largest absolute component of the eye position of `view` (columns,
/// rotation and translation): `-Rᵀ t`.
fn eye_magnitude(view: &[f32; 16]) -> f32 {
    (0..3)
        .map(|axis| {
            (0..3)
                .map(|row| view[axis * 4 + row] * view[12 + row])
                .sum::<f32>()
                .abs()
        })
        .fold(0., f32::max)
}

/// `proj` (columns) without `jitter`: the inverse of a jitter applied as a
/// clip-space translation, `T(jitter) * P`, which offsets every column's x
/// and y by the jitter times its w (perspective and orthographic alike).
fn unjittered_projection(mut proj: [f32; 16], jitter: [f32; 2]) -> [f32; 16] {
    for column in proj.chunks_exact_mut(4) {
        column[0] -= jitter[0] * column[3];
        column[1] -= jitter[1] * column[3];
    }
    proj
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
    /// Who creates the techniques' pipelines.
    pipelines: Pipelines,
    accumulation_buffers: HashMap<u32, AccumulationBufferInfo>,

    all_psos_ready: bool,
    linear_clamp: wgpu::Sampler,
}

impl TemporalAntiAliasing {
    /// Creates a new instance of the effect.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            render_tech: HashMap::new(),
            pipelines: Arc::new(InPlace),
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

    /// Gives the effect's techniques another pipeline compiler than
    /// `InPlace` (`render_technique::PipelineCompiler`), before its first
    /// `execute`.
    pub fn set_pipeline_compiler(&mut self, pipelines: Pipelines) {
        self.pipelines = pipelines;
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

        // A technique is ready once its pipeline compiler has given it its
        // pipeline: at once with the default, in place (PROVENANCE.md
        // DFX-2). The responsive inputs are not known before `execute`,
        // whose variant is asked for with the rest: any of these flags'
        // techniques that is ready counts.
        let reversed_depth = reversed_depth(post_fx_context);
        self.all_psos_ready = RENDER_TECHS.iter().all(|&render_tech| {
            self.render_tech.iter().any(|(key, tech)| {
                key.render_tech == render_tech
                    && key.feature_flags == feature_flags
                    && key.reversed_depth == reversed_depth
                    && tech.is_initialized_pso()
            })
        });
    }

    /// Creates the resolve that binds `responsive_masks` masks (at most two)
    /// and, with `neighborhood_tile`, a neighbourhood motion in tiles of
    /// that size, so the first frame that binds them does not create it
    /// while recording. `execute` creates any resolve it lacks.
    pub fn prepare_responsive_variant(
        &mut self,
        device: &wgpu::Device,
        post_fx_context: &PostFXContext,
        feature_flags: FeatureFlags,
        responsive_masks: usize,
        neighborhood_tile: Option<u32>,
    ) {
        assert!(
            responsive_masks <= 2,
            "TAA reads at most two responsive masks"
        );
        self.prepare_shaders_and_pso(
            device,
            feature_flags,
            reversed_depth(post_fx_context),
            Responsive::new(feature_flags, responsive_masks, neighborhood_tile),
        );
    }

    /// Executes the effect.
    ///
    /// # Panics
    ///
    /// If `prepare_resources` was not called for the accumulation buffer, if
    /// `MOTION_ADAPTIVE_HISTORY` is set and the motion vectors have fewer
    /// than four channels, if more than two responsive masks are given, or
    /// if the neighbourhood motion's tile is 0 or its view too small.
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
        // A two-channel view reads as Z = 0, W = 1 in WGSL: valid, unmoving
        // depth, which would be wrong rather than absent.
        let motion_format = render_attribs.motion_vectors_srv.texture().format();
        assert!(
            !feature_flags.contains(FeatureFlags::MOTION_ADAPTIVE_HISTORY)
                || motion_format.components() == 4,
            "MOTION_ADAPTIVE_HISTORY reads depth motion from the motion vectors' Z and W, \
             which {motion_format:?} lacks"
        );
        assert!(
            render_attribs.responsive_masks.len() <= 2,
            "TAA reads at most two responsive masks, not {}",
            render_attribs.responsive_masks.len()
        );
        if let Some(neighborhood) = render_attribs.neighborhood_motion {
            let tile = neighborhood.tile_size;
            let size = neighborhood.srv.texture().size();
            assert!(
                tile > 0
                    && size.width >= acc_buffer.width.div_ceil(tile)
                    && size.height >= acc_buffer.height.div_ceil(tile),
                "the neighbourhood motion, {}x{} in tiles of {tile}, does not cover {}x{}",
                size.width,
                size.height,
                acc_buffer.width,
                acc_buffer.height
            );
        }
        let responsive = Responsive::of(render_attribs, feature_flags);
        // The frame's own resolve, whose inputs `prepare_resources` did not
        // know: it draws only once its pipeline is there too (PROVENANCE
        // DFX-2), which with the default compiler it is from here on.
        let resolve_ready = self.prepare_shaders_and_pso(
            render_attribs.device,
            feature_flags,
            reversed_depth(render_attribs.post_fx_context),
            responsive,
        );

        let acc_buffer = self
            .accumulation_buffers
            .get_mut(&render_attribs.accumulation_buffer_idx)
            .unwrap();
        acc_buffer.update_constant_buffer(render_attribs.queue, render_attribs.taa_attribs);

        let all_psos_ready =
            self.all_psos_ready && resolve_ready && render_attribs.post_fx_context.is_psos_ready();
        let acc_buffer = &self.accumulation_buffers[&render_attribs.accumulation_buffer_idx];
        if all_psos_ready {
            self.compute_temporal_accumulation(render_attribs, acc_buffer, responsive);
        } else {
            self.compute_placeholder_texture(render_attribs, acc_buffer, responsive);
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

        let buff_idx = acc_buffer
            .current_frame_idx
            .wrapping_add(u32::from(is_prev_frame))
            & 0x01;
        acc_buffer.accumulated_buffer(buff_idx)
    }

    /// Whether the camera moved between `previous` and `current`, the
    /// cameras the context was given: [`TemporalAntiAliasingAttribs::camera_motion`].
    /// Each camera's jitter (`f2_jitter`) is removed from its projection
    /// first, so jitter is never motion, and the unjittered projections and
    /// the views are compared element by element: elements `a` and `b`
    /// agree when `|a - b| <= CAMERA_REST_STEPS * f32::EPSILON * max(1, m)`,
    /// where `m` is the larger of `|a|` and `|b|`, and for the view's
    /// translation also the eye's distance from the render origin (its
    /// largest absolute component, of either camera), whose rounding a
    /// translation element carries however small it is. A camera whose
    /// matrices are recomputed each frame from unchanged state, anywhere
    /// from the render origin, is at rest; any change larger than that
    /// rounding has moved it.
    pub fn camera_moved(current: &CameraAttribs, previous: &CameraAttribs) -> bool {
        let projection =
            |camera: &CameraAttribs| unjittered_projection(camera.m_proj, camera.f2_jitter);
        let eye = eye_magnitude(&current.m_view).max(eye_magnitude(&previous.m_view));
        let agree = |a: &[f32; 16], b: &[f32; 16], translation: f32| {
            a.iter().zip(b).enumerate().all(|(i, (&a, &b))| {
                let scale = if (12..15).contains(&i) {
                    translation
                } else {
                    0.
                };
                (a - b).abs()
                    <= CAMERA_REST_STEPS * f32::EPSILON * 1f32.max(a.abs()).max(b.abs()).max(scale)
            })
        };
        !(agree(&current.m_view, &previous.m_view, eye)
            && agree(&projection(current), &projection(previous), 0.))
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
        responsive: Responsive,
    ) -> bool {
        let key = |render_tech| RenderTechniqueKey {
            render_tech,
            feature_flags,
            reversed_depth,
            responsive,
        };
        let pipelines = self.pipelines.clone();
        // Both are initialized until both have their pipelines.
        let ready = |techniques: &HashMap<RenderTechniqueKey, PostFXRenderTechnique>, tech| {
            techniques
                .get(&key(tech))
                .is_some_and(PostFXRenderTechnique::is_initialized_pso)
        };
        let both = ready(&self.render_tech, RenderTech::ComputeTemporalAccumulation)
            && ready(&self.render_tech, RenderTech::ComputePlaceholder);
        let tech = self
            .render_tech
            .entry(key(RenderTech::ComputeTemporalAccumulation))
            .or_default();
        if !both {
            let responsive_masks = responsive.masks.to_string();
            let neighborhood_tile = responsive.neighborhood_tile.to_string();
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
                    "TAA_OPTION_MOTION_ADAPTIVE_HISTORY",
                    flag(FeatureFlags::MOTION_ADAPTIVE_HISTORY),
                ),
                (
                    "POSTFX_OPTION_INVERTED_DEPTH",
                    if reversed_depth { "1" } else { "0" },
                ),
                ("TAA_OPTION_RESPONSIVE_MASKS", &responsive_masks),
                ("TAA_OPTION_NEIGHBORHOOD_MOTION_TILE", &neighborhood_tile),
            ];

            let vs = create_shader("FullScreenTriangleVS.fx", "FullScreenTriangleVS", &[]);

            let ps = create_shader(
                "TAA_ComputeTemporalAccumulation.fx",
                "ComputeTemporalAccumulationPS",
                &macros,
            );

            tech.initialize_pso(
                &*pipelines,
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
                ]
                .into_iter()
                .chain((0..responsive.masks).map(|mask| (10 + mask, Resource::Texture)))
                .chain((responsive.neighborhood_tile > 0).then_some((12, Resource::Texture)))
                .collect::<Vec<_>>(),
                &[
                    ACCUMULATED_BUFFER_FORMAT,
                    closest_motion_format(feature_flags),
                ],
                None,
                DepthStencilStateDesc::DisableDepth,
            );

            // PROVENANCE.md DFX-47: the placeholder reads the colour through
            // the resolve's unfilterable binding.
            let placeholder_ps = ps.with_entry_point("ComputePlaceholderPS");
            self.render_tech
                .entry(key(RenderTech::ComputePlaceholder))
                .or_default()
                .initialize_pso(
                    &*pipelines,
                    device,
                    "TemporalAntiAliasing::ComputePlaceholder",
                    &vs,
                    &placeholder_ps,
                    &[(2, Resource::Texture), (4, Resource::Texture)],
                    &[
                        ACCUMULATED_BUFFER_FORMAT,
                        closest_motion_format(feature_flags),
                    ],
                    None,
                    DepthStencilStateDesc::DisableDepth,
                );
        }
        ready(&self.render_tech, RenderTech::ComputeTemporalAccumulation)
    }

    fn compute_temporal_accumulation(
        &self,
        render_attribs: &mut RenderAttributes<'_, '_>,
        acc_buff: &AccumulationBufferInfo,
        responsive: Responsive,
    ) {
        let ctx = &*render_attribs.post_fx_context;
        let tech = &self.render_tech[&RenderTechniqueKey {
            render_tech: RenderTech::ComputeTemporalAccumulation,
            feature_flags: acc_buff.feature_flags,
            reversed_depth: reversed_depth(ctx),
            responsive,
        }];

        let frame_index = ctx.get_frame_desc().index;
        let curr_buff_idx = frame_index & 0x01;
        let prev_buff_idx = frame_index.wrapping_add(1) & 0x01;
        let prev_buffer_srv = acc_buff.accumulated_buffer(prev_buff_idx);
        let curr_buffer_rtv = acc_buff.accumulated_buffer(curr_buff_idx);

        let texture = wgpu::BindingResource::TextureView;
        let mut responsive_entries: Vec<wgpu::BindGroupEntry<'_>> = render_attribs
            .responsive_masks
            .iter()
            .take(responsive.masks as usize)
            .zip(10..)
            .map(|(&mask, binding)| wgpu::BindGroupEntry {
                binding,
                resource: texture(mask),
            })
            .collect();
        if responsive.neighborhood_tile > 0
            && let Some(neighborhood) = render_attribs.neighborhood_motion
        {
            responsive_entries.push(wgpu::BindGroupEntry {
                binding: 12,
                resource: texture(neighborhood.srv),
            });
        }
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
            ]
            .into_iter()
            .chain(responsive_entries)
            .collect::<Vec<_>>(),
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
        &self,
        render_attribs: &mut RenderAttributes<'_, '_>,
        acc_buff: &AccumulationBufferInfo,
        responsive: Responsive,
    ) {
        let buff_idx = acc_buff.current_frame_idx & 0x01;

        // PROVENANCE.md DFX-47: upstream copies the colour with
        // `CopyTextureColor`; the placeholder loads it as the resolve does.
        let tech = &self.render_tech[&RenderTechniqueKey {
            render_tech: RenderTech::ComputePlaceholder,
            feature_flags: acc_buff.feature_flags,
            reversed_depth: reversed_depth(render_attribs.post_fx_context),
            responsive,
        }];
        // Its own pipeline may still be compiling: nothing is drawn then.
        if !tech.is_initialized_pso() {
            return;
        }
        let group = tech.bind_group(
            render_attribs.device,
            "ComputePlaceholder",
            &[
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(render_attribs.color_buffer_srv),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(render_attribs.motion_vectors_srv),
                },
            ],
        );
        draw(
            render_attribs.device_context,
            "ComputePlaceholder",
            &[
                acc_buff.accumulated_buffer(buff_idx),
                acc_buff.closest_motion(buff_idx),
            ],
            tech,
            &group,
            0..3,
            render_attribs
                .pass_timestamps
                .and_then(|timestamps| timestamps("TemporalAccumulation")),
        );

        // The placeholder owns both MRT outputs: arbitrary input B/A must
        // never become an adaptive recovery counter.
    }
}

#[cfg(test)]
mod camera_tests {
    use super::{CameraAttribs, TemporalAntiAliasing};

    /// The vertical field of view and rows the smallest changes are judged at:
    /// a 1080p view one radian high.
    const FOV_Y: f32 = 1.;
    const ROWS: f32 = 1080.;

    /// A left-handed perspective of `fov_y` (columns), near 0.1, far 100.
    fn projection(fov_y: f32) -> [f32; 16] {
        let (near, far) = (0.1f32, 100f32);
        let y = 1. / (fov_y * 0.5).tan();
        let x = y * ROWS / 1920.;
        let m22 = far / (far - near);
        [
            x,
            0.,
            0.,
            0., //
            0.,
            y,
            0.,
            0., //
            0.,
            0.,
            m22,
            1., //
            0.,
            0.,
            -near * m22,
            0.,
        ]
    }

    /// The view (columns) of a camera at `position` turned `yaw` radians
    /// about Y: the rotation's transpose, and its translation `-Rᵀ p`.
    fn view(position: [f32; 3], yaw: f32) -> [f32; 16] {
        let (s, c) = yaw.sin_cos();
        let [x, y, z] = position;
        [
            c,
            0.,
            s,
            0., //
            0.,
            1.,
            0.,
            0., //
            -s,
            0.,
            c,
            0., //
            -(c * x - s * z),
            -y,
            -(s * x + c * z),
            1.,
        ]
    }

    /// A camera of `view` and `projection`, the projection jittered by
    /// `jitter` (NDC) as a clip-space translation.
    fn camera(view: [f32; 16], mut projection: [f32; 16], jitter: [f32; 2]) -> CameraAttribs {
        for column in projection.chunks_exact_mut(4) {
            column[0] += jitter[0] * column[3];
            column[1] += jitter[1] * column[3];
        }
        CameraAttribs {
            f2_jitter: jitter,
            m_view: view,
            m_proj: projection,
            ..Default::default()
        }
    }

    /// `value` `steps` f32 rounding steps up (or down, if negative).
    fn stepped(mut value: f32, steps: i32) -> f32 {
        for _ in 0..steps.unsigned_abs() {
            value = if steps > 0 {
                value.next_up()
            } else {
                value.next_down()
            };
        }
        value
    }

    /// The camera positions the cases run at: near the render origin, about
    /// 100 m from it and several kilometres from it.
    const POSITIONS: [[f32; 3]; 3] = [
        [0.3, 1.2, -0.7],
        [83.1, 21.7, -54.9],
        [3170.4, 412.9, -4021.6],
    ];

    // Plausible defect: an absolute threshold (the 1e-6 TAA used before)
    // takes a camera at rest away from the render origin as moving, because
    // a view element of magnitude 16 or more has a rounding step above it.
    // The oracle is the requirement that a camera at rest is at rest
    // wherever it is: the same view re-submitted with one to three rounding
    // steps of noise on every element, at each distance, with a different
    // jitter each frame (jitter is not motion).
    #[test]
    fn a_camera_at_rest_with_rounding_noise_has_not_moved() {
        let projection = projection(FOV_Y);
        for position in POSITIONS {
            let rest = view(position, 0.7);
            let previous = camera(rest, projection, [0.3 / 1920., -0.1 / 1080.]);
            for (frame, steps) in [1, -2, 3, -3, 2, -1].into_iter().enumerate() {
                let mut noisy = rest;
                for (i, element) in noisy.iter_mut().enumerate() {
                    // Constant elements (0, 1) are rebuilt exactly; the
                    // rotation and translation carry the rounding.
                    if *element != 0. && *element != 1. {
                        *element = stepped(*element, if i % 2 == 0 { steps } else { -steps });
                    }
                }
                let current = camera(noisy, projection, [-0.2 / 1920., 0.4 / 1080.]);
                assert!(
                    !TemporalAntiAliasing::camera_moved(&current, &previous),
                    "a camera at rest at {position:?} with {steps} rounding steps of noise (frame {frame}) moved"
                );
            }
        }
    }

    // Plausible defect: a tolerance so wide, or a comparison that skips the
    // view or the projection, that a real camera change is taken for rest.
    // The changes, each the smallest of its kind that must count: a 1 mm
    // translation at the render origin, a rotation by a hundredth of a
    // pixel's angle at the view's centre, and a field of view widened enough
    // to move the edge rows by a hundredth of a pixel, at every distance.
    #[test]
    fn the_smallest_real_camera_changes_move_it() {
        let pixel_angle = (2. * (FOV_Y * 0.5).tan() / ROWS).atan();
        // The edge row lies `ROWS / 2` pixels from the centre, so a scale of
        // 1 + 0.01 / (ROWS / 2) moves it a hundredth of a pixel.
        let scale = 1. + 0.01 / (ROWS * 0.5);
        let wider = 2. * ((FOV_Y * 0.5).tan() * scale).atan();
        let projection = projection(FOV_Y);
        let [x, y, z] = POSITIONS[0];
        let translated = camera(view([x + 0.001, y, z], 0.7), projection, [0.; 2]);
        let origin = camera(view(POSITIONS[0], 0.7), projection, [0.; 2]);
        assert!(
            TemporalAntiAliasing::camera_moved(&translated, &origin),
            "a 1 mm translation at the render origin did not move the camera"
        );
        for position in POSITIONS {
            let rest = camera(view(position, 0.7), projection, [0.; 2]);
            let turned = camera(
                view(position, 0.7 + 0.01 * pixel_angle),
                projection,
                [0.; 2],
            );
            let widened = camera(view(position, 0.7), self::projection(wider), [0.; 2]);
            for (what, current) in [("a turn", turned), ("a wider field", widened)] {
                assert!(
                    TemporalAntiAliasing::camera_moved(&current, &rest),
                    "{what} moving pixels by a hundredth at {position:?} did not move the camera"
                );
            }
        }
    }

    // The tolerance of a view translation element is 8 rounding steps of the
    // larger of its own magnitude and the eye's largest component, so far
    // from the render origin it admits a small translation: here the
    // farthest camera's largest translation element, 5015 m, gives
    // 8 * 2^-23 * 5015 m = 4.8 mm. That is at rest for the view-wide signal
    // only: a surface the translation moves by 0.01 px or more (one within
    // about 475 m at this view, whose pixel spans 1.01 mrad) arms its own
    // pixels through its motion vector, and a farther one moved less than
    // that. The oracle is that bound: 1 mm (0.77 mm along that element) is
    // within it, 1 cm (7.7 mm) beyond it.
    #[test]
    fn far_from_the_origin_a_translation_within_the_rounding_bound_is_left_to_the_pixels() {
        let projection = projection(FOV_Y);
        let position = POSITIONS[2];
        let rest = camera(view(position, 0.7), projection, [0.; 2]);
        let at = |offset: f32| {
            let [x, y, z] = position;
            camera(view([x + offset, y, z], 0.7), projection, [0.; 2])
        };
        assert!(
            !TemporalAntiAliasing::camera_moved(&at(0.001), &rest),
            "1 mm at 5 km is within the 4.8 mm the rounding tolerance admits there"
        );
        assert!(
            TemporalAntiAliasing::camera_moved(&at(0.01), &rest),
            "1 cm at 5 km is beyond the 4.8 mm the rounding tolerance admits there"
        );
    }

    /// The yaw of the cameras below, and its view's right axis and forward
    /// axis in the world: `view`'s first row and its third row negated.
    const YAW: f32 = 0.7;
    fn right() -> [f32; 3] {
        [YAW.cos(), 0., -YAW.sin()]
    }
    fn forward() -> [f32; 3] {
        [-YAW.sin(), 0., -YAW.cos()]
    }

    /// `distance` along `axis`, 1.7 m up.
    fn eye_along(axis: [f32; 3], distance: f32) -> [f32; 3] {
        [axis[0] * distance, 1.7, axis[2] * distance]
    }

    // Plausible defect: a view translation element judged by its own
    // magnitude. It is `-dot(axis, eye)`, a sum of terms of the eye's size
    // that cancels to near zero when the eye lies along another axis, so
    // a view rebuilt each frame from the same eye position carrying one to
    // three of its own rounding steps (as a game's chase camera does) moves
    // it by steps of the eye's magnitude: 7.6e-6 at 100 m, on an element
    // near 0. The oracle is the requirement that such a camera is at rest:
    // eyes along the forward axis (the right-axis translation cancels) and
    // along the right axis (the forward one does), at 100 m and 5 km.
    #[test]
    fn a_view_rebuilt_from_a_rounded_eye_has_not_moved() {
        let projection = projection(FOV_Y);
        for (axis, name) in [(forward(), "forward"), (right(), "right")] {
            for distance in [100., 5000.] {
                let eye = eye_along(axis, distance);
                let rest = camera(view(eye, YAW), projection, [0.; 2]);
                for steps in [1, -2, 3, -3, 2, -1] {
                    let noisy = [
                        stepped(eye[0], steps),
                        stepped(eye[1], -steps),
                        stepped(eye[2], steps),
                    ];
                    let current = camera(view(noisy, YAW), projection, [0.; 2]);
                    assert!(
                        !TemporalAntiAliasing::camera_moved(&current, &rest),
                        "an eye {distance} m along the {name} axis, {steps} rounding steps off, moved"
                    );
                }
            }
        }
    }

    // At 100 m the eye's rounding admits a translation of 8 steps of its
    // largest component: with the eye 100 m along the forward axis that is
    // 76.5 m, so 8 * 2^-23 * 76.5 m = 73 µm along the right axis. A
    // translation that small moves only surfaces within about 7 m by 0.01
    // px or more at this view, and those arm their own pixels. The oracle is
    // that bound: 0.05 mm is within it, 1 mm beyond it.
    #[test]
    fn at_100_m_a_translation_beyond_the_eyes_rounding_moves_the_camera() {
        let projection = projection(FOV_Y);
        let eye = eye_along(forward(), 100.);
        let rest = camera(view(eye, YAW), projection, [0.; 2]);
        let at = |offset: f32| {
            let [x, y, z] = eye;
            camera(view([x + offset, y, z], YAW), projection, [0.; 2])
        };
        assert!(
            !TemporalAntiAliasing::camera_moved(&at(0.00005), &rest),
            "0.05 mm at 100 m is within the 73 µm the eye's rounding admits there"
        );
        assert!(
            TemporalAntiAliasing::camera_moved(&at(0.001), &rest),
            "1 mm at 100 m is beyond the 73 µm the eye's rounding admits there"
        );
    }
}
