//! The render-side GPU context: a windowed [`Context`] brought up through
//! the window GPU bring-up the compact renderer also uses, and the surface
//! lifecycle (resize / acquire / present).
//!
//! Adapter/device are requested **once at startup** asynchronously.
//! [`Context::try_new_async`] returns a [`RendererInitError`] a game can
//! show; [`Context::new_async`] panics with it (no "no GPU" degradation).
//! [`Gpu`] is the device + queue on their own — what uploads and offscreen
//! passes need — so headless tools and tests can render without a window
//! through [`Gpu::headless`]; a [`Context`] derefs to it. The swapchain is
//! sRGB; present mode comes from the vsync flag (PR-1: default off).

use std::ops::Deref;
use std::sync::Arc;

use winit::window::Window;

pub use crate::surface::RendererInitError;
use crate::surface::{self, Swapchain, WindowGpu, WindowSurface};

/// The wgpu device and queue without any surface: everything an upload or
/// an offscreen scene render needs. A windowed [`Context`] derefs to this,
/// so `&ctx` is accepted wherever a `&Gpu` is expected.
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// Why a headless device could not be created ([`Gpu::headless`]).
#[derive(Debug)]
pub enum GpuError {
    /// No wgpu adapter is available on this host.
    NoAdapter(String),
    /// The adapter refused a default-limits device.
    NoDevice(String),
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAdapter(err) => write!(f, "no wgpu adapter available ({err})"),
            Self::NoDevice(err) => write!(f, "adapter cannot create a default device ({err})"),
        }
    }
}

impl std::error::Error for GpuError {}

/// The canvas renderer's device, windowed or headless: default limits and
/// no optional features.
fn device_descriptor(label: &'static str) -> wgpu::DeviceDescriptor<'static> {
    wgpu::DeviceDescriptor {
        label: Some(label),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }
}

impl Gpu {
    /// Native convenience wrapper around [`Self::headless_async`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn headless() -> Result<Self, GpuError> {
        pollster::block_on(Self::headless_async())
    }

    /// A device + queue from whatever adapter the host offers, with no
    /// surface: for offscreen rendering, scene capture, and tests.
    pub async fn headless_async() -> Result<Self, GpuError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .map_err(|err| GpuError::NoAdapter(err.to_string()))?;
        let (device, queue) = adapter
            .request_device(&device_descriptor("sgl headless device"))
            .await
            .map_err(|err| GpuError::NoDevice(err.to_string()))?;
        Ok(Self { device, queue })
    }
}

/// Owns the wgpu device/queue ([`Gpu`]) and the configured window surface.
pub struct Context {
    gpu: Gpu,
    /// The sRGB surface format selected for the swapchain.
    pub surface_format: wgpu::TextureFormat,
    surface: WindowSurface,
}

impl Deref for Context {
    type Target = Gpu;

    fn deref(&self) -> &Gpu {
        &self.gpu
    }
}

/// One acquired swapchain frame: an attachable view plus `present`.
pub struct Frame {
    surface_texture: wgpu::SurfaceTexture,
    pub view: wgpu::TextureView,
}

impl Frame {
    /// Queue the frame for presentation. Call after submitting all passes.
    pub fn present(self) {
        self.surface_texture.present();
    }
}

impl Context {
    /// Native convenience wrapper around [`Self::new_async`]. Browser callers
    /// must await `new_async`, since blocking the browser's main thread would
    /// prevent WebGPU initialization from making progress.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(window: Arc<Window>, vsync: bool) -> Self {
        pollster::block_on(Self::new_async(window, vsync))
    }

    /// [`Self::try_new_async`] that **panics** with the error.
    pub async fn new_async(window: Arc<Window>, vsync: bool) -> Self {
        Self::try_new_async(window, vsync)
            .await
            .unwrap_or_else(|err| panic!("GPU init failed: {err}"))
    }

    /// Native convenience wrapper around [`Self::try_new_async`]. Browser
    /// callers must await `try_new_async`, as with [`Self::new`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn try_new(window: Arc<Window>, vsync: bool) -> Result<Self, RendererInitError> {
        pollster::block_on(Self::try_new_async(window, vsync))
    }

    /// Create the surface for `window`, request a compatible adapter and
    /// device, and configure the surface. `vsync` selects the present mode
    /// (`AutoVsync` / `AutoNoVsync` — both always supported).
    pub async fn try_new_async(
        window: Arc<Window>,
        vsync: bool,
    ) -> Result<Self, RendererInitError> {
        let WindowGpu {
            device,
            queue,
            surface,
        } = surface::bring_up(
            &window,
            |_| device_descriptor("sgl canvas device"),
            |caps| {
                // Native swapchains use sRGB store conversion. WebGPU canvas
                // contexts only accept their preferred non-sRGB canvas format
                // in browsers, so the blit pipeline selects a value-preserving
                // fragment entry point.
                #[cfg(not(target_arch = "wasm32"))]
                let format = caps
                    .formats
                    .iter()
                    .copied()
                    .find(wgpu::TextureFormat::is_srgb)
                    .unwrap_or(wgpu::TextureFormat::Bgra8UnormSrgb);
                #[cfg(target_arch = "wasm32")]
                let format = caps
                    .formats
                    .iter()
                    .copied()
                    .find(|format| {
                        matches!(
                            format,
                            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                        )
                    })
                    .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
                Some(Swapchain {
                    format,
                    present_mode: if vsync {
                        wgpu::PresentMode::AutoVsync
                    } else {
                        wgpu::PresentMode::AutoNoVsync
                    },
                    alpha_mode: *caps.alpha_modes.first()?,
                    view_formats: Vec::new(),
                })
            },
        )
        .await?;
        Ok(Self {
            gpu: Gpu { device, queue },
            surface_format: surface.format(),
            surface,
        })
    }

    /// Current surface size in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        self.surface.size()
    }

    /// Reconfigure the surface to a new size (call on window resize). Ignores
    /// zero-area sizes (minimized window).
    pub fn resize(&mut self, width: u32, height: u32) {
        self.surface.resize(&self.gpu.device, width, height);
    }

    /// Acquire the next swapchain frame.
    ///
    /// Returns `Some(frame)` when there is something to draw to. On
    /// `Outdated`/`Lost` (the surface drifted out of sync — e.g. a resize),
    /// reconfigure and retry once. Transient skip cases (`Timeout`,
    /// `Occluded`, `Validation`, or a still-bad surface after the retry)
    /// return `None` so the caller drops the frame.
    pub fn acquire(&mut self) -> Option<Frame> {
        let surface_texture = self.surface.acquire(&self.gpu.device)?;
        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        Some(Frame {
            surface_texture,
            view,
        })
    }
}
