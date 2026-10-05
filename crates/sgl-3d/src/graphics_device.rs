//! Device limits and features for the 3D renderer, native or WebGPU.

/// Renderer limits sized to the selected adapter.
pub fn limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    wgpu::Limits {
        max_storage_buffer_binding_size: adapter.limits().max_storage_buffer_binding_size,
        max_buffer_size: adapter.limits().max_buffer_size,
        max_color_attachment_bytes_per_sample: adapter
            .limits()
            .max_color_attachment_bytes_per_sample,
        max_sampled_textures_per_shader_stage: adapter
            .limits()
            .max_sampled_textures_per_shader_stage,
        max_storage_textures_per_shader_stage: adapter
            .limits()
            .max_storage_textures_per_shader_stage,
        max_storage_buffers_per_shader_stage: adapter.limits().max_storage_buffers_per_shader_stage,
        max_texture_array_layers: adapter.limits().max_texture_array_layers,
        max_texture_dimension_2d: adapter.limits().max_texture_dimension_2d,
        max_texture_dimension_3d: adapter.limits().max_texture_dimension_3d,
        ..Default::default()
    }
}

/// The optional features SGL3D uses where `adapter` has them:
/// `DEPTH_CLIP_CONTROL`, with which directional shadow casters draw with
/// unclipped depth instead of emulating it in their shader, and
/// `RG11B10UFLOAT_RENDERABLE`, with which bloom's mip chain is
/// `Rg11b10Ufloat` instead of RGBA16F.
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
