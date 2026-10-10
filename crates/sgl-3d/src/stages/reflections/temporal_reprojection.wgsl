// Temporal reprojection for reflections: Wicked Engine's (revision
// 2ff1d9e7b36091d6edf9f823af77e6bc9af20e3b, MIT, see LICENSE-wicked.txt)
// shaders/ssr_temporalCS.hlsl, with uv_to_clipspace, clipspace_to_uv and
// is_saturated from globals.hlsli: dual reprojection by motion and by
// reflection hit (Stachowiak and Uludag, SEED, "Towards Effortless
// Photorealism Through Real-Time Raytracing", DD18), depth disocclusion and a
// 3x3 colour box clamp. World-space rays and Velvet accumulate through it;
// each pass keeps its own inputs, reset and outputs. Modified: translated to
// WGSL; loads outside the grid are clamped where HLSL would read zero; the
// pass keeps its own depth history at its traced size in place of the
// previous frame's depth buffer, and supplies the velocity (SGL3D motion,
// current minus previous, negated into Wicked's); a reflection hit on or
// behind the previous camera's plane reprojects off the screen rather than
// mirrored onto it (temporal_reprojection_uv); the depth history, raw
// device depth, is linearised with the near plane it was written under.
// Reads `temporal_current`, `temporal_history`, `temporal_depth_history` and
// `linear_sampler`.
const TEMPORAL_RESPONSE:f32=.95;
const TEMPORAL_SCALE:f32=2.;
const DISOCCLUSION_DEPTH_WEIGHT:f32=1.;
const DISOCCLUSION_THRESHOLD:f32=.9;
// The pass's camera and traced grid.
struct TemporalView {
 inverse_view_projection:mat4x4<f32>,
 previous_view_projection:mat4x4<f32>,
 // Traced width, height, 1/width, 1/height.
 size:vec4<f32>,
 // The reversed-Z infinite projection's near plane, this frame's and the
 // previous frame's, which wrote the depth history.
 near:f32,
 previous_near:f32,
}
fn temporal_saturated(uv:vec2<f32>)->bool {
 return all(uv==clamp(uv,vec2(0.),vec2(1.)));
}
// Where the point at `depth` seen at `uv` was on the previous frame's screen.
// A point on or behind the previous camera's plane (clip w <= 0) was not on
// it: dividing by a negative w mirrors it onto the screen (a zero one gives
// no finite position), so it lies a screen off, where temporal_saturated
// rejects it. SGL3D's correction: Wicked's
// ssr_temporalCS (2ff1d9e, and 4323a33) divides unguarded, as AMD's
// reflection denoiser (FidelityFX SDK c6efa6b,
// FFX_DNSR_Reflections_GetHitPositionReprojection through ProjectPosition,
// ffx_denoiser_reflections_reproject.h and _common.h), Bevy 9d12036's motion
// vectors (prepass.wesl, and bevy_solari's virtual reflection points in
// resolve_dlss_rr_textures.wesl), Godot b130438's reflection hits
// (effects/screen_space_reflection.glsl) and Filament ef1a133's
// (surface_light_reflections.fs, ssrReprojection) and TAA history
// (antiAliasing/taa/taa.mat) do; Wicked 4323a33's own velocity
// (visibility_velocityCS.hlsl) keeps only a positive previous w, as
// gbuffer_encode_motion does.
fn temporal_reprojection_uv(view:TemporalView,uv:vec2<f32>,depth:f32)->vec2<f32> {
 let screen=vec2(uv.x*2.-1.,1.-uv.y*2.);
 let previous=view.previous_view_projection*(view.inverse_view_projection*vec4(screen,depth,1.));
 if previous.w<=0. {
  return vec2(-1.);
 }
 return previous.xy/previous.w*vec2(.5,-.5)+vec2(.5);
}
fn temporal_disocclusion(view:TemporalView,depth:f32,history:f32)->f32 {
 let current=linear_depth(view.near,depth);
 let previous=linear_depth(view.previous_near,history);
 return exp(-abs(previous-current)/current*DISOCCLUSION_DEPTH_WEIGHT);
}
fn temporal_history_depth(view:TemporalView,uv:vec2<f32>)->f32 {
 let q=clamp(vec2<i32>(floor(uv*view.size.xy)),vec2(0),vec2<i32>(view.size.xy)-vec2(1));
 return textureLoad(temporal_depth_history,q,0).x;
}
// A history colour, the disocclusion weight where it was read (0 when
// disoccluded) and its UV.
struct TemporalHistory { color:vec4<f32>, disocclusion:f32, uv:vec2<f32> }
fn temporal_previous_color(view:TemporalView,previous_uv:vec2<f32>,depth:f32)->TemporalHistory {
 var uv=previous_uv;
 var color=textureSampleLevel(temporal_history,linear_sampler,uv,0.);
 var disocclusion=temporal_disocclusion(view,depth,temporal_history_depth(view,uv));
 if disocclusion>DISOCCLUSION_THRESHOLD {
  return TemporalHistory(color,disocclusion,uv);
 }
 // Find the closest sample in the vicinity if a disocclusion is not certain,
 // offsetting each candidate from the best one so far.
 if disocclusion<DISOCCLUSION_THRESHOLD {
  let texel=view.size.zw;
  for(var y=-1;y<=1;y++) {
   for(var x=-1;x<=1;x++) {
    let candidate=uv+vec2<f32>(vec2(x,y))*texel;
    let weight=temporal_disocclusion(view,depth,temporal_history_depth(view,candidate));
    if weight>disocclusion {
     disocclusion=weight;
     uv=candidate;
    }
   }
  }
  color=textureSampleLevel(temporal_history,linear_sampler,uv,0.);
 }
 // Bilinear interpolation on fallback, near edges.
 if disocclusion<DISOCCLUSION_THRESHOLD {
  let f=fract(uv*view.size.xy+vec2(.5));
  let weights=array((1.-f.x)*(1.-f.y),f.x*(1.-f.y),(1.-f.x)*f.y,f.x*f.y);
  let base=vec2<i32>(view.size.xy*uv-vec2(.5));
  let offsets=array(vec2(0,0),vec2(1,0),vec2(0,1),vec2(1,1));
  var color_sum=vec4(0.);
  var depth_sum=0.;
  var weight_sum=0.;
  for(var i=0;i<4;i++) {
   let q=clamp(base+offsets[i],vec2(0),vec2<i32>(view.size.xy)-vec2(1));
   color_sum+=weights[i]*textureLoad(temporal_history,q,0);
   depth_sum+=weights[i]*textureLoad(temporal_depth_history,q,0).x;
   weight_sum+=weights[i];
  }
  color=color_sum/max(weight_sum,.00001);
  disocclusion=temporal_disocclusion(view,depth,depth_sum/max(weight_sum,.00001));
 }
 if disocclusion<DISOCCLUSION_THRESHOLD {
  disocclusion=0.;
 }
 return TemporalHistory(color,disocclusion,uv);
}
// Accumulates `current`, the colour at traced pixel `p` whose receiver is at
// `depth`, over the history the better of its two reprojections finds. The
// result is not yet clamped to zero; its disocclusion and UV are the
// history's.
fn temporal_accumulate(view:TemporalView,p:vec2<i32>,current:vec4<f32>,velocity:vec2<f32>,reprojection_depth:f32,depth:f32)->TemporalHistory {
 // Welford's online algorithm over the 3x3 neighbourhood.
 var m1=vec4(0.);
 var m2=vec4(0.);
 for(var x=-1;x<=1;x++) {
  for(var y=-1;y<=1;y++) {
   let q=clamp(p+vec2(x,y),vec2(0),vec2<i32>(view.size.xy)-vec2(1));
   let sample=textureLoad(temporal_current,q,0);
   m1+=sample;
   m2+=sample*sample;
  }
 }
 let mean=m1/9.;
 let stddev=sqrt(max(m2/9.-mean*mean,vec4(0.)));
 // Secondary reprojection based on ray lengths (SEED DD18, slide 45).
 let uv=(vec2<f32>(p)+.5)*view.size.zw;
 let by_velocity=uv+velocity;
 let by_hit=temporal_reprojection_uv(view,uv,reprojection_depth);
 let color_velocity=textureSampleLevel(temporal_history,linear_sampler,by_velocity,0.);
 let color_hit=textureSampleLevel(temporal_history,linear_sampler,by_hit,0.);
 let distance_velocity=abs(luminance(color_velocity.rgb)-luminance(mean.rgb));
 let distance_hit=abs(luminance(color_hit.rgb)-luminance(mean.rgb));
 let previous=temporal_previous_color(view,select(by_hit,by_velocity,distance_velocity<distance_hit),depth);
 var result=current;
 if previous.disocclusion>DISOCCLUSION_THRESHOLD && temporal_saturated(previous.uv) {
  // Colour box clamp.
  let history=clamp(previous.color,mean-TEMPORAL_SCALE*stddev,mean+TEMPORAL_SCALE*stddev);
  result=mix(current,history,TEMPORAL_RESPONSE);
 }
 return TemporalHistory(result,previous.disocclusion,previous.uv);
}
