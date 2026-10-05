//! Device limits and features for the 3D renderer, native or WebGPU.

/// Renderer limits sized to the selected adapter, the acceleration-structure
/// limits among them, which `Limits::default()` leaves at zero and hardware
/// ray tracing needs (`ray_tracing_features`). An adapter may report them
/// whether or not it has the feature (Metal reports its maximums on every
/// GPU); they take effect only on a device requested with it.
pub fn limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    let limits = adapter.limits();
    wgpu::Limits {
        max_storage_buffer_binding_size: limits.max_storage_buffer_binding_size,
        max_buffer_size: limits.max_buffer_size,
        max_color_attachment_bytes_per_sample: limits.max_color_attachment_bytes_per_sample,
        max_sampled_textures_per_shader_stage: limits.max_sampled_textures_per_shader_stage,
        max_storage_textures_per_shader_stage: limits.max_storage_textures_per_shader_stage,
        max_storage_buffers_per_shader_stage: limits.max_storage_buffers_per_shader_stage,
        max_texture_array_layers: limits.max_texture_array_layers,
        max_texture_dimension_2d: limits.max_texture_dimension_2d,
        max_texture_dimension_3d: limits.max_texture_dimension_3d,
        ..wgpu::Limits::default().using_acceleration_structure_values(limits)
    }
}

/// The optional features SGL3D uses where `adapter` has them:
/// `DEPTH_CLIP_CONTROL`, with which directional shadow casters draw with
/// unclipped depth instead of emulating it in their shader, and
/// `RG11B10UFLOAT_RENDERABLE`, with which bloom's mip chain is
/// `Rg11b10Ufloat` instead of RGBA16F. Hardware ray tracing's feature is
/// apart (`ray_tracing_features`).
pub fn features(adapter: &wgpu::Adapter) -> wgpu::Features {
    adapter.features()
        & (wgpu::Features::DEPTH_CLIP_CONTROL | wgpu::Features::RG11B10UFLOAT_RENDERABLE)
}

/// The features to request for FSR2 on `adapter`: its wgpu backend's, and the
/// FP16 permutations' when the adapter has every required one; otherwise
/// none, and TAA replaces FSR2. AMD recommends the FP16 permutations wherever
/// they are supported (docs 484–499).
pub fn fsr2_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    crate::stages::antialiasing::fsr2::features(adapter)
}

/// The feature to request for hardware ray tracing on `adapter`: wgpu's
/// `EXPERIMENTAL_RAY_QUERY` where the adapter has it, otherwise none. A game
/// that requests it also opts in with `Settings::hardware_ray_tracing`,
/// which is off by default. wgpu
/// refuses an experimental feature unless the device descriptor's
/// `experimental_features` is `ExperimentalFeatures::enabled()`, an `unsafe`
/// token by which the game accepts that wgpu's ray tracing may still have
/// bugs:
///
/// ```no_run
/// # async fn device(adapter: wgpu::Adapter) -> Result<(), wgpu::RequestDeviceError> {
/// let (device, queue) = adapter
///     .request_device(&wgpu::DeviceDescriptor {
///         required_features: sgl_3d::graphics_device::features(&adapter)
///             | sgl_3d::graphics_device::ray_tracing_features(&adapter),
///         required_limits: sgl_3d::graphics_device::limits(&adapter),
///         // SAFETY: the game accepts wgpu's experimental ray queries.
///         experimental_features: unsafe { wgpu::ExperimentalFeatures::enabled() },
///         ..Default::default()
///     })
///     .await?;
/// # Ok(()) }
/// ```
///
/// Adapters that have it: Metal from macOS 15 on a GPU that ray traces from
/// render stages (Apple silicon; in hardware from M3), Vulkan with ray
/// queries, and DX12 at ray-tracing tier 1.1, which needs DXC (wgpu's
/// default `Dx12Compiler::Auto` takes a statically linked DXC where the
/// game enables wgpu's `static-dxc`, else `dxcompiler.dll` beside the
/// executable). No browser's WebGPU has it, nor Vulkan on a Mac (MoltenVK).
pub fn ray_tracing_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    adapter.features() & wgpu::Features::EXPERIMENTAL_RAY_QUERY
}
