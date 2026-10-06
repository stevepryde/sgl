//! SGL post effects (`sgl-post-fx`) in SGL3D: the one place
//! SGL3D-specific adaptation of the effects library exists. The renderer owns the
//! context and lends it to Crystal SSR and TAA.
//!
//! Per frame, as in Diligent's Hydrogent renderer (`HnPostProcessTask`):
//! `prepare` before any geometry decides whether history continues and, with
//! TAA, the frame's jitter; after the G-buffer and the receivers, `begin`
//! converts SGL3D's surface (the receivers' over the G-buffer's) and camera
//! into DiligentFX's inputs in one fullscreen pass and runs
//! `PostFXContext::Execute` once for every effect; SSR and TAA then read that
//! context.
use super::history::CameraFrame;
use super::targets::{SharedTargets, Surface};
use crate::shading;
use glam::{Mat4, Vec4};
use sgl_post_fx::post_fx_context::{self, FrameDesc, PostFXContext};
use sgl_post_fx::screen_space_reflection::{self, FeatureFlags, ScreenSpaceReflection};
use sgl_post_fx::temporal_anti_aliasing::{self, TemporalAntiAliasing};
use sgl_post_fx::{CameraAttribs, ScreenSpaceReflectionAttribs, TemporalAntiAliasingAttribs};

/// Distance in metres of the effect camera's far plane. DiligentFX's camera
/// (`CameraAttribs::SetClipPlanes`) has a finite far plane where SGL3D's
/// projection is infinite; surfaces beyond it are background to the effects
/// (for SSR neither traced nor reflected). DiligentFX stores ray vectors in
/// RGBA16F, so it must stay well below 65 504 m.
pub(crate) const FAR_PLANE: f32 = 10_000.;

/// DiligentFX's inputs converted from SGL3D's G-buffer, at one render size.
struct Inputs {
    size: [u32; 2],
    normal: wgpu::TextureView,
    material: wgpu::TextureView,
    motion: wgpu::TextureView,
    /// Depth under the effect camera's finite-far projection, alternating by
    /// frame index as Hydrogent swaps its two depth buffers: the frame's is
    /// `pCurrDepthBufferSRV` and `pDepthBufferSRV`, the other the previous
    /// frame's, `pPrevDepthBufferSRV`.
    depth: [wgpu::TextureView; 2],
}

impl Inputs {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let texture = |label, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | usage,
                view_formats: &[],
            })
        };
        let target = |label, format| {
            texture(label, format, wgpu::TextureUsages::RENDER_ATTACHMENT)
                .create_view(&Default::default())
        };
        Self {
            size,
            normal: target("DiligentFX input normal", NORMAL_FORMAT),
            material: target("DiligentFX input material parameters", MATERIAL_FORMAT),
            motion: target("DiligentFX input motion vectors", MOTION_FORMAT),
            depth: std::array::from_fn(|_| {
                target("DiligentFX input depth", wgpu::TextureFormat::Depth32Float)
            }),
        }
    }
}

const NORMAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
// Full precision, so DiligentFX's traced test (roughness <= threshold) sees
// the roughness SGL3D's composition tests.
const MATERIAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const MOTION_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;

