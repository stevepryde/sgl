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
// 209, rtdiffuse_temporalCS.hlsl 197); the denoised slots
// (TracedParams.denoised) take the denoiser's result as Wicked's first
// four do (74–79), each from its layer where Wicked reads a channel a
// light, and word 0's others, when the denoiser filters slot 0 alone, are
// blended as words 1 to 3 are; a slot whose light changed takes
// its current value
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
// The denoiser's result for the denoised slots, a word a tracing pixel,
// packed as the trace packs its first word.
@group(0) @binding(8) var<storage,read> temporal_denoised:array<u32>;

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

// Word 0 of the output at tracing texel `texel`: the denoised slots
// (TracedParams.denoised, from slot 0) the denoiser's, the others
// `blended`'s.
fn temporal_word0(blended:u32,texel:vec2<u32>)->u32 {
 let denoised=temporal_denoised[texel.y*u32(traced.reduced.x)+texel.x];
 let mask=select(0xffu,0xffffffffu,traced.denoised>=TRACED_DENOISED_SLOTS);
 return (denoised&mask)|(blended&~mask);
}
// `current` with its denoised slots the denoiser's.
fn temporal_denoised_words(current:vec4<u32>,texel:vec2<u32>)->vec4<u32> {
 return vec4(temporal_word0(current.x,texel),current.yzw);
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
 // The nearest tracing texel, whose centre is full-resolution pixel 2q's.
 let previous_texel=min(vec2<u32>(floor(previous*.5+.25)),reduced-1u);
 if abs(depth-textureLoad(temporal_previous_depth,previous_texel,0).x)>TEMPORAL_DISOCCLUSION {
  textureStore(temporal_output,id.xy,temporal_denoised_words(current,id.xy));
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
  let samples=traced_unpack_words(textureLoad(temporal_current,texel,0));
  m1+=samples;
  for (var word=0u;word<SHADOW_MASK_LAYERS;word++) {
   m2[word]+=samples[word]*samples[word];
  }
 }
 let velocity=length(motion*traced.reduced.xy);
 let refresh=saturate(velocity/TEMPORAL_VELOCITY_PIXELS);
 // Word 0 is blended where the denoiser filters slot 0 alone.
 var word0=current.x;
 if traced.denoised<TRACED_DENOISED_SLOTS {
  word0=temporal_blend(0u,current,history,m1,m2,refresh);
 }
 textureStore(temporal_output,id.xy,vec4(
  temporal_word0(word0,id.xy),
  temporal_blend(1u,current,history,m1,m2,refresh),
  temporal_blend(2u,current,history,m1,m2,refresh),
  temporal_blend(3u,current,history,m1,m2,refresh),
 ));
}

// Word `word`'s four slots blended: traced as `current`, `history` the
// reprojected history's words, `m1` and `m2` the neighbourhood's moments,
// word w's in column w. A word whose slots hold no light keeps its traced
// zeros.
fn temporal_blend(word:u32,current:vec4<u32>,history:vec4<u32>,m1:mat4x4<f32>,m2:mat4x4<f32>,refresh:f32)->u32 {
 if all(shadow_mask_slots.lights[word]==vec4(SHADOW_MASK_EMPTY)) {
  return current[word];
 }
 let value=traced_unpack(current[word]);
 let mean=m1[word]/f32(TEMPORAL_TAPS);
 let deviation=sqrt(max(m2[word]/f32(TEMPORAL_TAPS)-mean*mean,vec4(0.)));
 let low=min(mean-TEMPORAL_SCALE*deviation,value);
 let high=max(mean+TEMPORAL_SCALE*deviation,value);
 let past=clamp(traced_unpack(history[word]),low,high);
 let difference=abs(value-past)/max(value,max(past,vec4(.2)));
 let weight=(1.-difference)*(1.-difference);
 let response=mix(mix(vec4(TEMPORAL_RESPONSE_MIN),vec4(TEMPORAL_RESPONSE_MAX),weight),vec4(TEMPORAL_VELOCITY_RESPONSE),refresh);
 let restart=((vec4(shadow_mask_slots.restart)>>shadow_mask_layer_slots(word))&vec4(1u))!=vec4(0u);
 return traced_pack(select(mix(value,past,response),value,restart));
}
