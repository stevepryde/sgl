# sgl-webgpu

SGL's WebGPU backend for wgpu in the browser: a port of wgpu's own, plugged
in through wgpu's custom-backend interface, so SGL can take a WebGPU call
the day a browser ships it instead of waiting for wgpu. The crate is empty
on every target but `wasm32`.

Depend on it for `wasm32`, with the same version requirement as the game's
other SGL crates (it first ships in 0.10.0):

```toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
sgl-webgpu = "0.14.0"
```

and create the wgpu instance with it where a native build calls
`wgpu::Instance::default()`:

```rust
let instance = sgl_webgpu::instance(&wgpu::InstanceDescriptor::new_without_display_handle());
```

Everything after is wgpu's API: `create_surface` with a canvas, an
offscreen canvas or a winit window, `request_adapter`, `request_device`, and
every device, queue and encoder call. The types stay `wgpu::*`, so any crate
that takes a wgpu device works with it. `sgl-2d` creates its own instance
here in the browser.

What WebGPU lacks (ray-tracing acceleration structures, mesh pipelines,
pipeline caches, passthrough shaders other than WGSL) behaves as wgpu's own
web backend does. SPIR-V, GLSL and naga IR shader sources need this crate's
`spirv`, `glsl` and `naga-ir` features, which translate them to WGSL.

## Differs from wgpu's own backend

- `create_render_pipeline_async` and `create_compute_pipeline_async` create
  a pipeline with the browser's asynchronous call, which compiles it without
  stalling the page; a failure is the future's `Err`, not a device error.
  wgpu has no such call yet (gfx-rs/wgpu#3794).

- The browser's objects come from this crate: `device_as_webgpu`,
  `queue_as_webgpu`, `texture_as_webgpu`, `texture_view_as_webgpu` and
  `buffer_as_webgpu` return the `GPUDevice`, `GPUQueue`, `GPUTexture`,
  `GPUTextureView` or `GPUBuffer` as a `JsValue`, and
  `create_texture_from_webgpu_handle` wraps a `GPUTexture`. wgpu's
  `as_webgpu` methods and `Device::create_texture_from_webgpu_handle` reach
  only wgpu's own backend.
- `Buffer`'s `get_mapped_range` and `get_mapped_range_mut` panic with the
  browser's message where wgpu's backend returns `Err`, including on a
  buffer that is not mapped: wgpu gives a custom backend no way to build that
  error.
- A failed `create_surface` (a canvas that already holds another context,
  for example) returns wgpu's "handle unavailable" error, and the browser's
  reason goes to the console.
- wgpu's `fragile-send-sync-non-atomic-wasm` feature is not supported.

[PROVENANCE.md](PROVENANCE.md) lists where the port departs from upstream and
how to port a wgpu upgrade.
