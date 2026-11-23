//! The `sprites` example's cost comparison (`support/sprite_cost.rs`) on
//! the browser's WebGPU: sprite batches against game-built quads through the
//! model path, particles and foliage. Built for wasm32 as a `cdylib`, bound
//! with `wasm-bindgen --target web` and driven in headless Chromium by
//! `bun scripts/tasks.ts measure-browser --sprites [--count N]`, which
//! prints its report. Browser timers are coarsened, so its CPU times are
//! too.
#![cfg(target_arch = "wasm32")]

#[path = "support/sprite_cost.rs"]
mod sprite_cost;

use std::future::Future;
use std::pin::Pin;
use wasm_bindgen::prelude::*;

/// The page's clock, and the queue's completion through its callback.
struct Browser;

impl sprite_cost::Platform for Browser {
    fn now(&self) -> f64 {
        let performance = js_sys::Reflect::get(&js_sys::global(), &"performance".into())
            .expect("the page has a performance clock");
        let now: js_sys::Function = js_sys::Reflect::get(&performance, &"now".into())
            .expect("performance.now")
            .dyn_into()
            .expect("performance.now is a function");
        now.call0(&performance)
            .expect("performance.now")
            .as_f64()
            .expect("a number")
    }

    fn completed<'a>(
        &'a self,
        _: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        let (send, receive) = futures::channel::oneshot::channel();
        queue.on_submitted_work_done(move || {
            let _ = send.send(());
        });
        Box::pin(async move {
            let _ = receive.await;
        })
    }
}

/// The comparison for `count` sprites a case, or the line that failed.
#[wasm_bindgen]
pub async fn measure(count: u32, frames: u32) -> String {
    console_error_panic_hook::set_once();
    measured(count as usize, frames)
        .await
        .unwrap_or_else(|error| format!("FAIL {error}\n"))
}

async fn measured(count: usize, frames: u32) -> Result<String, String> {
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&Default::default())
        .await
        .map_err(|error| format!("no WebGPU adapter: {error}"))?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("sprite cost"),
            required_features: sgl_3d::graphics_device::features(&adapter)
                | (adapter.features() & wgpu::Features::TIMESTAMP_QUERY),
            required_limits: sgl_3d::graphics_device::limits(&adapter),
            ..Default::default()
        })
        .await
        .map_err(|error| format!("request_device: {error}"))?;
    let errors = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    device.on_uncaptured_error(std::sync::Arc::new({
        let errors = errors.clone();
        move |error| errors.lock().unwrap().push(error.to_string())
    }));
    let info = adapter.get_info();
    let report = sprite_cost::report(&Browser, &device, &queue, count, frames).await?;
    if let Some(error) = errors.lock().unwrap().first() {
        return Err(format!("WebGPU error: {error}"));
    }
    Ok(format!("{} ({:?})\n{report}", info.name, info.backend))
}
