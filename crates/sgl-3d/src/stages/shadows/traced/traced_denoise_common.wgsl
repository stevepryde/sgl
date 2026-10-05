// Shared by the ray-traced shadow stage's denoiser passes
// (traced_denoise_tileclassification.wgsl, traced_denoise_filter.wgsl):
// their reading of the tracing pixels' depth and normals for AMD's
// callbacks, from the half-resolution copies the trace writes, as Wicked
// Engine's denoiser reads its half-resolution depth and normals
// (2ff1d9e Postprocess_RTShadow) where AMD's sample reads its full
// G-buffer. Reads `denoise_half_depth` and `denoise_normal`, which each
// pass's bindings declare.
// Whether tracing pixel `p` receives shadows: neither sky nor unlit, which
// the trace records as the sky's depth.
fn traced_denoise_receiver(p:vec2<u32>)->bool {
 return textureLoad(denoise_half_depth,min(p,vec2<u32>(traced.reduced.xy)-1u),0).x<TRACED_SKY_DEPTH;
}
// Tracing pixel `p`'s linear depth, 0 for the sky and unlit pixels.
fn traced_denoise_linear_depth(p:vec2<u32>)->f32 {
 let depth=textureLoad(denoise_half_depth,min(p,vec2<u32>(traced.reduced.xy)-1u),0).x;
 return select(0.,depth,depth<TRACED_SKY_DEPTH);
}
// Tracing pixel `p`'s shading normal.
fn traced_denoise_normal(p:vec2<u32>)->vec3<f32> {
 return textureLoad(denoise_normal,min(p,vec2<u32>(traced.reduced.xy)-1u),0).xyz;
}
