// Shared by the ray-traced shadow stage's denoiser passes
// (traced_denoise_tileclassification.wgsl, traced_denoise_filter.wgsl):
// the layout of the denoiser's scratch, which both write and read, and
// their reading of the tracing pixels' depth and normals for AMD's
// callbacks, from the copies the trace writes at the tracing resolution,
// where AMD's sample reads its full G-buffer: the linear depth of each
// tracing pixel's full-resolution pixel 2q, the pixel whose depth Wicked
// Engine's denoiser reads (2ff1d9e rtshadow_denoise_tileclassificationCS.hlsl
// and rtshadow_denoise_filterCS.hlsl, `texture_depth[did * 2]`), and the
// shading normals, as Wicked's denoiser reads its half-resolution normals
// copy (the normal module's traced_denoise_normal). Reads
// `denoise_half_depth`, which each pass's bindings declare.
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
// A texel of the denoiser's scratch (denoise.rs `Targets::scratch`): the
// four denoised slots' mean and variance, a slot a word, the mean its low
// half and the variance its high (pack2x16float), as Wicked keeps a light's
// in R16G16.
struct TracedDenoiseScratch {
 mean:vec4<f32>,
 variance:vec4<f32>,
}
fn traced_denoise_pack(mean:vec4<f32>,variance:vec4<f32>)->vec4<u32> {
 return vec4(
  pack2x16float(vec2(mean.x,variance.x)),
  pack2x16float(vec2(mean.y,variance.y)),
  pack2x16float(vec2(mean.z,variance.z)),
  pack2x16float(vec2(mean.w,variance.w)),
 );
}
fn traced_denoise_unpack(words:vec4<u32>)->TracedDenoiseScratch {
 let x=unpack2x16float(words.x);
 let y=unpack2x16float(words.y);
 let z=unpack2x16float(words.z);
 let w=unpack2x16float(words.w);
 return TracedDenoiseScratch(vec4(x.x,y.x,z.x,w.x),vec4(x.y,y.y,z.y,w.y));
}
// Measurement (#204): a texel of the scalar denoiser's scratch, slot 0's
// mean in its low half and variance in its high, as Wicked keeps a light's
// in R16G16.
fn traced_denoise_pack1(results:vec2<f32>)->u32 {
 return pack2x16float(results);
}
fn traced_denoise_unpack1(word:u32)->vec2<f32> {
 return unpack2x16float(word);
}
