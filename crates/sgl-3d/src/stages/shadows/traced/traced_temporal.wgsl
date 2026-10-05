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
// 209, rtdiffuse_temporalCS.hlsl 197); every slot is blended, where Wicked
// blends slots 4 to 15 and denoises 0 to 3, a denoiser this stage does not
// run yet; a slot whose light changed takes its current value
// (ShadowMaskSlots.restart), and so does every slot after the stage's
// history restarts (TracedParams.frame 0); the history is the stage's own
// linear depth at the tracing resolution, where Wicked reads its
// full-resolution depth history; the velocity in tracing pixels is the
// motion at the texel's full-resolution pixel; the neighbourhood is read
// once and a word's four slots blended at once, where Wicked reads it
// again for each slot, and a word whose slots hold no light is left as
// traced.
@group(0) @binding(0) var temporal_current:texture_2d<u32>;
@group(0) @binding(1) var temporal_history:texture_2d<u32>;
@group(0) @binding(2) var temporal_depth:texture_2d<f32>;
@group(0) @binding(3) var temporal_previous_depth:texture_2d<f32>;
@group(0) @binding(4) var temporal_motion:texture_2d<f32>;
@group(0) @binding(5) var<uniform> traced:TracedParams;
@group(0) @binding(6) var<uniform> shadow_mask_slots:ShadowMaskSlots;
@group(0) @binding(7) var temporal_output:texture_storage_2d<rgba32uint,write>;

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

@compute @workgroup_size(8,8) fn traced_shadow_temporal(@builtin(global_invocation_id) id:vec3<u32>) {
 let reduced=vec2<u32>(traced.reduced.xy);
 if any(id.xy>=reduced) {
  return;
 }
 let current=textureLoad(temporal_current,id.xy,0);
 let depth=textureLoad(temporal_depth,id.xy,0).x;
 if traced.frame==0u || depth>=TRACED_SKY_DEPTH {
  textureStore(temporal_output,id.xy,current);
  return;
 }
 // The texel's surface at its full-resolution pixel, where it was last
 // frame.
 let pixel=min(id.xy*2u,vec2<u32>(traced.full.xy)-1u);
 let motion=textureLoad(temporal_motion,pixel,0).xy;
 let previous=vec2<f32>(pixel)+.5-motion*traced.full.xy;
 if any(previous<vec2(0.)) || any(previous>=traced.full.xy) {
  textureStore(temporal_output,id.xy,current);
  return;
 }
 // The nearest tracing texel, whose centre is full-resolution pixel 2q's.
 let previous_texel=min(vec2<u32>(floor(previous*.5+.25)),reduced-1u);
 if abs(depth-textureLoad(temporal_previous_depth,previous_texel,0).x)>TEMPORAL_DISOCCLUSION {
  textureStore(temporal_output,id.xy,current);
  return;
 }
 let history=textureLoad(temporal_history,previous_texel,0);
 // The neighbourhood's first and second moments, each word's four slots
 // at once.
 var m1=mat4x4<f32>();
 var m2=mat4x4<f32>();
 for (var tap=0u;tap<TEMPORAL_TAPS;tap++) {
  let offset=vec2<i32>(i32(tap%3u)-1,i32(tap/3u)-1);
  let texel=clamp(vec2<i32>(id.xy)+offset,vec2(0),vec2<i32>(reduced)-1);
  let words=textureLoad(temporal_current,texel,0);
  let samples=mat4x4(traced_unpack(words.x),traced_unpack(words.y),traced_unpack(words.z),traced_unpack(words.w));
  m1+=samples;
  m2+=mat4x4(samples[0]*samples[0],samples[1]*samples[1],samples[2]*samples[2],samples[3]*samples[3]);
 }
 let velocity=length(motion*traced.reduced.xy);
 let refresh=saturate(velocity/TEMPORAL_VELOCITY_PIXELS);
 textureStore(temporal_output,id.xy,vec4(
  temporal_blend(0u,current.x,history.x,m1[0],m2[0],refresh),
  temporal_blend(1u,current.y,history.y,m1[1],m2[1],refresh),
  temporal_blend(2u,current.z,history.z,m1[2],m2[2],refresh),
  temporal_blend(3u,current.w,history.w,m1[3],m2[3],refresh),
 ));
}

// Word `word`'s four slots blended: traced as `current`, `history` the
// reprojected history, `m1` and `m2` the neighbourhood's moments. A word
// whose slots hold no light keeps its traced zeros.
fn temporal_blend(word:u32,current:u32,history:u32,m1:vec4<f32>,m2:vec4<f32>,refresh:f32)->u32 {
 if all(shadow_mask_slots.lights[word]==vec4(SHADOW_MASK_EMPTY)) {
  return current;
 }
 let value=traced_unpack(current);
 let mean=m1/f32(TEMPORAL_TAPS);
 let deviation=sqrt(max(m2/f32(TEMPORAL_TAPS)-mean*mean,vec4(0.)));
 let low=min(mean-TEMPORAL_SCALE*deviation,value);
 let high=max(mean+TEMPORAL_SCALE*deviation,value);
 let past=clamp(traced_unpack(history),low,high);
 let difference=abs(value-past)/max(value,max(past,vec4(.2)));
 let weight=(1.-difference)*(1.-difference);
 let response=mix(mix(vec4(TEMPORAL_RESPONSE_MIN),vec4(TEMPORAL_RESPONSE_MAX),weight),vec4(TEMPORAL_VELOCITY_RESPONSE),refresh);
 let restart=((vec4(shadow_mask_slots.restart)>>(vec4(0u,1u,2u,3u)+4u*word))&vec4(1u))!=vec4(0u);
 return traced_pack(select(mix(value,past,response),value,restart));
}
