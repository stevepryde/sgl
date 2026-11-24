//! SGL's WebGPU backend for wgpu in the browser.
//!
//! A port of wgpu's own WebGPU backend (wgpu v30.0.2,
//! `wgpu/src/backend/webgpu.rs`), plugged in through wgpu's custom-backend
//! interface. A browser game creates its [`wgpu::Instance`] with
//! [`instance`]; the adapter, device, surface and every later call are
//! wgpu's API, except where `README.md` lists how this backend differs from
//! wgpu's own: the browser's objects behind wgpu's come from this crate's
//! `*_as_webgpu` functions and [`create_texture_from_webgpu_handle`], not
//! wgpu's `as_webgpu` methods. `PROVENANCE.md` lists where the port departs
//! from upstream.
//!
//! The crate is empty on every target but `wasm32`.
#![cfg(target_arch = "wasm32")]

extern crate alloc;

mod webgpu;

use wasm_bindgen::{JsCast as _, JsValue};
use webgpu::{
    webgpu_sys, ContextWebGpu, WebBuffer, WebDevice, WebQueue, WebTexture, WebTextureView,
};

pub use webgpu::DropCallback;

/// A wgpu instance on the browser's WebGPU, through this backend.
///
/// Call it where a native build calls `wgpu::Instance::new` or
/// `wgpu::Instance::default`. Only `desc.backends` is read: an instance whose
/// backends exclude [`wgpu::Backends::BROWSER_WEBGPU`] finds no adapter, as
/// wgpu's own web backend behaves.
///
/// # Panics
///
/// Outside the main thread and dedicated workers, where the browser exposes
/// no `navigator.gpu`.
#[must_use]
pub fn instance(desc: &wgpu::InstanceDescriptor) -> wgpu::Instance {
    wgpu::Instance::from_custom(ContextWebGpu::with_backends(desc.backends))
}

/// The browser's `GPUDevice` behind `device`, or `None` for a device from
/// another backend. wgpu's own backend offers it as `Device::as_webgpu`.
#[must_use]
pub fn device_as_webgpu(device: &wgpu::Device) -> Option<JsValue> {
    device
        .as_custom::<WebDevice>()
        .map(|device| device.inner.clone().into())
}

/// The browser's `GPUQueue` behind `queue`, or `None` for a queue from
/// another backend. wgpu's own backend offers it as `Queue::as_webgpu`.
#[must_use]
pub fn queue_as_webgpu(queue: &wgpu::Queue) -> Option<JsValue> {
    queue
        .as_custom::<WebQueue>()
        .map(|queue| queue.inner.clone().into())
}

/// The browser's `GPUTexture` behind `texture`, or `None` for a texture from
/// another backend. wgpu's own backend offers it as `Texture::as_webgpu`.
#[must_use]
pub fn texture_as_webgpu(texture: &wgpu::Texture) -> Option<JsValue> {
    texture
        .as_custom::<WebTexture>()
        .map(|texture| texture.inner.clone().into())
}

/// The browser's `GPUTextureView` behind `view`, or `None` for a view from
/// another backend. wgpu's own backend offers it as `TextureView::as_webgpu`.
#[must_use]
pub fn texture_view_as_webgpu(view: &wgpu::TextureView) -> Option<JsValue> {
    view.as_custom::<WebTextureView>()
        .map(|view| view.inner.clone().into())
}

/// The browser's `GPUBuffer` behind `buffer`, or `None` for a buffer from
/// another backend. wgpu's own backend offers it as `Buffer::as_webgpu`.
#[must_use]
pub fn buffer_as_webgpu(buffer: &wgpu::Buffer) -> Option<JsValue> {
    buffer
        .as_custom::<WebBuffer>()
        .map(|buffer| buffer.inner.clone().into())
}

/// Creates a [`wgpu::RenderPipeline`] with the browser's
/// `GPUDevice.createRenderPipelineAsync`, which compiles it without stalling
/// the page, resolving once it is ready to be used.
///
/// `desc` is read before the function returns. A validation or internal
/// error is the future's `Err` (`wgpu::Error::Validation` or
/// `wgpu::Error::Internal`) and reaches neither the device's error scopes
/// nor its uncaptured error handler. This is the call gfx-rs/wgpu#10438 adds
/// as `wgpu::Device::create_render_pipeline_async`.
///
/// # Panics
///
/// If `device` is from another backend.
pub fn create_render_pipeline_async(
    device: &wgpu::Device,
    desc: &wgpu::RenderPipelineDescriptor<'_>,
) -> impl Future<Output = Result<wgpu::RenderPipeline, wgpu::Error>> + 'static {
    webgpu::create_render_pipeline_async(device, desc)
}

/// Creates a [`wgpu::ComputePipeline`] with the browser's
/// `GPUDevice.createComputePipelineAsync`, as
/// [`create_render_pipeline_async`] creates a render pipeline.
///
/// # Panics
///
/// If `device` is from another backend.
pub fn create_compute_pipeline_async(
    device: &wgpu::Device,
    desc: &wgpu::ComputePipelineDescriptor<'_>,
) -> impl Future<Output = Result<wgpu::ComputePipeline, wgpu::Error>> + 'static {
    webgpu::create_compute_pipeline_async(device, desc)
}

/// A [`wgpu::Texture`] over a `GPUTexture` the program created or received
/// in the browser, as wgpu's own backend offers with
/// `Device::create_texture_from_webgpu_handle`.
///
/// The texture is external: wgpu never destroys it, and
/// [`wgpu::Texture::destroy`] does nothing. If `drop_callback` is `Some`, it
/// runs when wgpu drops its last reference to the texture, so the program
/// can destroy or reuse the handle then.
///
/// The caller must guarantee, as for wgpu's own:
///
/// 1. `texture` is a `GPUTexture` created by the same `GPUDevice` that
///    `device` wraps. Like the port's canvas context, it is not checked with
///    `instanceof`, which fails across realms.
/// 2. `desc.format`, `desc.size`, `desc.usage`, `desc.dimension`,
///    `desc.mip_level_count` and `desc.sample_count` match the
///    `GPUTexture`'s. wgpu stores them as given and returns them from
///    [`wgpu::Texture::size`], [`wgpu::Texture::format`] and the rest without
///    re-checking the handle; a mismatch yields wrong metadata and,
///    downstream, `GPUValidationError`s, never memory unsafety.
/// 3. The `GPUTexture` stays alive while wgpu may use it (until any submitted
///    command buffer that references it has finished). With a
///    `drop_callback`, keeping it alive until the callback runs is enough.
///
/// # Panics
///
/// If `device` is from another backend.
#[must_use]
pub fn create_texture_from_webgpu_handle(
    device: &wgpu::Device,
    texture: JsValue,
    desc: &wgpu::TextureDescriptor<'_>,
    drop_callback: Option<DropCallback>,
) -> wgpu::Texture {
    let device = device
        .as_custom::<WebDevice>()
        .expect("sgl-webgpu was given a device from another wgpu backend");
    let texture = texture.unchecked_into::<webgpu_sys::GpuTexture>();
    wgpu::Texture::from_custom(device.wrap_external_texture(texture, drop_callback), desc)
}
