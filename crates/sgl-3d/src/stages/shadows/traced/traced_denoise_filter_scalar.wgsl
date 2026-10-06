// Measurement (#204): the ray-traced shadow stage's denoiser, its filter
// passes over slot 0 alone, through AMD's scalar filter
// (ffx_denoiser_shadows_filter_scalar.wgsl), as Wicked Engine 2ff1d9e's
// rtshadow_denoise_filterCS.hlsl (MIT, src/LICENSE-wicked.txt) runs it for
// one light; the callbacks are traced_denoise_filter.wgsl's for slot 0,
// the scratch a word a texel (traced_denoise_pack1), the denoised byte 0
// of the tracing pixel's word. `filter_step` is the pass's step,
// `filter_final` whether it writes the denoised visibility, and
// `filter_apron` the group's apron (upstream's 4, or the step). Group 0 is
// the pass's own; the normals' binding is the normal module's.
override filter_step:u32;
override filter_final:bool;
override filter_write_cleared:bool;
override filter_apron:i32;
@group(0) @binding(2) var denoise_metadata:texture_2d<u32>;
@group(0) @binding(3) var denoise_input:texture_2d<u32>;
@group(0) @binding(4) var<uniform> traced:TracedParams;
@group(0) @binding(5) var denoise_history:texture_storage_2d<r32uint,write>;
@group(0) @binding(6) var<storage,read_write> denoise_output:array<u32>;
@group(0) @binding(7) var denoise_half_depth:texture_2d<f32>;

var<workgroup> denoise_tile_meta_data:u32;

fn FFX_DNSR_Shadows_GetBufferDimensions()->vec2<u32> {
 return vec2<u32>(traced.reduced.xy);
}
fn FFX_DNSR_Shadows_GetInvBufferDimensions()->vec2<f32> {
 return traced.reduced.zw;
}
fn FFX_DNSR_Shadows_GetDepthSimilaritySigma()->f32 {
 return 1.;
}
fn FFX_DNSR_Shadows_GetApron()->i32 {
 return filter_apron;
}
fn FFX_DNSR_Shadows_ReadDepth(p:vec2<i32>)->f32 {
 return traced_denoise_linear_depth(vec2<u32>(p));
}
fn FFX_DNSR_Shadows_ReadNormals(p:vec2<i32>)->vec3<f32> {
 return traced_denoise_normal(vec2<u32>(p));
}
fn FFX_DNSR_Shadows_IsShadowReciever(did:vec2<u32>)->bool {
 return traced_denoise_receiver(did);
}
fn FFX_DNSR_Shadows_ReadInput(p:vec2<i32>)->u32 {
 return textureLoad(denoise_input,p,0).x;
}
fn FFX_DNSR_Shadows_ReadTileMetaData(p:u32)->u32 {
 let groups=FFX_DNSR_Shadows_RoundedDivide(FFX_DNSR_Shadows_GetBufferDimensions().x,8u);
 denoise_tile_meta_data=textureLoad(denoise_metadata,vec2(p%groups,p/groups),0).x;
 workgroupBarrier();
 return workgroupUniformLoad(&denoise_tile_meta_data);
}

@compute @workgroup_size(8,8) fn traced_denoise_filter(@builtin(workgroup_id) gid:vec3<u32>,@builtin(local_invocation_id) gtid:vec3<u32>,@builtin(global_invocation_id) did:vec3<u32>) {
 let filtered=FFX_DNSR_Shadows_FilterSoftShadowsPass(gid.xy,gtid.xy,did.xy,filter_write_cleared,filter_step);
 if !filtered.write_results {
  return;
 }
 if !filter_final {
  textureStore(denoise_history,did.xy,vec4(traced_denoise_pack1(filtered.results),0u,0u,0u));
 } else {
  // final pass:
  // Recover some of the contrast lost during denoising
  let shadow_remap=max(1.2-filtered.results.y,1.);
  let mean=saturate(pow(abs(filtered.results.x),shadow_remap));
  let size=vec2<u32>(traced.reduced.xy);
  if all(did.xy<size) {
   denoise_output[did.y*size.x+did.x]=traced_pack(vec4(mean,0.,0.,0.));
  }
 }
}