/// Crystal's DiligentFX attributes, the same at every
/// `Settings::screen_space_reflections` level: DiligentFX's
/// `ScreenSpaceReflectionAttribs` defaults, `RoughnessThreshold` 0.2
/// included as in AMD's SSSR sample, with the traversal budget of Diligent's
/// own renderer, Hydrogent (`HnPostProcessTaskParams`:
/// `MaxTraversalIntersections` 64), and the material input `begin` writes:
/// perceptual roughness in channel 0, as Hydrogent supplies it.
/// `ScreenSpaceReflection` sets `AlphaInterpolation` itself.
///
/// Each ray follows its lobe's peak, the mirror direction
/// (`GGXImportanceSampleBias` 1, DFX-20), as Godot's SSR traces: one GGX
/// sample per pixel leaves noise on glossy, normal-mapped receivers that the
/// denoiser cannot remove, and its temporal pass holds it as blotches. The
/// spatial reconstruction still widens reflections with roughness. The
/// temporal pass keeps 0.95 of its clamped history, as Wicked Engine's
/// `ssr_temporalCS` (the pass it derives from) does; at DiligentFX's 1.0 the
/// current frame never enters and stale history smears as the camera moves.
pub(crate) fn ssr_attribs() -> ScreenSpaceReflectionAttribs {
    ScreenSpaceReflectionAttribs {
        is_roughness_perceptual: 1,
        roughness_channel: 0,
        max_traversal_intersections: 64,
        ggx_importance_sample_bias: 1.,
        temporal_radiance_stability_factor: 0.95,
        ..Default::default()
    }
}

/// Hydrogent's texture mip bias while TAA runs (`HnBeginFrameTask`:
/// `MipBias = UseTAA ? -0.5 : 0.0`), which keeps jittered samples of
/// minified textures as sharp as the resolved image.
pub(crate) const TAA_MIP_BIAS: f32 = -0.5;

/// Hydrogent's TAA feature flags (`HnPostProcessTaskParams::TAAFeatureFlags`).
const TAA_FEATURE_FLAGS: temporal_anti_aliasing::FeatureFlags =
    temporal_anti_aliasing::FeatureFlags::BICUBIC_FILTER;

/// The frame the effects' history refers to.
struct History {
    frame_index: u32,
}

/// SGL3D's G-buffer as DiligentFX's inputs.
pub(crate) static INPUTS: shading::Module = shading::Module {
    name: "post_fx_inputs",
    source: include_str!("post_fx_inputs.wgsl"),
    deps: &[&shading::GBUFFER, &shading::FULLSCREEN_VS],
};
/// The entry point the inputs' pipeline is created with, beside
/// `shading::FULLSCREEN_VS_ENTRY`.
pub(crate) const FS_MAIN_ENTRY: &str = "fs_main";

pub(crate) struct PostFx {
    context: PostFXContext,
    /// Created when SSR first runs and dropped when it is switched off.
    ssr: Option<ScreenSpaceReflection>,
    taa: Option<TemporalAntiAliasing>,
    history: Option<History>,
    inputs: Option<Inputs>,
    prepare: wgpu::RenderPipeline,
    /// The inputs pass's near and far planes.
    planes: wgpu::Buffer,
    /// This frame's index and NDC jitter, and whether the effects' history
    /// restarts.
    frame_index: u32,
    jitter: [f32; 2],
    reset_accumulation: bool,
}

/// Diligent's shaders place view space in front of the camera at +z (`w =
/// +z`, e.g. `ScreenXYDepthToViewSpace`); SGL3D's is right-handed, looking
/// down −z. Mirroring z in view space keeps every clip position: the view
/// becomes `S · view` and the projection `projection · S`.
fn left_handed(view: Mat4, projection: Mat4) -> (Mat4, Mat4) {
    let mirror = Mat4::from_diagonal(Vec4::new(1., 1., -1., 1.));
    (mirror * view, projection * mirror)
}

