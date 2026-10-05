// The ray-traced shadow stage's temporal blend (the architecture's
// Ray-traced shadows), at the tracing resolution: each slot's traced
// visibility blended with its history reprojected by the surface's motion.
// Group 0 is the pass's own.
//
// Ports Wicked Engine 2ff1d9e's rtshadow_denoise_temporalCS.hlsl (MIT,
// src/LICENSE-wicked.txt): the current value where the reprojection leaves
// the screen or the linear depth there differs by more than a metre;
// otherwise, per slot, a 3×3 neighbourhood's mean and deviation, a
// response from 0.88 to 1 by how much the value changed, refreshed toward
// 0.2 by velocity, Unreal's ghosting fix it cites (101–124). Changed: the
// history is clamped to the neighbourhood's box (the mean within twice
// its deviation, and the current value), which Wicked's ResolverAABB
// computes and then leaves unused, so a shadow that moves takes the
// current value where its history falls outside the box, as Wicked's own
// SSR and RT diffuse temporal passes clamp theirs (ssr_temporalCS.hlsl
// 209, rtdiffuse_temporalCS.hlsl 197); the denoised slots take the
// denoiser's result as Wicked's first four do (74–79), each from its layer
// where Wicked reads a channel a light; a slot whose light changed takes
// its current value
// (ShadowMaskSlots.restart), and so does every slot after the stage's
// history restarts (TracedParams.frame 0); the history is the stage's own
// linear depth at the tracing resolution, where Wicked reads its
// full-resolution depth history; the velocity in tracing pixels is the
// motion at the texel's full-resolution pixel.
@group(0) @binding(0) var temporal_current:texture_2d<u32>;
@group(0) @binding(1) var temporal_history:texture_2d<u32>;
@group(0) @binding(2) var temporal_depth:texture_2d<f32>;
@group(0) @binding(3) var temporal_previous_depth:texture_2d<f32>;
@group(0) @binding(4) var temporal_motion:texture_2d<f32>;
@group(0) @binding(5) var<uniform> traced:TracedParams;
@group(0) @binding(6) var<uniform> shadow_mask_slots:ShadowMaskSlots;
@group(0) @binding(7) var temporal_output:texture_storage_2d<rgba32uint,write>;
// The denoiser's result for each denoised slot, a layer a slot.
@group(0) @binding(8) var temporal_denoised:texture_2d_array<f32>;

// Wicked's temporalResponseMin and temporalResponseMax: the history's
// least and greatest share.
const TEMPORAL_RESPONSE_MIN:f32=.88;
const TEMPORAL_RESPONSE_MAX:f32=1.;
// Wicked's temporalScale: the box's half-width in deviations.
const TEMPORAL_SCALE:f32=2.;
// Wicked's disocclusion threshold: linear depths further apart, in metres,
// are different surfaces.
const TEMPORAL_DISOCCLUSION:f32=1.;
// Wicked's velocity refresh: the response at and beyond this many tracing
// pixels of motion a frame, and that response.
const TEMPORAL_VELOCITY_PIXELS:f32=100.;
const TEMPORAL_VELOCITY_RESPONSE:f32=.2;
// The neighbourhood's offsets: Wicked's 3×3 SampleOffset.
const TEMPORAL_TAPS:u32=9u;

// `current`'s visibilities with the denoised slots' the denoiser's, at
// tracing texel `texel`.
fn temporal_denoised_words(current:vec4<u32>,texel:vec2<u32>)->vec4<u32> {
 var words=vec4(0u);
 for (var slot=0u;slot<RT_SHADOW_LIGHTS;slot++) {
  var value=traced_load(current,slot);
  if slot<TRACED_DENOISED_SLOTS {
   value=textureLoad(temporal_denoised,texel,slot,0).x;
  }
  words=traced_store(words,slot,value);
 }
 return words;
}

@compute @workgroup_size(8,8) fn traced_shadow_temporal(@builtin(global_invocation_id) id:vec3<u32>) {
 let reduced=vec2<u32>(traced.reduced.xy);
 if any(id.xy>=reduced) {
  return;
 }
 let current=textureLoad(temporal_current,id.xy,0);
 let depth=textureLoad(temporal_depth,id.xy,0).x;
 if traced.frame==0u || depth>=TRACED_SKY_DEPTH {
  textureStore(temporal_output,id.xy,temporal_denoised_words(current,id.xy));
  return;
 }
 // The texel's surface at its full-resolution pixel, where it was last
 // frame.
 let pixel=traced_full_pixel(id.xy);
 let motion=textureLoad(temporal_motion,pixel,0).xy;
 let previous=vec2<f32>(pixel)+.5-motion*traced.full.xy;
 if any(previous<vec2(0.)) || any(previous>=traced.full.xy) {
  textureStore(temporal_output,id.xy,temporal_denoised_words(current,id.xy));
  return;
 }
 let previous_texel=min(vec2<u32>(previous*.5),reduced-1u);
 if abs(depth-textureLoad(temporal_previous_depth,previous_texel,0).x)>TEMPORAL_DISOCCLUSION {
  textureStore(temporal_output,id.xy,temporal_denoised_words(current,id.xy));
  return;
 }
 let history=textureLoad(temporal_history,previous_texel,0);
 var neighbours:array<vec4<u32>,9>;
 for (var tap=0u;tap<TEMPORAL_TAPS;tap++) {
  let offset=vec2<i32>(i32(tap%3u)-1,i32(tap/3u)-1);
  let texel=clamp(vec2<i32>(id.xy)+offset,vec2(0),vec2<i32>(reduced)-1);
  neighbours[tap]=textureLoad(temporal_current,texel,0);
 }
 let velocity=length(motion*traced.reduced.xy);
 let refresh=saturate(velocity/TEMPORAL_VELOCITY_PIXELS);
 var words=vec4(0u);
 for (var slot=0u;slot<RT_SHADOW_LIGHTS;slot++) {
  let value=traced_load(current,slot);
  if slot<TRACED_DENOISED_SLOTS {
   words=traced_store(words,slot,textureLoad(temporal_denoised,id.xy,slot,0).x);
   continue;
  }
  if (shadow_mask_slots.restart&(1u<<slot))!=0u {
   words=traced_store(words,slot,value);
   continue;
  }
  var m1=0.;
  var m2=0.;
  for (var tap=0u;tap<TEMPORAL_TAPS;tap++) {
   let sample=traced_load(neighbours[tap],slot);
   m1+=sample;
   m2+=sample*sample;
  }
  let mean=m1/f32(TEMPORAL_TAPS);
  let deviation=sqrt(max(m2/f32(TEMPORAL_TAPS)-mean*mean,0.));
  let low=min(mean-TEMPORAL_SCALE*deviation,value);
  let high=max(mean+TEMPORAL_SCALE*deviation,value);
  let past=clamp(traced_load(history,slot),low,high);
  let difference=abs(value-past)/max(value,max(past,.2));
  let weight=(1.-difference)*(1.-difference);
  let response=mix(mix(TEMPORAL_RESPONSE_MIN,TEMPORAL_RESPONSE_MAX,weight),TEMPORAL_VELOCITY_RESPONSE,refresh);
  words=traced_store(words,slot,mix(value,past,response));
 }
 textureStore(temporal_output,id.xy,words);
}
