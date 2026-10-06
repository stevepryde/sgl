// Measurement (#204): the ray-traced shadow stage's denoiser, its first
// pass, over slot 0 alone, through AMD's scalar tile classification
// (ffx_denoiser_shadows_tileclassification_scalar.wgsl), as Wicked Engine
// 2ff1d9e's rtshadow_denoise_tileclassificationCS.hlsl (MIT,
// src/LICENSE-wicked.txt) runs it for one light. The callbacks are
// traced_denoise_tileclassification.wgsl's for slot 0: the tile masks'
// first word, the slot's restart bit, one moments layer, and the scratch
// a word a texel (traced_denoise_pack1). Group 0 is the pass's own; the
// normals' binding is the normal module's.
@group(0) @binding(0) var denoise_depth:texture_depth_2d;
@group(0) @binding(2) var denoise_tiles:texture_2d<u32>;
@group(0) @binding(3) var denoise_moments_previous:texture_2d<f32>;
@group(0) @binding(4) var denoise_history:texture_2d<u32>;
@group(0) @binding(5) var denoise_previous_depth:texture_2d<f32>;
@group(0) @binding(6) var denoise_motion:texture_2d<f32>;
@group(0) @binding(7) var<uniform> traced:TracedParams;
@group(0) @binding(8) var<uniform> shadow_mask_slots:ShadowMaskSlots;
@group(0) @binding(9) var denoise_metadata:texture_storage_2d<r32uint,write>;
@group(0) @binding(10) var denoise_reprojection:texture_storage_2d<r32uint,write>;
@group(0) @binding(11) var denoise_moments:texture_storage_2d<rgba16float,write>;
@group(0) @binding(12) var denoise_half_depth:texture_2d<f32>;

// Whether this is slot 0's first frame: the stage's history restarted, or
// its light changed.
fn FFX_DNSR_Shadows_IsFirstFrame()->bool {
 return traced.frame==0u || (shadow_mask_slots.restart&1u)!=0u;
}
fn FFX_DNSR_Shadows_GetBufferDimensions()->vec2<u32> {
 return vec2<u32>(traced.reduced.xy);
}
fn FFX_DNSR_Shadows_GetInvBufferDimensions()->vec2<f32> {
 return traced.reduced.zw;
}
fn FFX_DNSR_Shadows_GetEye()->vec3<f32> {
 return traced.eye.xyz;
}
fn FFX_DNSR_Shadows_GetViewProjectionInverse()->mat4x4<f32> {
 return traced.inverse_view_projection;
}
fn FFX_DNSR_Shadows_GetPreviousLinearDepth(world_position:vec3<f32>)->f32 {
 return -(traced.previous_view*vec4(world_position,1.)).z;
}
fn FFX_DNSR_Shadows_ReadPreviousLinearDepth(idx:vec2<i32>)->f32 {
 let last=vec2<i32>(FFX_DNSR_Shadows_GetBufferDimensions())-1;
 return textureLoad(denoise_previous_depth,clamp(idx,vec2(0),last),0).x;
}
fn FFX_DNSR_Shadows_ReadDepth(did:vec2<u32>)->f32 {
 if !traced_denoise_receiver(did) {
  return 0.;
 }
 return textureLoad(denoise_depth,traced_full_pixel(did),0);
}
fn FFX_DNSR_Shadows_ReadNormals(did:vec2<u32>)->vec3<f32> {
 return traced_denoise_normal(did);
}
// Slot 0's mask of an 8×4 tile.
fn FFX_DNSR_Shadows_ReadRaytracedShadowMask(linear_tile_index:u32)->u32 {
 let tiles=FFX_DNSR_Shadows_RoundedDivide(FFX_DNSR_Shadows_GetBufferDimensions().x,8u);
 return textureLoad(denoise_tiles,vec2(linear_tile_index%tiles,linear_tile_index/tiles),0).x;
}
fn FFX_DNSR_Shadows_ReadPreviousMomentsBuffer(history_pos:vec2<i32>)->vec3<f32> {
 if FFX_DNSR_Shadows_IsFirstFrame() {
  return vec3(0.);
 }
 let last=vec2<i32>(FFX_DNSR_Shadows_GetBufferDimensions())-1;
 return textureLoad(denoise_moments_previous,clamp(history_pos,vec2(0),last),0).xyz;
}
fn traced_denoise_history(texel:vec2<i32>)->f32 {
 let last=vec2<i32>(FFX_DNSR_Shadows_GetBufferDimensions())-1;
 return traced_denoise_unpack1(textureLoad(denoise_history,clamp(texel,vec2(0),last),0).x).x;
}
fn FFX_DNSR_Shadows_ReadHistory(history_uv:vec2<f32>)->f32 {
 let dims=vec2<f32>(FFX_DNSR_Shadows_GetBufferDimensions());
 let position=history_uv*dims-.5;
 let base=vec2<i32>(floor(position));
 let fraction=position-floor(position);
 let top=mix(traced_denoise_history(base),traced_denoise_history(base+vec2(1,0)),fraction.x);
 let bottom=mix(traced_denoise_history(base+vec2(0,1)),traced_denoise_history(base+vec2(1,1)),fraction.x);
 return mix(top,bottom,fraction.y);
}
fn FFX_DNSR_Shadows_ReadVelocity(did:vec2<u32>)->vec2<f32> {
 return textureLoad(denoise_motion,traced_full_pixel(did),0).xy;
}

fn FFX_DNSR_Shadows_WriteReprojectionResults(did:vec2<u32>,results:vec2<f32>) {
 textureStore(denoise_reprojection,did,vec4(traced_denoise_pack1(results),0u,0u,0u));
}
fn FFX_DNSR_Shadows_WriteMoments(did:vec2<u32>,moments:vec3<f32>) {
 textureStore(denoise_moments,did,vec4(moments,0.));
}
fn FFX_DNSR_Shadows_WriteMetadata(idx:u32,mask:u32) {
 let groups=FFX_DNSR_Shadows_RoundedDivide(FFX_DNSR_Shadows_GetBufferDimensions().x,8u);
 textureStore(denoise_metadata,vec2(idx%groups,idx/groups),vec4(mask,0u,0u,0u));
}

fn FFX_DNSR_Shadows_IsShadowReciever(did:vec2<u32>)->bool {
 return traced_denoise_receiver(did);
}

@compute @workgroup_size(64) fn traced_denoise_tile_classification(@builtin(workgroup_id) gid:vec3<u32>,@builtin(local_invocation_index) group_index:u32) {
 FFX_DNSR_Shadows_TileClassification(group_index,gid.xy);
}