/// `CameraAttribs` for SGL3D's reversed-Z camera with its infinite far plane
/// moved to `FAR_PLANE`, the projection the inputs pass converts depth to.
/// The projection is `sgl_3d::perspective`'s form: near in `w_axis.z`, no far
/// plane.
/// `projection` carries the frame's `jitter` (NDC), which `f2Jitter` records.
fn camera_attribs(
    view: Mat4,
    projection: Mat4,
    jitter: [f32; 2],
    size: [u32; 2],
    frame_index: u32,
) -> CameraAttribs {
    let far = FAR_PLANE;
    let (view, mut projection) = left_handed(view, projection);
    // Reversed-Z with a far plane: z_ndc = near (far - z) / ((far - near) z).
    let near = projection.w_axis.z;
    projection.z_axis.z = -near / (far - near);
    projection.w_axis.z = near * far / (far - near);
    let view_projection = projection * view;
    let columns = Mat4::to_cols_array;
    let mut attribs = CameraAttribs {
        f4_position: view.inverse().w_axis.to_array(),
        f4_viewport_size: [
            size[0] as f32,
            size[1] as f32,
            1. / size[0] as f32,
            1. / size[1] as f32,
        ],
        f_handness: -1.,
        ui_frame_index: frame_index,
        f2_jitter: jitter,
        m_view: columns(&view),
        m_proj: columns(&projection),
        m_view_proj: columns(&view_projection),
        m_view_inv: columns(&view.inverse()),
        m_proj_inv: columns(&projection.inverse()),
        m_view_proj_inv: columns(&view_projection.inverse()),
        ..Default::default()
    };
    // Reversed-Z: fNearZ > fFarZ.
    attribs.set_clip_planes(far, near);
    attribs
}

/// SGL3D's pass groups, one per upstream debug group.
fn pass_group(pass: &str) -> &'static str {
    match pass {
        "ComputeBlueNoiseTexture" => "DiligentFX blue noise",
        "ComputeReprojectedDepth" => "DiligentFX reprojected depth",
        "ComputePreviousDepth" => "DiligentFX previous depth",
        "ComputeHierarchicalDepthBuffer" => "SSR depth hierarchy",
        "ComputeStencilMaskAndExtractRoughness" => "SSR mask and roughness",
        "ComputeDownsampledStencilMask" => "SSR half-resolution mask",
        "ComputeIntersection" => "SSR intersection",
        "SpatialReconstruction" => "SSR spatial reconstruction",
        "ComputeTemporalAccumulation" => "SSR temporal accumulation",
        "ComputeBilateralCleanup" => "SSR bilateral cleanup",
        "TemporalAccumulation" => "TAA",
        _ => "DiligentFX",
    }
}

