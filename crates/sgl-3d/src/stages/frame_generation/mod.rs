//! FSR3 generation: prepare the existing surface depth/motion before post;
//! optical flow and interpolation consume completed display color without UI.
//! Honors the frame's camera, timing and history, independently of AA.
//! Timing groups: FSR3 prepare, FSR3 optical flow, FSR3 interpolation.
mod sdk;
use crate::{FrameGenerationStatus, settings::FrameGeneration, view::frame::FrameContext};
use sp_fidelity::{
    frameinterpolation::FfxFrameInterpolationPrepareDescription, interface::FfxInterfaceRef,
    types::*,
};
use sp_fidelity_wgpu::{FfxWgpuBackend, FfxWgpuPassTimestamps};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    rc::Rc,
};

pub(crate) struct Generation {
    backend: Rc<RefCell<FfxWgpuBackend>>,
    interface: FfxInterfaceRef,
    sdk: Option<sdk::Sdk>,
    pub status: FrameGenerationStatus,
    pub frame_id: u64,
    pub reset: bool,
    pending: bool,
    prepared: bool,
    generated: bool,
    failed: bool,
}
impl Generation {
    pub fn new(backend: Rc<RefCell<FfxWgpuBackend>>, interface: FfxInterfaceRef) -> Self {
        Self {
            backend,
            interface,
            sdk: None,
            status: FrameGenerationStatus::Off,
            frame_id: 0,
            reset: true,
            pending: false,
            prepared: false,
            generated: false,
            failed: false,
        }
    }
    /// Caller drained old GPU work before this can replace SDK allocations.
    #[allow(clippy::too_many_arguments)]
    pub fn configure(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requested: FrameGeneration,
        render: [u32; 2],
        display: [u32; 2],
        format: wgpu::TextureFormat,
        encoded: bool,
    ) {
        self.sdk = None;
        self.reset = true;
        self.pending = false;
        self.prepared = false;
        self.generated = false;
        self.failed = false;
        self.status = FrameGenerationStatus::Off;
        if requested == FrameGeneration::Off {
            return;
        }
        if !device
            .features()
            .contains(sp_fidelity_wgpu::required_features())
        {
            self.status = FrameGenerationStatus::Unavailable("device lacks FSR3 features".into());
            return;
        }
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let created = sdk::Sdk::new(
            device,
            queue,
            self.backend.clone(),
            self.interface.clone(),
            render,
            display,
            format,
            encoded,
        );
        let error = validation_error(scope);
        match (created, error) {
            (Ok(sdk), None) => {
                self.sdk = Some(sdk);
                self.status = FrameGenerationStatus::Active;
            }
            (result, error) => {
                self.status = FrameGenerationStatus::Unavailable(error.map_or_else(
                    || format!("FSR3 initialization failed ({:#x})", result.err().unwrap()),
                    |e| e.to_string(),
                ))
            }
        }
    }
    pub fn available(&self) -> bool {
        self.sdk.is_some() && !self.failed
    }
    pub fn output(&self) -> Option<&wgpu::Texture> {
        self.generated.then(|| &self.sdk.as_ref().unwrap().output)
    }
    pub fn abandon(&mut self) {
        self.reset = true;
        self.pending = false;
        self.prepared = false;
        self.generated = false;
    }
    pub fn finish_frame(&mut self) {
        self.frame_id = self.frame_id.wrapping_add(1);
        self.pending = false;
        self.reset = !self.generated;
    }
    pub fn prepare(&mut self, ctx: &mut FrameContext<'_>) {
        self.reset |= self.pending || !ctx.history.valid;
        self.pending = true;
        self.generated = false;
        self.prepared = false;
        if !self.available() {
            return;
        }
        if !ctx.effective.frame_generation {
            self.status = FrameGenerationStatus::Unavailable(
                "frame generation requires SGL's perspective camera".into(),
            );
            self.reset = true;
            return;
        }
        self.status = FrameGenerationStatus::Active;
        let projection = ctx.input.camera.projection;
        let size = ctx.sizes.render;
        let jitter = ctx.history.camera.jitter;
        let desc = FfxFrameInterpolationPrepareDescription {
            render_size: FfxDimensions2D {
                width: size[0],
                height: size[1],
            },
            jitter_offset: FfxFloatCoords2D {
                x: jitter[0] * size[0] as f32 * 0.5,
                y: -jitter[1] * size[1] as f32 * 0.5,
            },
            motion_vector_scale: FfxFloatCoords2D {
                x: -(size[0] as f32),
                y: -(size[1] as f32),
            },
            frame_time_delta: ctx.input.frame_time_ms,
            camera_near: f32::MAX,
            camera_far: projection.w_axis.z,
            camera_fov_angle_vertical: 2. * projection.y_axis.y.recip().atan(),
            view_space_to_meters_factor: 1.,
            frame_id: self.frame_id,
            ..Default::default()
        };
        let sdk = self.sdk.as_mut().unwrap();
        let depth = ctx.surface.depth.texture().clone();
        let motion = ctx.targets.motion.texture().clone();
        let result = record(&self.backend, ctx, "FSR3 prepare", |list| {
            sdk.prepare(list, desc, &depth, &motion)
        });
        match result {
            Ok(()) => self.prepared = true,
            Err(error) => self.fail(error),
        }
    }
    pub fn encode(
        &mut self,
        ctx: &mut FrameContext<'_>,
        color: &wgpu::Texture,
        distortion: Option<&wgpu::Texture>,
    ) {
        if !self.prepared {
            return;
        }
        let reset = self.reset;
        let sdk = self.sdk.as_mut().unwrap();
        let result = record(&self.backend, ctx, "FSR3 optical flow", |list| {
            sdk.optical(list, color, reset)
        })
        .and_then(|()| {
            record(&self.backend, ctx, "FSR3 interpolation", |list| {
                sdk.interpolate(list, color, reset, distortion)
            })
        });
        match result {
            Ok(()) => self.generated = true,
            Err(error) => self.fail(error),
        }
    }
    fn fail(&mut self, error: String) {
        self.failed = true;
        self.generated = false;
        self.reset = true;
        self.status = FrameGenerationStatus::Unavailable(error);
    }
}
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
fn record(
    backend: &Rc<RefCell<FfxWgpuBackend>>,
    ctx: &mut FrameContext<'_>,
    label: &'static str,
    dispatch: impl FnOnce(FfxCommandList) -> Result<(), sp_fidelity::error::FfxErrorCode>,
) -> Result<(), String> {
    let scope = ctx.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let encoder = std::mem::replace(
        ctx.encoder,
        ctx.device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) }),
    );
    let list = {
        let mut backend = backend.borrow_mut();
        backend.set_pass_timestamps(
            ctx.timing
                .and_then(|timing| timing.reserve_compute_passes(label, 128))
                .map(|(query_set, first, count)| FfxWgpuPassTimestamps {
                    query_set,
                    first,
                    count,
                    used: Cell::new(0),
                }),
        );
        backend.ffx_get_command_list_wgpu(encoder)
    };
    let result = dispatch(list);
    let mut backend = backend.borrow_mut();
    *ctx.encoder = backend
        .ffx_take_command_list_wgpu(list)
        .expect("generation command list");
    if let (Some(timing), Some(pool)) = (ctx.timing, backend.set_pass_timestamps(None)) {
        timing.release_unused(pool.count - pool.used.get());
    }
    let job = backend.take_job_error();
    drop(backend);
    let error = validation_error(scope);
    match (result, job, error) {
        (Ok(()), None, None) => Ok(()),
        (_, _, Some(error)) => Err(error.to_string()),
        (_, Some(error), _) => Err(error.to_string()),
        (Err(code), _, _) => Err(format!("{label} failed ({code:#x})")),
    }
}
