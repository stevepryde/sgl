//! AMD FidelityFX Super Resolution 2 (the `sp-fidelity` port of FidelityFX SDK
//! 1.1.4, run by `sp-fidelity-wgpu`) as SGL3D's upscaling
//! antialiasing: the one place SGL3D-specific adaptation of the port exists,
//! as `view/post_fx.rs` is for DiligentFX.
//!
//! Integrated as AMD's documentation describes
//! (`vendor/sdk-1.1.4/docs/techniques/super-resolution-temporal.md` in `sp-fidelity`,
//! cited below as "docs" with line numbers) and as AMD's FSR sample does it
//! (`samples/fsrapi/fsrapirendermodule.{h,cpp}` at the SDK revision):
//!
//! - Placement (docs 281–301): every pass up to and including heat distortion
//!   renders at the render size; FSR2 upscales the linear HDR frame to the
//!   scene size; bloom and tone mapping follow at the scene size.
//! - Context: created for the scene size with it as the maximum render size,
//!   and recreated when that changes, as the sample's `UpdateFSRContext`
//!   does; a quality change only changes the dispatch's render size.
//! - Jitter: `ffxFsr2GetJitterOffset` over `ffxFsr2GetJitterPhaseCount`
//!   phases, counted by frame as the sample's `m_JitterIndex`; the projection
//!   offset is `(2 x / width, -2 y / height)` in NDC and the dispatch gets the
//!   pixel offset (docs 382–435).
//! - Texture mip bias `log2(render / display) - 1` (docs 453–470; the
//!   sample's `cMipBias` at its preset ratios).
//! - Motion vectors: SGL3D's unjittered `stable_motion` is current minus
//!   previous UV (+y down) at the render size. FSR2 wants the motion from the
//!   current pixel to its previous position (docs 199): it multiplies by
//!   `motionVectorScale / renderSize` (`fsr2Dispatch`,
//!   `LoadInputMotionVector`) and reprojects to `uv + motion`
//!   (`ffx_fsr2_reproject.h`), so the scale is minus the render size.
//! - Depth: SGL3D's infinite reversed-Z surface depth (the opaque depth, or
//!   the receivers' over it: the Surface contract), flagged
//!   `FFX_FSR2_ENABLE_DEPTH_INVERTED | FFX_FSR2_ENABLE_DEPTH_INFINITE`, with
//!   `cameraNear = FLT_MAX` and `cameraFar` the near plane, as the sample and
//!   the SDK's debug checker expect for that configuration.
//! - Exposure: the frame's exposure stage meters the frame before FSR2, and
//!   FSR2 reads its 1×1 exposure texture: the docs (262) ask for the exposure
//!   the later tone mapping uses. `FFX_FSR2_ENABLE_AUTO_EXPOSURE` (docs 266,
//!   used by the sample) would substitute an ISO-100 estimate of the frame's
//!   average luminance, a different exposure than the displayed image has.
//!   The input is not pre-exposed: `preExposure` is 1.
//! - `FFX_FSR2_ENABLE_HIGH_DYNAMIC_RANGE`: linear HDR input (docs 478–482).
//! - `frameTimeDelta` in milliseconds from the frame's `frame_time_ms`
//!   (docs 472–476).
//! - Sharpening: RCAS as `Settings::fsr2_sharpening` and `fsr2_sharpness`
//!   choose, a slider as the docs (110) ask applications to expose; by
//!   default on at 0.8, the sample's `m_RCASSharpen` and `m_Sharpness`.
//! - Reactive and transparency-and-composition masks (docs 222–242) at the
//!   render size, written as AMD's FSR sample writes them from its
//!   translucency pass (`view::targets::mask_targets`): cleared before the camera's
//!   transparent draws, additive effects marking reactivity as its reactive
//!   particles do and mist marking transparency and composition as its
//!   translucent materials do (`glow.wgsl`, `mist.wgsl`). Before them,
//!   opaque and masked surfaces whose material's normal layers move mark
//!   transparency and composition 1, as its animated textures do (docs 237
//!   name animated textures; `view::targets::composition_targets`). SDK
//!   1.1.4's `ffxFsr2ContextGenerateReactiveMask` schedules nothing, and the
//!   docs prefer masks rendered from materials to generated ones.
//! - Reset on SGL3D history loss: an invalid temporal frame (a camera cut, a
//!   resize or another scene), a missed frame or a new render size; the docs
//!   (447–451) reset for camera jump cuts. Content and lighting changes keep
//!   history: FSR2's shading change detection handles them (docs 720, 777).
//!
//! The context's pipelines are created with it; an error there (for example a
//! pass the device cannot run) leaves FSR2 unavailable and the caller falls
//! back to TAA. So does a dispatch whose job fails, which the backend
//! reports through `take_job_error`, since `ffxFsr2ContextDispatch` ignores
//! its jobs' result.
use crate::settings::Fsr2Quality;
use sp_fidelity::fsr2::{
    FFX_FSR2_ENABLE_DEPTH_INFINITE, FFX_FSR2_ENABLE_DEPTH_INVERTED,
    FFX_FSR2_ENABLE_HIGH_DYNAMIC_RANGE, FfxFsr2Context, FfxFsr2ContextDescription,
    FfxFsr2DispatchDescription, FfxFsr2QualityMode, ffx_fsr2_context_create,
    ffx_fsr2_context_destroy, ffx_fsr2_context_dispatch, ffx_fsr2_get_jitter_offset,
    ffx_fsr2_get_jitter_phase_count, ffx_fsr2_get_render_resolution_from_quality_mode,
};
use sp_fidelity::types::{
    FFX_RESOURCE_STATE_COMPUTE_READ, FFX_RESOURCE_STATE_UNORDERED_ACCESS, FfxDimensions2D,
    FfxFloatCoords2D,
};
use sp_fidelity_wgpu::{FfxWgpuBackend, FfxWgpuPassTimestamps, ffx_get_interface_wgpu};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The device features FSR2 needs: the wgpu backend's.
fn required_features() -> wgpu::Features {
    sp_fidelity_wgpu::required_features()
}

