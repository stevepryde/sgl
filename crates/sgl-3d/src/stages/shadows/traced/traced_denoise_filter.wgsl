// The ray-traced shadow stage's denoiser, its filter passes (the
// architecture's Ray-traced shadows): AMD's edge-stopping filter over the
// slots it denoises at step 1, 2 and 4 (`filter_pass` 0, 1 and 2), one
// dispatch a pass for all four slots, the slot each group's z. The first
// two keep their result for the next, and the first's for the next frame's
// tile classification; the last recovers some of the contrast the
// filtering took and writes the denoised visibility. Group 0 is the
// pass's own.
//
// Ports Wicked Engine 2ff1d9e's rtshadow_denoise_filterCS.hlsl (MIT,
// src/LICENSE-wicked.txt): the callbacks AMD's
// ffx_denoiser_shadows_filter.h takes, its depth similarity's sigma of 1,
// and the last pass's contrast recovery (79–82). Changed at the port
// boundary: one dispatch for the four slots, the slot the group's z; the
// pass a pipeline constant, where Wicked pushes it; depth and normals the
// G-buffer's at the full-resolution pixel of each tracing pixel; the tile's
// metadata read once by the group through workgroupUniformLoad; and the
// denoised visibility written to a layer a slot, where Wicked writes a
// channel a light.
override filter_pass:u32;
@group(0) @binding(0) var denoise_depth:texture_depth_2d;
@group(0) @binding(1) var denoise_normal:texture_2d<f32>;
@group(0) @binding(2) var denoise_metadata:texture_2d_array<u32>;
@group(0) @binding(3) var denoise_input:texture_2d_array<u32>;
@group(0) @binding(4) var<uniform> traced:TracedParams;
@group(0) @binding(5) var denoise_history:texture_storage_2d_array<r32uint,write>;
@group(0) @binding(6) var denoise_output:texture_storage_2d_array<r32float,write>;

// The slot the invocation's group denoises.
var<private> rtshadow_denoise_lightindex:u32;
// The group's tile metadata, which every invocation reads alike.
var<workgroup> denoise_tile_meta_data:u32;

fn FFX_DNSR_Shadows_GetBufferDimensions()->vec2<u32> {
 return vec2<u32>(traced.reduced.xy);
}
fn FFX_DNSR_Shadows_GetInvBufferDimensions()->vec2<f32> {
 return traced.reduced.zw;
}
fn FFX_DNSR_Shadows_GetProjectionInverse()->mat4x4<f32> {
 return traced.inverse_projection;
}

fn FFX_DNSR_Shadows_GetDepthSimilaritySigma()->f32 {
 return 1.;
}

fn FFX_DNSR_Shadows_ReadDepth(p:vec2<i32>)->f32 {
 return textureLoad(denoise_depth,traced_full_pixel(vec2<u32>(p)),0);
}
fn FFX_DNSR_Shadows_ReadNormals(p:vec2<i32>)->vec3<f32> {
 return gbuffer_base_normal(textureLoad(denoise_normal,traced_full_pixel(vec2<u32>(p)),0));
}

fn FFX_DNSR_Shadows_IsShadowReciever(did:vec2<u32>)->bool {
 let depth=FFX_DNSR_Shadows_ReadDepth(vec2<i32>(did));
 return (depth>0.) && (depth<1.);
}

fn FFX_DNSR_Shadows_ReadInput(p:vec2<i32>)->vec2<f32> {
 return unpack2x16float(textureLoad(denoise_input,p,rtshadow_denoise_lightindex,0).x);
}

fn FFX_DNSR_Shadows_ReadTileMetaData(p:u32)->u32 {
 let groups=FFX_DNSR_Shadows_RoundedDivide(FFX_DNSR_Shadows_GetBufferDimensions().x,8u);
 denoise_tile_meta_data=textureLoad(denoise_metadata,vec2(p%groups,p/groups),rtshadow_denoise_lightindex,0).x;
 workgroupBarrier();
 return workgroupUniformLoad(&denoise_tile_meta_data);
}

@compute @workgroup_size(8,8) fn traced_denoise_filter(@builtin(workgroup_id) gid:vec3<u32>,@builtin(local_invocation_id) gtid:vec3<u32>,@builtin(global_invocation_id) did:vec3<u32>) {
 rtshadow_denoise_lightindex=gid.z;
 let step_size=1u<<filter_pass;
 let filtered=FFX_DNSR_Shadows_FilterSoftShadowsPass(gid.xy,gtid.xy,did.xy,filter_pass,step_size);
 if !filtered.write_results {
  return;
 }
 if filter_pass<2u {
  textureStore(denoise_history,did.xy,rtshadow_denoise_lightindex,vec4(pack2x16float(filtered.results)));
 } else {
  // final pass:
  // Recover some of the contrast lost during denoising
  let shadow_remap=max(1.2-filtered.results.y,1.);
  let mean=saturate(pow(abs(filtered.results.x),shadow_remap));
  textureStore(denoise_output,did.xy,rtshadow_denoise_lightindex,vec4(mean));
 }
}
