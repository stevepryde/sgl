// The ray-traced shadow stage's denoiser, its first pass (the
// architecture's Ray-traced shadows): AMD's tile classification over the
// slots it denoises, one dispatch for all four, the slot each group's z.
// Group 0 is the pass's own.
//
// Ports Wicked Engine 2ff1d9e's rtshadow_denoise_tileclassificationCS.hlsl
// (MIT, src/LICENSE-wicked.txt): the callbacks AMD's
// ffx_denoiser_shadows_tileclassification.h takes, over the stage's
// targets, under INVERTED_DEPTH_RANGE. Changed at the port boundary: one
// dispatch for the four slots, the slot the group's z, where Wicked
// dispatches each with its index pushed (rtshadow_denoise_lightindex);
// depth and normals are the G-buffer's at the full-resolution pixel of
// each tracing pixel (Wicked's depth reads at did * 2, its normals from its
// half-resolution copy); the previous depth is the stage's own, linear;
// a pixel the trace found nothing lit at reads as the sky, as the trace
// records it; a slot's first frame is the stage's history restarting or the slot's
// light changing (ShadowMaskSlots.restart), whose previous moments read as
// zero, as Wicked clears its resources on its first frame; the history is
// sampled bilinearly from two packed halves (pack2x16float), where Wicked
// keeps it in R16G16 and samples it through a linear sampler; and the
// moments are kept in RGBA16F, where Wicked keeps R11G11B10.
@group(0) @binding(0) var denoise_depth:texture_depth_2d;
@group(0) @binding(1) var denoise_normal:texture_2d<f32>;
@group(0) @binding(2) var denoise_tiles:texture_2d<u32>;
@group(0) @binding(3) var denoise_moments_previous:texture_2d_array<f32>;
@group(0) @binding(4) var denoise_history:texture_2d_array<u32>;
@group(0) @binding(5) var denoise_previous_depth:texture_2d<f32>;
@group(0) @binding(6) var denoise_motion:texture_2d<f32>;
@group(0) @binding(7) var<uniform> traced:TracedParams;
@group(0) @binding(8) var<uniform> shadow_mask_slots:ShadowMaskSlots;
@group(0) @binding(9) var denoise_metadata:texture_storage_2d_array<r32uint,write>;
@group(0) @binding(10) var denoise_reprojection:texture_storage_2d_array<r32uint,write>;
@group(0) @binding(11) var denoise_moments:texture_storage_2d_array<rgba16float,write>;
// The tracing resolution's linear depth this frame, the sky's where the
// G-buffer drew nothing lit.
@group(0) @binding(12) var denoise_half_depth:texture_2d<f32>;

// The slot the invocation's group denoises.
var<private> rtshadow_denoise_lightindex:u32;

