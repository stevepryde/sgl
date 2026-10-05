// Shared by the ray-traced shadow stage's denoiser passes
// (traced_denoise_tileclassification.wgsl, traced_denoise_filter.wgsl):
// their reading of the G-buffer's depth for AMD's ReadDepth and
// IsShadowReciever callbacks. Reads `denoise_depth` and
// `denoise_half_depth`, which each pass's bindings declare.
// The G-buffer's depth at tracing pixel `p` (its full-resolution pixel's),
// the sky's (0) where the trace found nothing lit, as the trace records an
// unlit pixel, so that an unlit pixel is no receiver.
fn traced_denoise_depth(p:vec2<u32>)->f32 {
 let texel=min(p,vec2<u32>(traced.reduced.xy)-1u);
 if textureLoad(denoise_half_depth,texel,0).x>=TRACED_SKY_DEPTH {
  return 0.;
 }
 return textureLoad(denoise_depth,traced_full_pixel(p),0);
}
// Whether tracing pixel `p` receives shadows: neither sky nor unlit.
fn traced_denoise_receiver(p:vec2<u32>)->bool {
 let depth=traced_denoise_depth(p);
 return (depth>0.) && (depth<1.);
}
