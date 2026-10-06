// The denoiser passes' reading of the tracing pixels' shading normals,
// which the trace writes (traced_normal_store.wgsl) at the binding each
// pass's group 0 gives them.
@group(0) @binding(1) var denoise_normal:texture_2d<f32>;
// Tracing pixel `p`'s shading normal.
fn traced_denoise_normal(p:vec2<u32>)->vec3<f32> {
 return textureLoad(denoise_normal,min(p,vec2<u32>(traced.reduced.xy)-1u),0).xyz;
}