fn FFX_DNSR_Shadows_IsFirstFrame()->i32 {
 let restart=(shadow_mask_slots.restart&(1u<<rtshadow_denoise_lightindex))!=0u;
 return select(0,1,traced.frame==0u || restart);
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
fn FFX_DNSR_Shadows_GetProjectionInverse()->mat4x4<f32> {
 return traced.inverse_projection;
}
fn FFX_DNSR_Shadows_GetViewProjectionInverse()->mat4x4<f32> {
 return traced.inverse_view_projection;
}
// The linear depth of `world_position` in the last submitted frame's view.
fn FFX_DNSR_Shadows_GetPreviousLinearDepth(world_position:vec3<f32>)->f32 {
 return -(traced.previous_view*vec4(world_position,1.)).z;
}
fn FFX_DNSR_Shadows_ReadPreviousLinearDepth(idx:vec2<i32>)->f32 {
 let last=vec2<i32>(FFX_DNSR_Shadows_GetBufferDimensions())-1;
 return textureLoad(denoise_previous_depth,clamp(idx,vec2(0),last),0).x;
}

fn FFX_DNSR_Shadows_ReadDepth(did:vec2<u32>)->f32 {
 return traced_denoise_depth(did);
}
fn FFX_DNSR_Shadows_ReadNormals(did:vec2<u32>)->vec3<f32> {
 return gbuffer_base_normal(textureLoad(denoise_normal,traced_full_pixel(did),0));
}
fn FFX_DNSR_Shadows_ReadRaytracedShadowMask(linear_tile_index:u32)->u32 {
 let tiles=FFX_DNSR_Shadows_RoundedDivide(FFX_DNSR_Shadows_GetBufferDimensions().x,8u);
 return textureLoad(denoise_tiles,vec2(linear_tile_index%tiles,linear_tile_index/tiles),0)[rtshadow_denoise_lightindex];
}
fn FFX_DNSR_Shadows_ReadPreviousMomentsBuffer(history_pos:vec2<i32>)->vec3<f32> {
 if FFX_DNSR_Shadows_IsFirstFrame()!=0 {
  return vec3(0.);
 }
 let last=vec2<i32>(FFX_DNSR_Shadows_GetBufferDimensions())-1;
 return textureLoad(denoise_moments_previous,clamp(history_pos,vec2(0),last),rtshadow_denoise_lightindex,0).xyz;
}
// The history's mean at `history_uv`, filtered bilinearly and clamped to
// the edge, as Wicked's sampler_linear_clamp filters it, from its four
// texels about it.
const DENOISE_BILINEAR_TAPS:i32=4;
fn FFX_DNSR_Shadows_ReadHistory(history_uv:vec2<f32>)->f32 {
 let dims=vec2<f32>(FFX_DNSR_Shadows_GetBufferDimensions());
 let position=history_uv*dims-.5;
 let base=floor(position);
 let fraction=position-base;
 let last=vec2<i32>(dims)-1;
 var corners=array<f32,DENOISE_BILINEAR_TAPS>(0.,0.,0.,0.);
 for (var corner=0;corner<DENOISE_BILINEAR_TAPS;corner++) {
  let texel=clamp(vec2<i32>(base)+vec2(corner%2,corner/2),vec2(0),last);
  corners[corner]=unpack2x16float(textureLoad(denoise_history,texel,rtshadow_denoise_lightindex,0).x).x;
 }
 return mix(mix(corners[0],corners[1],fraction.x),mix(corners[2],corners[3],fraction.x),fraction.y);
}
fn FFX_DNSR_Shadows_ReadVelocity(did:vec2<u32>)->vec2<f32> {
 return textureLoad(denoise_motion,traced_full_pixel(did),0).xy;
}

fn FFX_DNSR_Shadows_WriteReprojectionResults(did:vec2<u32>,value:vec2<f32>) {
 textureStore(denoise_reprojection,did,rtshadow_denoise_lightindex,vec4(pack2x16float(value)));
}
fn FFX_DNSR_Shadows_WriteMoments(did:vec2<u32>,value:vec3<f32>) {
 textureStore(denoise_moments,did,rtshadow_denoise_lightindex,vec4(value,0.));
}
fn FFX_DNSR_Shadows_WriteMetadata(idx:u32,mask:u32) {
 let groups=FFX_DNSR_Shadows_RoundedDivide(FFX_DNSR_Shadows_GetBufferDimensions().x,8u);
 textureStore(denoise_metadata,vec2(idx%groups,idx/groups),rtshadow_denoise_lightindex,vec4(mask));
}

fn FFX_DNSR_Shadows_IsShadowReciever(did:vec2<u32>)->bool {
 return traced_denoise_receiver(did);
}

@compute @workgroup_size(64) fn traced_denoise_tile_classification(@builtin(workgroup_id) gid:vec3<u32>,@builtin(local_invocation_index) group_index:u32) {
 rtshadow_denoise_lightindex=gid.z;
 FFX_DNSR_Shadows_TileClassification(group_index,gid.xy);
}
