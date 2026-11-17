//! The wait every SGL3D readback ends in (`diagnostics::read_async`, the
//! probe prefilter's): a buffer's map for reading, awaited.
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Poll, Waker};

/// Maps `buffer`, whose copy the caller has submitted, for reading.
/// Natively it first waits on `device`, so the map has completed before it
/// awaits and the future finishes under any executor, a bare `block_on`
/// included, with nothing else driving the device. In a browser it only
/// awaits: the page maps the buffer once control returns to it.
pub(crate) async fn map_read(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Result<(), String> {
    #[derive(Default)]
    struct Map {
        result: Option<Result<(), wgpu::BufferAsyncError>>,
        waker: Option<Waker>,
    }
    let map = Arc::new(Mutex::new(Map::default()));
    let mapped = Arc::clone(&map);
    // Whichever thread's poll runs the callback, the result lands here.
    buffer.map_async(wgpu::MapMode::Read, .., move |result| {
        let mut map = mapped.lock().unwrap_or_else(PoisonError::into_inner);
        map.result = Some(result);
        if let Some(waker) = map.waker.take() {
            waker.wake();
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| format!("poll: {e}"))?;
    #[cfg(target_arch = "wasm32")]
    let _ = device;
    std::future::poll_fn(|context| {
        let mut map = map.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(result) = map.result.take() {
            Poll::Ready(result.map_err(|e| format!("map: {e}")))
        } else {
            map.waker = Some(context.waker().clone());
            Poll::Pending
        }
    })
    .await
}
