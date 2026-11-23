# sgl-webgpu provenance

`sgl-webgpu` is a port of wgpu's own WebGPU backend to wgpu's custom-backend
interface ([D-75](../../specs/decisions.md)). Upstream is
[gfx-rs/wgpu](https://github.com/gfx-rs/wgpu) at tag `v30.0.2`, commit
`2b38d994b38dd001157d31fd3321111103a8dd80`, MIT OR Apache-2.0; the crate
keeps wgpu's MIT licence in [LICENSE-wgpu.txt](LICENSE-wgpu.txt).

| Upstream | Here |
| --- | --- |
| `wgpu/src/backend/webgpu.rs` | `src/webgpu.rs` |
| `wgpu/src/backend/webgpu/ext_bindings.rs` | `src/webgpu/ext_bindings.rs` |
| `wgpu/src/backend/webgpu/defined_non_null_js_value.rs` | `src/webgpu/defined_non_null_js_value.rs` |
| `wgpu/src/backend/webgpu/webgpu_sys/` | `src/webgpu/webgpu_sys/` |

`webgpu_sys/` is web-sys's generated WebGPU bindings, which wgpu vendors
from wasm-bindgen 0.2.127 (commit `a579ee62b631fd1fa96c7740b52a2952b07c0219`,
MIT OR Apache-2.0, licence in [LICENSE-wasm-bindgen.txt](LICENSE-wasm-bindgen.txt)).
The port is one file, as upstream's is, so an upgrade diffs file for file.
It keeps upstream's rustfmt style (`rustfmt.toml`) and allows clippy's
pedantic group in the ported module for the same reason.

## Departures

Each is forced by the custom-backend interface or by compiling beside wgpu's
own backend, except the upstream defects fixed and the browser features
mapped last.

- **Wrapping.** Upstream converts its objects into wgpu's `Dispatch*` types
  with `.into()` and unwraps them with `as_webgpu()`. Here `custom_types!`
  implements `From<Web*> for Dispatch*` with `Dispatch*::custom`, and `AsWeb`
  unwraps both `Dispatch*` and wgpu's API types with their public
  `as_custom`, so upstream's call sites read `.into()` and `.as_web()`. An
  object from another backend panics with a message naming `sgl-webgpu`.
- **Identity.** wgpu's private `cmp::Identifier` and
  `impl_eq_ord_hash_proxy!` are gone: the custom wrapper compares objects by
  the address of their `Arc`, which a clone shares.
- **`Send`.** `impl_send_sync!`, `MakeSendFuture` and its `unsafe impl Send`
  are gone, replaced by the async `mapped_future`. On wasm32 wgpu's interface
  asks for no `Send` unless `fragile-send-sync-non-atomic-wasm` is on, which
  SGL does not support.
- **wgpu-private helpers.** `Error::from_js` and `CompilationMessage::from_js`
  are the free functions `error_from_js` and `compilation_message_from_js`;
  `util::is_panicking` is `std::thread::panicking`; the request-device error
  is `RequestDeviceError::from_message` with upstream's message.
- **`CreateSurfaceError`.** wgpu gives a custom backend no constructor
  (`wgpu/src/api/surface.rs:507`, field `pub(crate)`). Where upstream returns
  its `CreateSurfaceErrorKind::Web(message)`, the port writes the message to
  the browser console and returns the error wgpu itself makes for an
  unavailable window handle (`create_surface_error`).
- **`MapRangeError`.** wgpu gives a custom backend no constructor either
  (`wgpu/src/api/buffer.rs:895`). Where `getMappedRange` throws, upstream
  returns the error and the port panics with the browser's message.
- **Instance.** `InstanceDescriptor` cannot be cloned, so `sgl_webgpu::instance`
  builds the context from `desc.backends` (`ContextWebGpu::with_backends`),
  which `InstanceInterface::new` also calls.
- **Browser handles.** wgpu's `as_webgpu` methods on `Device`, `Queue`,
  `Texture`, `TextureView` and `Buffer`, and
  `Device::create_texture_from_webgpu_handle`, are `#[cfg(webgpu)]` and reach
  only wgpu's own backend (`wgpu/src/api/device.rs:403` and `:426`). The
  crate's `*_as_webgpu` functions and `create_texture_from_webgpu_handle`
  stand in for them, returning and taking `JsValue` so the vendored binding
  types stay private. Upstream's `DropGuard`, `DropCallback` and
  `WebTexture::drop_guard` are kept as they are;
  `WebDevice::wrap_external_texture` returns the `WebTexture`, which
  `wgpu::Texture::from_custom` wraps, where upstream returns the
  `DispatchTexture`.
- **`todo!`.** Upstream's `BindingType::AccelerationStructure` layout entry
  and `PipelineCacheInterface::get_data` are `todo!()`; WebGPU has neither, and
  the port panics with `unimplemented!`, as upstream does for every other
  ray-tracing and pipeline-cache call.
- **Non-exhaustive enums.** `SurfaceTargetUnsafe`, `ShaderSource` and
  `BindingResource` are matched with a wildcard arm outside wgpu; it panics.
  A `ShaderSource` this crate was built without the feature for lands there.
- **Features.** wgpu's `wgsl` feature is always on, so its `cfg`s are gone;
  `cfg(custom)` on `finish_boxed` is gone; `cfg(naga)` and the `spirv`,
  `glsl` and `naga-ir` paths are this crate's features of those names, which
  enable wgpu's and naga's.
- **wgpu's `webgpu` feature.** wgpu declares
  `BufferMappedRangeInterface::as_uint8array` only under it
  (`wgpu/src/dispatch.rs:648`), so a custom backend compiles only if the
  feature's state is fixed. The crate enables it, implements the method as
  upstream does, and so compiles wgpu's own backend into the build, unused.
- **Bindings.** The vendored bindings are byte for byte wgpu's but for two
  things. Each string enum carries `#[wasm_bindgen(js_name = "Sgl…")]`:
  wasm-bindgen's CLI up to 0.2.129 rejects two string enums of one name in a
  build, and wgpu's own copy is linked beside this one. `ext_bindings.rs`
  imports them as `super::webgpu_sys`.
- **Safe copy-back.** A written mapping's drop copies back with js-sys's safe
  `Uint8Array::copy_from` where upstream calls `set(&Uint8Array::view(..), 0)`
  in an `unsafe` block; it is the same copy. `unsafe` stays only on the trait
  methods wgpu declares unsafe.
- **Occlusion queries (upstream defect).** Upstream's `begin_render_pass`
  drops `RenderPassDescriptor::occlusion_query_set`, so every occlusion query
  failed the browser's validation. The port passes it.
- **Depth slices (upstream defect).** It also drops a colour attachment's
  `depth_slice`, so a pass could not draw into a slice of a 3D texture. The
  port passes it.
- **Fallback adapters (upstream defect).** `request_adapter` drops
  `RequestAdapterOptions::force_fallback_adapter`. The port passes it as
  `GPURequestAdapterOptions.forceFallbackAdapter`.
- **External image colour space (upstream defect).** Upstream leaves
  `CopyExternalImageDestInfo::color_space` commented out, so every
  `copy_external_image_to_texture` encoded into sRGB. The port sets
  `GPUCopyExternalImageDestInfo.colorSpace` (`"srgb"` or `"display-p3"`)
  by reflection, since the vendored bindings have no setter for it.
- **Texture format tiers (SGL addition, [D-82](../../specs/decisions.md)).**
  Upstream maps neither of the browser's `texture-formats-tier1` and
  `texture-formats-tier2`, so a device could not use the storage formats and
  read-write accesses they add. The
  port maps `Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` to
  `"texture-formats-tier2"`, which implies tier1 and
  `"rg11b10ufloat-renderable"` (WebGPU §3.6.1.2), and its adapter's
  `get_texture_format_features` adds what the two tiers give each format
  (`texture_formats_tiers`): read-only and write-only storage on the 8-bit,
  16-bit integer and float, two-component 16-bit, 10-bit and 11/11/10
  formats (§25.19); render attachment, blending, multisampling and resolve
  on the 8-bit snorm formats (§25.19); read-write storage on r8unorm,
  r8uint, r8sint, r16uint, r16sint, r16float and the four-component
  unorm, uint and sint 8-bit, uint, sint and float 16-bit and 32-bit formats
  (§25.20). The source is the WebGPU specification's Editor's Draft
  of 21 November 2026, §25.19, §25.20 and the plain colour format table of
  §26.1.1. Tier1's 16-bit normalized formats are not reported:
  `Features::TEXTURE_FORMAT_16BIT_NORM` would enable them, and wgpu calls
  them filterable where WebGPU makes them `"unfilterable-float"`, so that
  feature stays unmapped. Nor is `"subgroups"` mapped to `Features::SUBGROUP`
  (gfx-rs/wgpu#10265 does so after v30): the browser takes a subgroup
  built-in only after `enable subgroups;`, which naga 30's WGSL front end
  rejects. The browser lane's backend page checks the reported storage
  accesses and usages of every plain colour format but the 16-bit
  normalized ones against what the browser accepts, on a device with every
  feature the adapter reports.

## Upgrading wgpu

A wgpu upgrade ports upstream's changes to the backend in the same change
([D-75](../../specs/decisions.md)):

1. Diff `wgpu/src/backend/webgpu.rs` and `wgpu/src/backend/webgpu/` between
   the old and new tags in gfx-rs/wgpu, and apply the changes here, keeping
   the departures above. A re-vendored `webgpu_sys/` replaces this one whole,
   and the string enums get their `js_name` again.
2. Build for `wasm32-unknown-unknown`: the compiler names every trait method
   of `wgpu/src/dispatch.rs` that changed. Implement each as upstream's new
   version does.
3. Update the tag, commit and wasm-bindgen version above, the revisions in
   `licenses/upstreams.json` (then `bun scripts/refresh-licenses.ts`), and the
   source headers.
4. Run the browser lane (`bun scripts/tasks.ts check-browser`).

When wgpu exposes a WebGPU call this crate added, the crate implements wgpu's
trait method and deletes its own entry point.
