// The ray-traced shadow stage's denoiser, its first pass (the
// architecture's Ray-traced shadows): AMD's tile classification over the
// slots it denoises, from slot 0, in one invocation, a slot a lane of the
// program's lanes (traced_denoise_lanes_*.wgsl). Group 0 is the pass's
// own.
//
// Ports Wicked Engine 2ff1d9e's rtshadow_denoise_tileclassificationCS.hlsl
// (MIT, src/LICENSE-wicked.txt): the callbacks AMD's
// ffx_denoiser_shadows_tileclassification.h takes, over the stage's
// targets, under INVERTED_DEPTH_RANGE. Changed at the port boundary: the
// slots in one invocation, a slot a lane of the port's values
// (ffx_denoiser_shadows_tileclassification.wgsl), where Wicked dispatches
// each with its index pushed (rtshadow_denoise_lightindex), so the tile
// masks, metadata and history hold the four slots in one texel's
// channels, a lane's its own, and one lane leaves the others zero;
// depth is the G-buffer's at the full-resolution pixel of each tracing
// pixel (Wicked's reads at did * 2), its normals the trace's
// half-resolution copy, as Wicked's (traced_denoise_common.wgsl); the
// previous depth is the stage's own, linear; a pixel the trace found
// nothing lit at reads as the sky, as the trace records it; AMD's kernel
// weights are constants (ffx_denoiser_shadows_tileclassification.wgsl);
// a slot's first frame is the stage's history restarting or the slot's
// light changing (ShadowMaskSlots.restart), whose previous moments read as
// zero, as Wicked clears its resources on its first frame; the history is
// sampled bilinearly from the scratch's two packed halves a lane
// (traced_denoise_common.wgsl), where Wicked keeps it in R16G16 and
// samples it through a linear sampler; and
// the moments are kept in RGBA16F, a layer a slot, where Wicked keeps
// R11G11B10, a lane reading and writing its own layer.
@group(0) @binding(0) var denoise_depth:texture_depth_2d;
// The tracing pixels' shading normals the trace writes.
@group(0) @binding(1) var denoise_normal:texture_2d<f32>;
@group(0) @binding(2) var denoise_tiles:texture_2d<u32>;
@group(0) @binding(3) var denoise_moments_previous:texture_2d_array<f32>;
@group(0) @binding(4) var denoise_history:texture_2d<u32>;
@group(0) @binding(5) var denoise_previous_depth:texture_2d<f32>;
@group(0) @binding(6) var denoise_motion:texture_2d<f32>;
@group(0) @binding(7) var<uniform> traced:TracedParams;
@group(0) @binding(8) var<uniform> shadow_mask_slots:ShadowMaskSlots;
@group(0) @binding(9) var denoise_metadata:texture_storage_2d<rgba32uint,write>;
@group(0) @binding(10) var denoise_reprojection:texture_storage_2d<rgba32uint,write>;
@group(0) @binding(11) var denoise_moments:texture_storage_2d_array<rgba16float,write>;
// The tracing resolution's linear depth this frame, the sky's where the
// G-buffer drew nothing lit.
@group(0) @binding(12) var denoise_half_depth:texture_2d<f32>;