impl PostFx {
    /// With `taa`, the effects include TAA.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, taa: bool) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("DiligentFX inputs"),
            source: wgpu::ShaderSource::Wgsl(shading::compose(&[&INPUTS]).into()),
        });
        let target = |format| {
            Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })
        };
        Self {
            context: PostFXContext::new(
                device,
                queue,
                // PSOs are created synchronously, so there is no pending state
                // for the transition to hide; it would fade SSR in over
                // wall-clock time.
                post_fx_context::CreateInfo {
                    transition_duration: 0.,
                },
            ),
            ssr: None,
            taa: taa.then(|| TemporalAntiAliasing::new(device)),
            history: None,
            inputs: None,
            prepare: device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("DiligentFX inputs from the G-buffer"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(shading::FULLSCREEN_VS_ENTRY),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(FS_MAIN_ENTRY),
                    compilation_options: Default::default(),
                    targets: &[
                        target(NORMAL_FORMAT),
                        target(MATERIAL_FORMAT),
                        target(MOTION_FORMAT),
                    ],
                }),
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            }),
            planes: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("DiligentFX input planes"),
                    size: 16,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            frame_index: 0,
            jitter: [0.; 2],
            reset_accumulation: true,
        }
    }

    pub fn taa(&self) -> bool {
        self.taa.is_some()
    }

    /// Releases SSR and its history; the next `screen_space_reflections`
    /// starts a new effect.
    pub fn release_screen_space_reflections(&mut self) {
        self.ssr = None;
    }

    /// Before the frame's geometry: prepares the effects for this frame and
    /// returns its NDC jitter, zero without TAA or when history restarts (as
    /// Hydrogent's `HnPostProcessTask::Prepare` publishes it).
    /// `history_valid` is false after a reset, resize or camera cut;
    /// `frame_index` counts frames since that reset, so a gap means the
    /// effects missed frames.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        size: [u32; 2],
        frame_index: u32,
        history_valid: bool,
    ) -> [f32; 2] {
        // History continues only while SGL3D's does and no frame was missed,
        // the rule DiligentFX's TAA applies (DFX-12 in sgl-post-fx's
        // PROVENANCE.md).
        self.reset_accumulation = !history_valid
            || self
                .history
                .as_ref()
                .is_none_or(|history| frame_index != history.frame_index.wrapping_add(1));
        self.frame_index = frame_index;
        self.context.prepare_resources(
            device,
            &FrameDesc {
                index: frame_index,
                width: size[0],
                height: size[1],
                output_width: size[0],
                output_height: size[1],
            },
            post_fx_context::FeatureFlags::REVERSED_DEPTH,
        );
        self.jitter = match &mut self.taa {
            Some(taa) => {
                taa.prepare_resources(device, encoder, &self.context, TAA_FEATURE_FLAGS, 0);
                if self.reset_accumulation {
                    [0.; 2]
                } else {
                    taa.get_jitter_offset(0)
                }
            }
            None => [0.; 2],
        };
        self.jitter
    }

    /// After `prepare`, when FSR2 rather than TAA jitters the frame: the
    /// frame's NDC jitter, which the effects' camera records (`f2Jitter`).
    pub fn set_jitter(&mut self, jitter: [f32; 2]) {
        self.jitter = jitter;
    }

    /// After `prepare`, the G-buffer and the receivers: converts the
    /// `surface` over the G-buffer into DiligentFX's inputs and executes the
    /// post-effect context. `view` is right-handed and `projection` is
    /// `perspective`'s infinite reversed-Z form with the frame's jitter
    /// applied; `previous` is the renderer's camera history, the previous
    /// camera.
    #[allow(clippy::too_many_arguments)]
    pub fn begin(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        t: &SharedTargets,
        surface: Surface<'_>,
        size: [u32; 2],
        view: Mat4,
        projection: Mat4,
        previous: Option<CameraFrame>,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        if self
            .inputs
            .as_ref()
            .is_none_or(|inputs| inputs.size != size)
        {
            self.inputs = Some(Inputs::new(device, size));
        }
        let inputs = self.inputs.as_ref().unwrap();
        let near = projection.w_axis.z;
        crate::counters::write_buffer(
            queue,
            &self.planes,
            0,
            bytemuck::cast_slice(&[near, FAR_PLANE, 0., 0.]),
        );
        let resource = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        {
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("DiligentFX inputs"),
                layout: &self.prepare.get_bind_group_layout(0),
                entries: &[
                    resource(0, &t.normal),
                    resource(1, &t.material),
                    resource(2, &t.f0),
                    resource(3, &t.motion),
                    resource(4, surface.depth),
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: self.planes.as_entire_binding(),
                    },
                    resource(6, &t.depth),
                    resource(7, surface.receivers),
                ],
            });
            let attachment = |view| {
                Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })
            };
            let depth = &inputs.depth[(self.frame_index & 1) as usize];
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("DiligentFX inputs"),
                color_attachments: &[
                    attachment(&inputs.normal),
                    attachment(&inputs.material),
                    attachment(&inputs.motion),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: timing.and_then(|t| t.render_pass("DiligentFX inputs")),
                ..Default::default()
            });
            pass.set_pipeline(&self.prepare);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }

        let depth_view = &inputs.depth[(self.frame_index & 1) as usize];
        let previous_depth = &inputs.depth[((self.frame_index + 1) & 1) as usize];
        let camera = camera_attribs(view, projection, self.jitter, size, self.frame_index);
        let reset_accumulation = self.reset_accumulation;
        if reset_accumulation {
            // A new history has no previous frame: its previous depth is the
            // far plane, as Diligent's depth buffers are cleared when created
            // (Tutorial27_PostProcessing), so reprojection fails and the
            // frame seeds the effects' history. The previous camera is this
            // frame's (Hydrogent's HnBeginFrameTask on its first frame).
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("DiligentFX previous depth reset"),
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: previous_depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
        }
        let previous_camera = match (&self.history, previous) {
            (Some(history), Some(previous)) if !reset_accumulation => camera_attribs(
                previous.view,
                previous.jittered_projection(),
                previous.jitter,
                size,
                history.frame_index,
            ),
            _ => camera,
        };
        let timestamps =
            |pass: &'static str| timing.and_then(|timing| timing.render_pass(pass_group(pass)));
        self.context
            .execute(&mut post_fx_context::RenderAttributes {
                device,
                queue,
                device_context: encoder,
                curr_depth_buffer_srv: depth_view,
                prev_depth_buffer_srv: previous_depth,
                curr_camera: Some(&camera),
                prev_camera: Some(&previous_camera),
                camera_attribs_cb: None,
                pass_timestamps: Some(&timestamps),
            });
        self.history = Some(History {
            frame_index: self.frame_index,
        });
    }

    /// DiligentFX SSR of `radiance` after `begin`: for each traced receiver,
    /// the radiance its traced lobe reflects, premultiplied by the confidence
    /// in it (rgb), and that confidence (a). `frame_time` is the seconds
    /// since the previous frame.
    #[allow(clippy::too_many_arguments)]
    pub fn screen_space_reflections(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame_time: f32,
        half_resolution: bool,
        radiance: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) -> &wgpu::TextureView {
        let inputs = self.inputs.as_ref().expect("PostFx::begin");
        let feature_flags = if half_resolution {
            FeatureFlags::HALF_RESOLUTION
        } else {
            FeatureFlags::NONE
        };
        let ssr = self
            .ssr
            .get_or_insert_with(|| ScreenSpaceReflection::new(device));
        ssr.prepare_resources(device, encoder, &mut self.context, feature_flags);
        let depth_view = &inputs.depth[(self.frame_index & 1) as usize];
        let timestamps =
            |pass: &'static str| timing.and_then(|timing| timing.render_pass(pass_group(pass)));
        ssr.execute(&mut screen_space_reflection::RenderAttributes {
            device,
            queue,
            device_context: encoder,
            post_fx_context: &mut self.context,
            color_buffer_srv: radiance,
            depth_buffer_srv: depth_view,
            normal_buffer_srv: &inputs.normal,
            material_buffer_srv: &inputs.material,
            motion_vectors_srv: &inputs.motion,
            ssr_attribs: &ssr_attribs(),
            pass_timestamps: Some(&timestamps),
            reset_accumulation: self.reset_accumulation,
            frame_time,
        });
        ssr.get_ssr_radiance_srv()
    }

    /// The anti-aliased frame of the last `temporal_anti_aliasing`.
    pub fn taa_output(&self) -> &wgpu::TextureView {
        self.taa
            .as_ref()
            .expect("PostFx::new with TAA")
            .get_accumulated_frame_srv(false, 0)
    }

    /// DiligentFX TAA of `color`, the frame's linear HDR before bloom and
    /// tone mapping, after `begin`. Requires a context created with `taa`.
    pub fn temporal_anti_aliasing(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) -> &wgpu::TextureView {
        let taa = self.taa.as_mut().expect("PostFx::new with TAA");
        let inputs = self.inputs.as_ref().expect("PostFx::begin");
        let timestamps =
            |pass: &'static str| timing.and_then(|timing| timing.render_pass(pass_group(pass)));
        taa.execute(&mut temporal_anti_aliasing::RenderAttributes {
            device,
            queue,
            device_context: encoder,
            post_fx_context: &mut self.context,
            color_buffer_srv: color,
            depth_buffer_srv: &inputs.depth[(self.frame_index & 1) as usize],
            motion_vectors_srv: &inputs.motion,
            taa_attribs: &TemporalAntiAliasingAttribs {
                reset_accumulation: u32::from(self.reset_accumulation),
                ..Default::default()
            },
            accumulation_buffer_idx: 0,
            pass_timestamps: Some(&timestamps),
        });
        taa.get_accumulated_frame_srv(false, 0)
    }
}