/// `graphics_device::fsr2_features`.
pub(crate) fn features(adapter: &wgpu::Adapter) -> wgpu::Features {
    let available = adapter.features();
    if available.contains(required_features()) {
        required_features() | (available & sp_fidelity_wgpu::optional_features())
    } else {
        wgpu::Features::empty()
    }
}

/// The SDK quality mode, `None` for Native AA (the sample's `NativeAA`
/// preset, ratio 1).
fn quality_mode(quality: Fsr2Quality) -> Option<FfxFsr2QualityMode> {
    match quality {
        Fsr2Quality::NativeAa => None,
        Fsr2Quality::Quality => Some(FfxFsr2QualityMode::Quality),
        Fsr2Quality::Balanced => Some(FfxFsr2QualityMode::Balanced),
        Fsr2Quality::Performance => Some(FfxFsr2QualityMode::Performance),
        Fsr2Quality::UltraPerformance => Some(FfxFsr2QualityMode::UltraPerformance),
    }
}

/// The render size for a `display` (scene) size:
/// `ffxFsr2GetRenderResolutionFromQualityMode`, at least one pixel.
pub(crate) fn render_size(display: [u32; 2], quality: Fsr2Quality) -> [u32; 2] {
    let Some(mode) = quality_mode(quality) else {
        return display;
    };
    let (width, height) =
        ffx_fsr2_get_render_resolution_from_quality_mode(display[0], display[1], mode)
            .expect("every FfxFsr2QualityMode is in range");
    [width.max(1), height.max(1)]
}

/// The texture mip bias AMD recommends, `log2(render / display) - 1`
/// (docs 458).
pub(crate) fn mip_bias(render: [u32; 2], display: [u32; 2]) -> f32 {
    (render[0] as f32 / display[0] as f32).log2() - 1.
}

/// Compute passes one dispatch may record: its clears and passes
/// (`fsr2Dispatch`), with room to spare. Timed together as "FSR2", as AMD's
/// sample profiles the whole upscale.
const MAX_TIMED_PASSES: u32 = 24;

/// Validation errors caught by `scope`. Native wgpu reports them when the
/// scope is popped, so the future is ready at once. WebGPU's would not be,
/// but FSR2 is never created there: `fsr2_features` is empty and `Fsr2::new`
/// refuses a device without them before pushing a scope.
fn validation_error(scope: wgpu::ErrorScopeGuard) -> Option<wgpu::Error> {
    let mut future = std::pin::pin!(scope.pop());
    match future
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(error) => error,
        std::task::Poll::Pending => None,
    }
}

/// The history FSR2's accumulation continues from.
struct History {
    frames: u32,
    render_size: [u32; 2],
}