// Whether this is each denoised slot's first frame: the stage's history
// restarted, or the slot's light changed.
fn FFX_DNSR_Shadows_IsFirstFrame()->FfxDnsrBool {
 let restart=((vec4(shadow_mask_slots.restart)>>shadow_mask_layer_slots(0u))&vec4(1u))!=vec4(0u);
 return ffx_dnsr_bool(vec4(traced.frame==0u)|restart);
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
// The linear depth of `world_position` in the last submitted frame's view.
fn FFX_DNSR_Shadows_GetPreviousLinearDepth(world_position:vec3<f32>)->f32 {
 return -(traced.previous_view*vec4(world_position,1.)).z;
}
fn FFX_DNSR_Shadows_ReadPreviousLinearDepth(idx:vec2<i32>)->f32 {
 let last=vec2<i32>(FFX_DNSR_Shadows_GetBufferDimensions())-1;
 return textureLoad(denoise_previous_depth,clamp(idx,vec2(0),last),0).x;
}

// The G-buffer's depth at the tracing pixel's full-resolution pixel, the
// sky's (0) where the trace found nothing lit, so that an unlit pixel is
// no receiver: the reprojection reconstructs its position from it.
fn FFX_DNSR_Shadows_ReadDepth(did:vec2<u32>)->f32 {
 if !traced_denoise_receiver(did) {
  return 0.;
 }
 return textureLoad(denoise_depth,traced_full_pixel(did),0);
}
fn FFX_DNSR_Shadows_ReadNormals(did:vec2<u32>)->vec3<f32> {
 return traced_denoise_normal(did);
}
// The denoised slots' masks of an 8×4 tile, a lane each.
fn FFX_DNSR_Shadows_ReadRaytracedShadowMask(linear_tile_index:u32)->FfxDnsrUint {
 let tiles=FFX_DNSR_Shadows_RoundedDivide(FFX_DNSR_Shadows_GetBufferDimensions().x,8u);
 return ffx_dnsr_uint(textureLoad(denoise_tiles,vec2(linear_tile_index%tiles,linear_tile_index/tiles),0));
}
// Each slot's previous moments, its lane's layer, zero in its first frame.
fn FFX_DNSR_Shadows_ReadPreviousMomentsBuffer(history_pos:vec2<i32>)->FFX_DNSR_Shadows_Moments {
 let last=vec2<i32>(FFX_DNSR_Shadows_GetBufferDimensions())-1;
 let texel=clamp(history_pos,vec2(0),last);
 var layers=mat4x4<f32>();
 for (var lane=0u;lane<FFX_DNSR_LANES;lane++) {
  layers[lane]=textureLoad(denoise_moments_previous,texel,lane,0);
 }
 let moments=transpose(layers);
 let first=FFX_DNSR_Shadows_IsFirstFrame();
 return FFX_DNSR_Shadows_Moments(
  select(ffx_dnsr_float(moments[0]),FfxDnsrFloat(0.),first),
  select(ffx_dnsr_float(moments[1]),FfxDnsrFloat(0.),first),
  select(ffx_dnsr_float(moments[2]),FfxDnsrFloat(0.),first),
 );
}
// The history's means at `texel`, a slot a lane.
fn traced_denoise_history(texel:vec2<i32>)->FfxDnsrFloat {
 let last=vec2<i32>(FFX_DNSR_Shadows_GetBufferDimensions())-1;
 return traced_denoise_unpack(textureLoad(denoise_history,clamp(texel,vec2(0),last),0)).mean;
}
// The history's means at `history_uv`, filtered bilinearly and clamped to
// the edge, as Wicked's sampler_linear_clamp filters it, from its four
// texels about it.
fn FFX_DNSR_Shadows_ReadHistory(history_uv:vec2<f32>)->FfxDnsrFloat {
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

fn FFX_DNSR_Shadows_WriteReprojectionResults(did:vec2<u32>,mean:FfxDnsrFloat,variance:FfxDnsrFloat) {
 textureStore(denoise_reprojection,did,traced_denoise_pack(mean,variance));
}
// Each slot's moments in its lane's layer.
fn FFX_DNSR_Shadows_WriteMoments(did:vec2<u32>,m:FfxDnsrFloat,s:FfxDnsrFloat,count:FfxDnsrFloat) {
 var moments=transpose(mat4x4(ffx_dnsr_float_texel(m),ffx_dnsr_float_texel(s),ffx_dnsr_float_texel(count),vec4(0.)));
 for (var lane=0u;lane<FFX_DNSR_LANES;lane++) {
  textureStore(denoise_moments,did,lane,moments[lane]);
 }
}
fn FFX_DNSR_Shadows_WriteMetadata(idx:u32,mask:FfxDnsrUint) {
 let groups=FFX_DNSR_Shadows_RoundedDivide(FFX_DNSR_Shadows_GetBufferDimensions().x,8u);
 textureStore(denoise_metadata,vec2(idx%groups,idx/groups),ffx_dnsr_uint_texel(mask));
}

fn FFX_DNSR_Shadows_IsShadowReciever(did:vec2<u32>)->bool {
 return traced_denoise_receiver(did);
}

@compute @workgroup_size(64) fn traced_denoise_tile_classification(@builtin(workgroup_id) gid:vec3<u32>,@builtin(local_invocation_index) group_index:u32) {
 FFX_DNSR_Shadows_TileClassification(group_index,gid.xy);
}
