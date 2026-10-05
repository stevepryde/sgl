// The ray-traced shadow stage's upsample (the architecture's Ray-traced
// shadows): each slot's blended visibility from the tracing resolution to
// the render size's shadow mask, slot s in channel s % 4 of layer s / 4
// (shadow_mask_slots.wgsl). Group 0 is the pass's own.
//
// Ports Wicked Engine 2ff1d9e's rtshadow_upsampleCS.hlsl 35–55 (MIT,
// src/LICENSE-wicked.txt): the four tracing texels about the pixel,
// bilinearly weighted and each weighted again by how close its linear
// depth is to the pixel's (1 less twice the difference in metres, at least
// 0.001), normalised. Changed at the port boundary: SGL3D's tracing texel
// q holds full-resolution pixel 2q, where Wicked's samples between pixels
// 2q and 2q + 1, so pixel p's taps are texels p / 2 and the next, at a
// fraction of half for an odd p and none for an even one; Wicked's taps
// (p / 2 and the next, at the fraction of a grid half a texel over) and
// its top row (its gather order's x reversed) do not agree with its
// fraction. Every slot is written, where Wicked writes the lights of the
// pixel's tile; the lighting pass reads only the slots that hold a light.
@group(0) @binding(0) var upsample_visibility:texture_2d<u32>;
@group(0) @binding(1) var upsample_half_depth:texture_2d<f32>;
@group(0) @binding(2) var upsample_depth:texture_depth_2d;
@group(0) @binding(3) var<uniform> traced:TracedParams;
@group(0) @binding(4) var upsample_mask:texture_storage_2d_array<rgba8unorm,write>;

// Wicked's threshold: the weight lost per metre of linear depth between a
// tap and the pixel, and the least weight.
const UPSAMPLE_DEPTH_FALLOFF:f32=2.;
const UPSAMPLE_LEAST_WEIGHT:f32=.001;
// The taps: (0,0), (1,0), (0,1), (1,1) from texel p / 2.
const UPSAMPLE_TAPS:u32=4u;

@compute @workgroup_size(8,8) fn traced_shadow_upsample(@builtin(global_invocation_id) id:vec3<u32>) {
 let full=vec2<u32>(traced.full.xy);
 if any(id.xy>=full) {
  return;
 }
 let z=textureLoad(upsample_depth,id.xy,0);
 if z<=0. {
  for (var layer=0u;layer<SHADOW_MASK_LAYERS;layer++) {
   textureStore(upsample_mask,id.xy,layer,vec4(1.));
  }
  return;
 }
 let uv=(vec2<f32>(id.xy)+.5)*traced.full.zw;
 let depth=traced_linear_depth(traced_position(uv,z));
 let reduced=vec2<u32>(traced.reduced.xy);
 let base=id.xy/2u;
 let fraction=vec2<f32>(id.xy%2u)*.5;
 var words:array<vec4<u32>,4>;
 var weights:array<f32,4>;
 var total=0.;
 for (var tap=0u;tap<UPSAMPLE_TAPS;tap++) {
  let texel=min(base+vec2(tap%2u,tap/2u),reduced-1u);
  let along=select(1.-fraction,fraction,vec2(tap%2u,tap/2u)==vec2(1u));
  let closeness=max(UPSAMPLE_LEAST_WEIGHT,1.-saturate(abs(textureLoad(upsample_half_depth,texel,0).x-depth)*UPSAMPLE_DEPTH_FALLOFF));
  words[tap]=textureLoad(upsample_visibility,texel,0);
  weights[tap]=along.x*along.y*closeness;
  total+=weights[tap];
 }
 var layers:array<vec4<f32>,SHADOW_MASK_LAYERS>;
 for (var slot=0u;slot<RT_SHADOW_LIGHTS;slot++) {
  var sum=0.;
  for (var tap=0u;tap<UPSAMPLE_TAPS;tap++) {
   sum+=traced_load(words[tap],slot)*weights[tap];
  }
  layers[shadow_mask_layer(slot)][shadow_mask_channel(slot)]=sum/total;
 }
 for (var layer=0u;layer<SHADOW_MASK_LAYERS;layer++) {
  textureStore(upsample_mask,id.xy,layer,layers[layer]);
 }
}