/// The camera and frame inputs of one dispatch.
pub(crate) struct Inputs<'a> {
    /// The complete linear HDR frame at the render size.
    pub color: &'a wgpu::TextureView,
    pub depth: &'a wgpu::TextureView,
    pub motion: &'a wgpu::TextureView,
    /// The reactive and transparency-and-composition masks.
    pub masks: &'a [wgpu::TextureView; 2],
    /// `perspective`'s infinite reversed-Z projection, unjittered.
    pub projection: glam::Mat4,
    pub frame_time_ms: f32,
    /// The frame's 1×1 R32Float exposure multiplier.
    pub exposure: &'a wgpu::Texture,
    /// RCAS's sharpness in 0..=1, `None` for no sharpening.
    pub sharpness: Option<f32>,
}

pub(crate) struct Fsr2 {
    backend: Rc<RefCell<FfxWgpuBackend>>,
    context: FfxFsr2Context,
    display_size: [u32; 2],
    output: wgpu::TextureView,
    jitter_index: i32,
    /// This frame's jitter in render pixels, as `jitterOffset` takes it.
    jitter: [f32; 2],
    render_size: [u32; 2],
    reset: bool,
    history: Option<History>,
}

impl Fsr2 {
    /// A context upscaling to `display_size`, or why FSR2 cannot run.
    pub fn new(device: &wgpu::Device, display_size: [u32; 2]) -> Result<Self, String> {
        if !device.features().contains(required_features()) {
            return Err(format!(
                "the device lacks FSR2's features {:?}",
                required_features() - device.features()
            ));
        }
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let (backend, interface) = ffx_get_interface_wgpu(device);
        let size = FfxDimensions2D {
            width: display_size[0],
            height: display_size[1],
        };
        let mut context = FfxFsr2Context::default();
        let created = ffx_fsr2_context_create(
            &mut context,
            &FfxFsr2ContextDescription {
                flags: FFX_FSR2_ENABLE_HIGH_DYNAMIC_RANGE
                    | FFX_FSR2_ENABLE_DEPTH_INVERTED
                    | FFX_FSR2_ENABLE_DEPTH_INFINITE,
                max_render_size: size,
                display_size: size,
                fp_message: None,
                backend_interface: interface,
            },
        );
        let texture = |label, size: [u32; 2], format, usage| {
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
        // The backend's output is RGBA16F; captures copy from it.
        let output = texture(
            "FSR2 output",
            display_size,
            crate::shading::gbuffer::COLOR,
            wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        )
        .create_view(&Default::default());
        let error = validation_error(scope);
        let fsr2 = Self {
            backend,
            context,
            display_size,
            output,
            jitter_index: 0,
            jitter: [0.; 2],
            render_size: display_size,
            reset: true,
            history: None,
        };
        match (created, error) {
            (Ok(()), None) => Ok(fsr2),
            (Err(code), _) => Err(format!("FSR2 context creation failed ({code:#x})")),
            (Ok(()), Some(error)) => Err(format!("FSR2 context creation failed: {error}")),
        }
    }

    pub fn display_size(&self) -> [u32; 2] {
        self.display_size
    }

    /// The upscaled frame of the last successful `dispatch`.
    pub fn output(&self) -> &wgpu::TextureView {
        &self.output
    }

    /// Before the frame's geometry: decides whether history continues and
    /// returns the frame's projection jitter in NDC.
    pub fn prepare(&mut self, render_size: [u32; 2], frames: u32, history_valid: bool) -> [f32; 2] {
        self.reset = !history_valid
            || self.history.as_ref().is_none_or(|history| {
                frames != history.frames.wrapping_add(1) || history.render_size != render_size
            });
        self.history = Some(History {
            frames,
            render_size,
        });
        self.render_size = render_size;
        let [width, height] = render_size.map(|v| i32::try_from(v).unwrap_or(i32::MAX));
        let phases = ffx_fsr2_get_jitter_phase_count(
            width,
            i32::try_from(self.display_size[0]).unwrap_or(i32::MAX),
        );
        let (x, y) = ffx_fsr2_get_jitter_offset(self.jitter_index, phases)
            .expect("ffxFsr2GetJitterPhaseCount is positive");
        self.jitter_index = self.jitter_index.wrapping_add(1).max(0);
        self.jitter = [x, y];
        [2. * x / width as f32, -2. * y / height as f32]
    }

    /// Upscales `inputs.color` into `output`. An error leaves the output
    /// undefined for this frame. A job that fails, one whose bind group wgpu
    /// rejects among them, records nothing invalid and stops the jobs after
    /// it (sp-fidelity-wgpu's SDK-P28), so the frame's encoder stays valid
    /// and the caller falls back to TAA; the error carries wgpu's message.
    pub fn dispatch(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        inputs: &Inputs<'_>,
        timing: Option<&crate::timing::GpuTiming>,
    ) -> Result<(), String> {
        let projection = inputs.projection;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        // The backend records into an encoder it owns while it dispatches;
        // lend it the frame's encoder and take it back afterwards.
        let recorded = std::mem::replace(
            encoder,
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame after FSR2"),
            }),
        );
        let mut description = FfxFsr2DispatchDescription {
            jitter_offset: FfxFloatCoords2D {
                x: self.jitter[0],
                y: self.jitter[1],
            },
            motion_vector_scale: FfxFloatCoords2D {
                x: -(self.render_size[0] as f32),
                y: -(self.render_size[1] as f32),
            },
            render_size: FfxDimensions2D {
                width: self.render_size[0],
                height: self.render_size[1],
            },
            enable_sharpening: inputs.sharpness.is_some(),
            sharpness: inputs.sharpness.unwrap_or_default(),
            frame_time_delta: inputs.frame_time_ms,
            pre_exposure: 1.,
            reset: self.reset,
            camera_near: f32::MAX,
            // `perspective`: near in w_axis.z, cot(fov / 2) in y_axis.y.
            camera_far: projection.w_axis.z,
            camera_fov_angle_vertical: 2. * projection.y_axis.y.recip().atan(),
            view_space_to_meters_factor: 1.,
            ..Default::default()
        };
        {
            let mut backend = self.backend.borrow_mut();
            backend.set_pass_timestamps(
                timing
                    .and_then(|t| t.reserve_compute_passes("FSR2", MAX_TIMED_PASSES))
                    .map(|(query_set, first, count)| FfxWgpuPassTimestamps {
                        query_set,
                        first,
                        count,
                        used: Cell::new(0),
                    }),
            );
            let read = FFX_RESOURCE_STATE_COMPUTE_READ;
            description.command_list = backend.ffx_get_command_list_wgpu(recorded);
            description.color =
                backend.ffx_get_resource_wgpu(inputs.color.texture(), "FSR2_InputColor", read);
            description.depth =
                backend.ffx_get_resource_wgpu(inputs.depth.texture(), "FSR2_InputDepth", read);
            description.motion_vectors = backend.ffx_get_resource_wgpu(
                inputs.motion.texture(),
                "FSR2_InputMotionVectors",
                read,
            );
            description.exposure =
                backend.ffx_get_resource_wgpu(inputs.exposure, "FSR2_InputExposure", read);
            let [reactive, composition] = inputs.masks;
            description.reactive =
                backend.ffx_get_resource_wgpu(reactive.texture(), "FSR2_InputReactiveMap", read);
            description.transparency_and_composition = backend.ffx_get_resource_wgpu(
                composition.texture(),
                "FSR2_TransparencyAndCompositionMap",
                read,
            );
            description.output = backend.ffx_get_resource_wgpu(
                self.output.texture(),
                "FSR2_OutputUpscaledColor",
                FFX_RESOURCE_STATE_UNORDERED_ACCESS,
            );
        }
        let result = ffx_fsr2_context_dispatch(&mut self.context, &description);
        let mut backend = self.backend.borrow_mut();
        if let (Some(timing), Some(pool)) = (timing, backend.set_pass_timestamps(None)) {
            timing.release_unused(pool.count - pool.used.get());
        }
        *encoder = backend
            .ffx_take_command_list_wgpu(description.command_list)
            .expect("the FSR2 command list");
        // `ffxFsr2ContextDispatch` ignores its jobs' result, as AMD's does.
        let job_error = backend.take_job_error();
        drop(backend);
        let error = validation_error(scope);
        match (result, job_error, error) {
            (Ok(()), None, None) => Ok(()),
            (Err(code), _, _) => Err(format!("FSR2 dispatch failed ({code:#x})")),
            (Ok(()), Some(job), _) => Err(format!("FSR2 dispatch failed: {job}")),
            (Ok(()), None, Some(error)) => Err(format!("FSR2 dispatch failed: {error}")),
        }
    }
}

impl Drop for Fsr2 {
    fn drop(&mut self) {
        // fsr2Release reports no error; a context whose creation failed is
        // released the same way.
        let _ = ffx_fsr2_context_destroy(&mut self.context);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
