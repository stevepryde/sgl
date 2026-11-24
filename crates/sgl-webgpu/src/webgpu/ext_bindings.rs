// Ported from wgpu v30.0.2, wgpu/src/backend/webgpu/ext_bindings.rs
// (https://github.com/gfx-rs/wgpu, MIT OR Apache-2.0, copyright the gfx-rs
// developers); see PROVENANCE.md.

//! Extension bindings for WebGPU.
//!
//! These contain ideomatic Rust extension traits for various parts of the WebGPU
//! bindings that are missing, need to be improved, or otherwise need to be different
//! from the generated web_sys bindings.

use super::webgpu_sys;
use wasm_bindgen::prelude::*;

/// Extension trait for [`web_sys::Navigator`] and [`web_sys::WorkerNavigator`] to
/// access the `gpu` property.
pub trait NavigatorGpu {
    /// Get the `gpu` property.
    ///
    /// This is intentionally a free function, to prevent overload conflicts with
    /// the method if it is enabled in web-sys itself.
    fn gpu(navigator: &Self) -> webgpu_sys::Gpu;
}

// --- Bindings for `Navigator` ---
#[wasm_bindgen]
extern "C" {
    /// Create a fake class which we tell wasm-bindgen has access to the `gpu` property.
    #[wasm_bindgen]
    type NavigatorWithGpu;

    #[wasm_bindgen(method, getter)]
    fn gpu(ext: &NavigatorWithGpu) -> webgpu_sys::Gpu;
}

// --- Bindings for `GPUPipelineError` ---
// SGL: the vendored bindings (wasm-bindgen 0.2.127) have no `GPUPipelineError`,
// which rejects `createRenderPipelineAsync` and `createComputePipelineAsync`
// (PROVENANCE.md). `reason` is read as a string, not as the generated
// `GPUPipelineErrorReason` enum, whose name wgpu's own bindings would share.
#[wasm_bindgen]
extern "C" {
    /// A `GPUPipelineError`, a `DOMException` with a `reason`.
    #[wasm_bindgen(js_name = GPUPipelineError, extends = js_sys::Object)]
    #[derive(Debug, Clone)]
    pub type GpuPipelineError;

    /// `"validation"` or `"internal"`.
    #[wasm_bindgen(method, getter)]
    pub fn reason(this: &GpuPipelineError) -> String;

    /// The `DOMException`'s message.
    #[wasm_bindgen(method, getter)]
    pub fn message(this: &GpuPipelineError) -> String;
}

impl NavigatorGpu for web_sys::Navigator {
    fn gpu(navigator: &Self) -> webgpu_sys::Gpu {
        // Must be an unchecked ref as this class does not exist at runtime.
        let extension: &NavigatorWithGpu = navigator.unchecked_ref();
        extension.gpu()
    }
}

impl NavigatorGpu for web_sys::WorkerNavigator {
    fn gpu(navigator: &Self) -> webgpu_sys::Gpu {
        // Must be an unchecked ref as this class does not exist at runtime.
        let extension: &NavigatorWithGpu = navigator.unchecked_ref();
        extension.gpu()
    }
}
