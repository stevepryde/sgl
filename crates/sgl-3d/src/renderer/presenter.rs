//! Native presentation boundary. The game owns its window, loop and simulation;
//! a borrowing frame owns the one encoder/scene pair that may be committed.
use super::Renderer;
use crate::{
    FrameGenerationStatus, FrameInput, RendererError, Scene,
    settings::Settings,
    stages::{frame_generation::Generation, post::Output},
    timing::GpuTiming,
    view::targets::target,
};
use sp_fidelity::{
    interface::{FfxFrameGenerationConfig, FfxInterfaceRef},
    types::FfxRect2D,
};
use sp_fidelity_wgpu::{
    FfxWgpuBackend, ffx_get_interface_wgpu,
    presentation::{
        FrameInterpolationSwapchain, PresentFrame, PresentationError, PresentationReport,
        PresentationTarget, SurfaceTarget, UI_PREMULTIPLIED_ALPHA,
    },
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
};

// Scheduler slots need storage-capable raw intermediates. Surface attachments
// need only rendering support (for example RGB10A2 without adapter-specific
// storage features). Keep the actual surface configuration at this boundary;
// the scheduler's existing composition pass converts into the acquired format.
struct DisplayTarget<T> {
    target: T,
    config: Arc<Mutex<wgpu::SurfaceConfiguration>>,
}
impl<T: PresentationTarget> PresentationTarget for DisplayTarget<T> {
    fn configure(
        &mut self,
        device: &wgpu::Device,
        _: &wgpu::SurfaceConfiguration,
    ) -> Result<(), PresentationError> {
        self.target.configure(device, &self.config.lock().unwrap())
    }
    fn acquire(&mut self) -> Result<PresentFrame, PresentationError> {
        self.target.acquire()
    }
}
fn scheduler_config(
    config: &wgpu::SurfaceConfiguration,
    output: Output,
) -> wgpu::SurfaceConfiguration {
    let mut config = config.clone();
    config.format = output.format;
    config.view_formats.clear();
    config
}

#[derive(Debug)]
pub enum PresenterError {
    Renderer(RendererError),
    Presentation(PresentationError),
}
impl std::fmt::Display for PresenterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Renderer(e) => e.fmt(f),
            Self::Presentation(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for PresenterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Renderer(e) => Some(e),
            Self::Presentation(e) => Some(e),
        }
    }
}
impl From<RendererError> for PresenterError {
    fn from(e: RendererError) -> Self {
        Self::Renderer(e)
    }
}
impl From<PresentationError> for PresenterError {
    fn from(e: PresentationError) -> Self {
        Self::Presentation(e)
    }
}

