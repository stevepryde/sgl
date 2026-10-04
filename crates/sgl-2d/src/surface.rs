//! The window GPU bring-up both renderers share — surface → adapter →
//! device → configured surface — and the swapchain acquire that reconfigures
//! a surface that drifted out of sync. Each renderer supplies only its device
//! and swapchain choices. Failures return [`RendererInitError`] so a game can
//! show them.

use core::fmt;
use std::sync::Arc;

use winit::window::Window;

/// Why a renderer could not bring the GPU up for a window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RendererInitError {
    Surface(String),
    Adapter(String),
    Device(String),
    UnsupportedSurface,
}

impl fmt::Display for RendererInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Surface(message) | Self::Adapter(message) | Self::Device(message) => {
                formatter.write_str(message)
            }
            Self::UnsupportedSurface => formatter.write_str("surface has no compatible format"),
        }
    }
}

impl std::error::Error for RendererInitError {}

/// A renderer's swapchain choices from what the surface supports.
pub(crate) struct Swapchain {
    pub format: wgpu::TextureFormat,
    pub present_mode: wgpu::PresentMode,
    pub alpha_mode: wgpu::CompositeAlphaMode,
    pub view_formats: Vec<wgpu::TextureFormat>,
}

/// A device and queue with the window surface configured for them.
pub(crate) struct WindowGpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface: WindowSurface,
}

/// Create a surface for `window`, request an adapter that can present to it
/// and a device described by `device_descriptor`, and configure the surface
/// at the window's size with the swapchain `choose_swapchain` picks from the
/// surface's capabilities (`None`: nothing usable, an unsupported surface).
pub(crate) async fn bring_up(
    window: &Arc<Window>,
    device_descriptor: impl FnOnce(&wgpu::Adapter) -> wgpu::DeviceDescriptor<'static>,
    choose_swapchain: impl FnOnce(&wgpu::SurfaceCapabilities) -> Option<Swapchain>,
) -> Result<WindowGpu, RendererInitError> {
    let instance = wgpu::Instance::default();
    // The surface borrows nothing beyond the Arc<Window> it is given, so it
    // is 'static for as long as that Arc is alive.
    let surface = instance
        .create_surface(window.clone())
        .map_err(|error| RendererInitError::Surface(error.to_string()))?;
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::None,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        })
        .await
        .map_err(|error| RendererInitError::Adapter(error.to_string()))?;
    let (device, queue) = adapter
        .request_device(&device_descriptor(&adapter))
        .await
        .map_err(|error| RendererInitError::Device(error.to_string()))?;
    let swapchain = choose_swapchain(&surface.get_capabilities(&adapter))
        .ok_or(RendererInitError::UnsupportedSurface)?;
    let size = window.inner_size();
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: swapchain.format,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode: swapchain.present_mode,
        alpha_mode: swapchain.alpha_mode,
        view_formats: swapchain.view_formats,
        desired_maximum_frame_latency: 2,
    };
    surface.configure(&device, &config);
    Ok(WindowGpu {
        device,
        queue,
        surface: WindowSurface { surface, config },
    })
}

/// A window surface and the configuration it is kept configured with.
pub(crate) struct WindowSurface {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
}

impl WindowSurface {
    /// The configured swapchain format.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// The configured size in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Reconfigure at a new size. Ignores zero-area sizes (minimized window).
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(device, &self.config);
    }

    /// Acquire the next swapchain texture, reconfiguring and retrying once on
    /// `Outdated`/`Lost`; `None` means drop the frame.
    pub fn acquire(&mut self, device: &wgpu::Device) -> Option<wgpu::SurfaceTexture> {
        use wgpu::CurrentSurfaceTexture as Current;
        match self.surface.get_current_texture() {
            Current::Success(texture) | Current::Suboptimal(texture) => Some(texture),
            Current::Outdated | Current::Lost => {
                self.surface.configure(device, &self.config);
                match self.surface.get_current_texture() {
                    Current::Success(texture) | Current::Suboptimal(texture) => Some(texture),
                    _ => None,
                }
            }
            Current::Timeout | Current::Occluded | Current::Validation => None,
        }
    }
}