/// A native renderer with FSR3 generation, separate UI and paced presentation.
/// The supplied surface must not be acquired/configured/presented independently.
/// Request `graphics_device::fsr_features` for generation; missing features use
/// real frames and are reported by `frame_generation_status`.
pub struct Presenter {
    scheduler: Option<FrameInterpolationSwapchain>,
    renderer: Renderer,
    device: wgpu::Device,
    queue: wgpu::Queue,
    backend: Rc<RefCell<FfxWgpuBackend>>,
    interface: FfxInterfaceRef,
    config: wgpu::SurfaceConfiguration,
    display_config: Arc<Mutex<wgpu::SurfaceConfiguration>>,
    capabilities: wgpu::SurfaceCapabilities,
    settings: Settings,
    scale: f32,
    output: Output,
    color: wgpu::TextureView,
    ui: wgpu::TextureView,
    surface_failed: bool,
}
impl Presenter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface: Arc<wgpu::Surface<'static>>,
        config: &wgpu::SurfaceConfiguration,
        capabilities: &wgpu::SurfaceCapabilities,
        device_scale: f32,
        settings: &Settings,
    ) -> Result<Self, PresenterError> {
        Self::with_target(
            device,
            queue,
            SurfaceTarget::new(surface),
            config,
            capabilities,
            device_scale,
            settings,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_target(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: impl PresentationTarget,
        config: &wgpu::SurfaceConfiguration,
        capabilities: &wgpu::SurfaceCapabilities,
        device_scale: f32,
        settings: &Settings,
    ) -> Result<Self, PresenterError> {
        if config.width == 0 || config.height == 0 {
            return Err(
                PresentationError::InvalidInput("presenter requires nonzero dimensions").into(),
            );
        }
        let output = Self::output(device, config, capabilities)?;
        let (backend, interface) = ffx_get_interface_wgpu(device);
        let display_config = Arc::new(Mutex::new(config.clone()));
        let target = DisplayTarget {
            target,
            config: display_config.clone(),
        };
        let scheduler = FrameInterpolationSwapchain::new(
            backend.clone(),
            queue,
            target,
            &scheduler_config(config, output),
        )?;
        let mut renderer = Renderer::with_output(
            device,
            queue,
            output,
            [config.width, config.height],
            device_scale,
            settings,
        )?;
        let mut generation = Generation::new(backend.clone(), interface.clone());
        generation.configure(
            device,
            settings.frame_generation,
            renderer.sizes.render,
            renderer.sizes.output,
            output.format,
            output.encodes_srgb,
        );
        renderer.generation = Some(generation);
        Ok(Self {
            scheduler: Some(scheduler),
            renderer,
            device: device.clone(),
            queue: queue.clone(),
            backend,
            interface,
            config: config.clone(),
            display_config,
            capabilities: wgpu::SurfaceCapabilities {
                formats: capabilities.formats.clone(),
                format_capabilities: capabilities.format_capabilities.clone(),
                present_modes: capabilities.present_modes.clone(),
                alpha_modes: capabilities.alpha_modes.clone(),
                usages: capabilities.usages,
            },
            settings: *settings,
            scale: device_scale,
            output,
            color: crate::view::targets::target(
                device,
                "presenter display color",
                [config.width, config.height],
                output.format,
            ),
            ui: crate::view::targets::target(
                device,
                "presenter UI",
                [config.width, config.height],
                output.format,
            ),
            surface_failed: false,
        })
    }
    fn output(
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        capabilities: &wgpu::SurfaceCapabilities,
    ) -> Result<Output, RendererError> {
        let surface = Output::surface(config, capabilities, device.adapter_info().backend).ok_or(
            RendererError::UnsupportedSurfaceOutput {
                format: config.format,
                color_space: config.color_space,
            },
        )?;
        // The scheduler copies raw display values through a linear surface view.
        // Encode explicitly even for sRGB surfaces; RGBA intermediates also
        // avoid requiring BGRA storage support from the device.
        Ok(Output {
            format: match surface.format.remove_srgb_suffix() {
                wgpu::TextureFormat::Bgra8Unorm => wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureFormat::Rgb10a2Unorm => wgpu::TextureFormat::Rgba16Float,
                format => format,
            },
            encodes_srgb: surface.encodes_srgb || surface.format.is_srgb(),
        })
    }
    /// Read-only renderer queries. Submission/history mutation stays with Presenter.
    pub fn renderer(&self) -> &Renderer {
        &self.renderer
    }
    pub fn frame_generation_status(&self) -> FrameGenerationStatus {
        self.renderer.frame_generation_status()
    }
    /// UI uses premultiplied alpha in this non-sRGB attachment's stored color domain.
    pub fn ui_format(&self) -> wgpu::TextureFormat {
        self.output.format
    }
    /// True: UI RGB must be sRGB-encoded before premultiplication. False: linear.
    pub fn ui_encodes_srgb(&self) -> bool {
        self.output.encodes_srgb
    }
    pub fn configuration(&self) -> &wgpu::SurfaceConfiguration {
        &self.config
    }

    /// Drain before replacing resources or reconfiguring the surface. Zero size
    /// leaves the last nonzero configuration intact while a window is minimized.
    pub fn resize(
        &mut self,
        config: &wgpu::SurfaceConfiguration,
        scale: f32,
        settings: &Settings,
    ) -> Result<(), PresenterError> {
        if config.width == 0 || config.height == 0 {
            return Ok(());
        }
        if *config == self.config
            && scale == self.scale
            && *settings == self.settings
            && !self.surface_failed
        {
            return Ok(());
        }
        let output = Self::output(&self.device, config, &self.capabilities)?;
        // A surface error does not prevent draining the remaining jobs. A GPU
        // or worker failure does: leave the old contexts alive and report it.
        if let Err(e) = self.scheduler.as_mut().unwrap().wait_for_presents()
            && !matches!(e, PresentationError::Surface(_))
        {
            return Err(e.into());
        }
        if *config != self.config || self.surface_failed {
            *self.display_config.lock().unwrap() = config.clone();
            if let Err(error) = self
                .scheduler
                .as_mut()
                .unwrap()
                .resize(&scheduler_config(config, output))
            {
                *self.display_config.lock().unwrap() = self.config.clone();
                return Err(error.into());
            }
        }
        if output.format != self.output.format || output.encodes_srgb != self.output.encodes_srgb {
            self.renderer = Renderer::with_output(
                &self.device,
                &self.queue,
                output,
                [config.width, config.height],
                scale,
                settings,
            )?;
            self.renderer.generation = Some(Generation::new(
                self.backend.clone(),
                self.interface.clone(),
            ));
        } else {
            self.renderer
                .resize(&self.device, [config.width, config.height], scale, settings);
        }
        if config.width != self.config.width
            || config.height != self.config.height
            || output.format != self.output.format
        {
            self.color = target(
                &self.device,
                "presenter display color",
                [config.width, config.height],
                output.format,
            );
            self.ui = target(
                &self.device,
                "presenter UI",
                [config.width, config.height],
                output.format,
            );
        }
        self.renderer.generation.as_mut().unwrap().configure(
            &self.device,
            settings.frame_generation,
            self.renderer.sizes.render,
            self.renderer.sizes.output,
            output.format,
            output.encodes_srgb,
        );
        self.scheduler.as_mut().unwrap().reset_history();
        self.config = config.clone();
        self.settings = *settings;
        self.scale = scale;
        self.output = output;
        self.surface_failed = false;
        Ok(())
    }
    /// Render one real frame and clear its UI. Add UI commands through the
    /// returned frame, then consume it with `present`. Dropping it abandons the
    /// encoder without committing scene/camera histories.
    pub fn render<'a>(
        &'a mut self,
        scene: &'a mut Scene,
        input: &FrameInput,
        settings: &Settings,
        timing: Option<&'a mut GpuTiming>,
    ) -> Result<PresentationFrame<'a>, PresenterError> {
        self.resize(&self.config.clone(), self.scale, settings)?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("SGL presented frame"),
            });
        self.renderer.render(
            &self.device,
            &self.queue,
            &mut encoder,
            scene,
            input,
            settings,
            &self.color,
            timing.as_deref(),
        );
        {
            let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear presenter UI"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.ui,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        Ok(PresentationFrame {
            presenter: self,
            scene,
            encoder: Some(encoder),
            timing,
        })
    }
    fn submit(
        &mut self,
        scene: &mut Scene,
        encoder: wgpu::CommandEncoder,
        timing: Option<&mut GpuTiming>,
    ) -> Result<(), PresenterError> {
        let generation = self.renderer.generation.as_ref().unwrap();
        let generated = generation.output().cloned();
        let config = FfxFrameGenerationConfig {
            frame_generation_enabled: generated.is_some(),
            frame_id: generation.frame_id,
            interpolation_rect: FfxRect2D {
                left: 0,
                top: 0,
                width: self.config.width as i32,
                height: self.config.height as i32,
            },
            ..Default::default()
        };
        let scheduler = self.scheduler.as_mut().unwrap();
        if generation.reset {
            scheduler.reset_history();
        }
        if let Err(error) = scheduler.configure_frame_generation(&config) {
            self.failed(&error);
            self.abandon();
            return Err(error.into());
        }
        scheduler.register_ui_resource(Some(self.ui.texture()), UI_PREMULTIPLIED_ALPHA);
        let list = self.backend.borrow_mut().ffx_get_command_list_wgpu(encoder);
        let result =
            scheduler.present_with_interpolation(list, self.color.texture(), generated.as_ref());
        let unsubmitted = self.backend.borrow_mut().ffx_take_command_list_wgpu(list);
        if unsubmitted.is_some() {
            self.abandon();
        } else {
            self.renderer.finish_frame(scene);
            if let Some(timing) = timing {
                timing.submitted(&self.queue);
            }
        }
        if let Err(error) = result {
            self.failed(&error);
            return Err(error.into());
        }
        Ok(())
    }
    fn abandon(&mut self) {
        self.renderer.rendered = None;
        self.renderer.pending_reset = true;
        self.renderer.generation.as_mut().unwrap().abandon();
        self.scheduler.as_mut().unwrap().reset_history();
    }
    fn failed(&mut self, error: &PresentationError) {
        self.surface_failed |= matches!(error, PresentationError::Surface(reason) if !matches!(*reason,"occluded"|"timeout"));
        self.renderer.generation.as_mut().unwrap().reset = true;
        self.scheduler.as_mut().unwrap().reset_history();
    }
    /// Waits for GPU reads and presentation calls, not physical scanout.
    pub fn wait_for_presents(&mut self) -> Result<(), PresenterError> {
        let result = self.scheduler.as_mut().unwrap().wait_for_presents();
        if let Err(error) = &result {
            self.failed(error);
        }
        result.map_err(Into::into)
    }
    /// The port retains at most the latest two retired real-frame reports.
    pub fn take_reports(&mut self) -> Vec<PresentationReport> {
        self.scheduler.as_mut().unwrap().take_reports()
    }
    pub fn shutdown(mut self) -> Result<(), PresenterError> {
        let result = self.scheduler.take().unwrap().shutdown();
        self.renderer.generation.take();
        result.map_err(Into::into)
    }
}
impl Drop for Presenter {
    fn drop(&mut self) {
        if let Some(scheduler) = self.scheduler.as_mut() {
            let _ = scheduler.wait_for_presents();
        }
        // Commands may reference SDK-owned allocations. Drain first, destroy
        // contexts next, and only then let the scheduler/backend drop.
        self.renderer.generation.take();
    }
}

/// One rendered scene and its owned, unsubmitted encoder. The borrow prevents
/// resize, replacement rendering, or committing a different scene until consumed.
pub struct PresentationFrame<'a> {
    presenter: &'a mut Presenter,
    scene: &'a mut Scene,
    encoder: Option<wgpu::CommandEncoder>,
    timing: Option<&'a mut GpuTiming>,
}
impl PresentationFrame<'_> {
    /// Record UI/custom commands; do not replace or submit this encoder yourself.
    pub fn encoder(&mut self) -> &mut wgpu::CommandEncoder {
        self.encoder.as_mut().unwrap()
    }
    /// Cleared transparent target. Clone the view before borrowing `encoder()`.
    pub fn ui_target(&self) -> &wgpu::TextureView {
        &self.presenter.ui
    }
    pub fn present(mut self) -> Result<(), PresenterError> {
        let encoder = self.encoder.take().unwrap();
        self.presenter
            .submit(self.scene, encoder, self.timing.take())
    }
}
impl Drop for PresentationFrame<'_> {
    fn drop(&mut self) {
        if self.encoder.is_some() {
            self.presenter.abandon();
        }
    }
}

#[cfg(test)]
mod tests;
