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
// fraction. Every layer with a slot that holds a light is written, four
// slots at once, where Wicked writes the lights of the pixel's tile; the
// lighting pass reads only the slots that hold a light.
@group(0) @binding(0) var upsample_visibility:texture_2d<u32>;
@group(0) @binding(1) var upsample_half_depth:texture_2d<f32>;
@group(0) @binding(2) var upsample_depth:texture_depth_2d;
@group(0) @binding(3) var<uniform> traced:TracedParams;
@group(0) @binding(4) var upsample_mask:texture_storage_2d_array<rgba8unorm,write>;
@group(0) @binding(5) var<uniform> shadow_mask_slots:ShadowMaskSlots;

// Wicked's threshold: the weight lost per metre of linear depth between a
// tap and the pixel, and the least weight.
const UPSAMPLE_DEPTH_FALLOFF:f32=2.;
const UPSAMPLE_LEAST_WEIGHT:f32=.001;
// The taps: (0,0), (1,0), (0,1), (1,1) from texel p / 2.
const UPSAMPLE_TAPS:u32=4u;

// Layer `layer` of the mask at `pixel`, its four slots' `visibility`,
// unless none of them holds a light, which no pass reads.
fn upsample_store(pixel:vec2<u32>,layer:u32,visibility:vec4<f32>) {
 if any(shadow_mask_slots.lights[layer]!=vec4(SHADOW_MASK_EMPTY)) {
  textureStore(upsample_mask,pixel,layer,visibility);
 }
}

@compute @workgroup_size(8,8) fn traced_shadow_upsample(@builtin(global_invocation_id) id:vec3<u32>) {
 let full=vec2<u32>(traced.full.xy);
 if any(id.xy>=full) {
  return;
 }
 let z=textureLoad(upsample_depth,id.xy,0);
 if z<=0. {
  for (var layer=0u;layer<SHADOW_MASK_LAYERS;layer++) {
   upsample_store(id.xy,layer,vec4(1.));
  }
  return;
 }
 let uv=(vec2<f32>(id.xy)+.5)*traced.full.zw;
 let depth=traced_linear_depth(traced_position(uv,z));
 let reduced=vec2<u32>(traced.reduced.xy);
 let base=id.xy/2u;
 let fraction=vec2<f32>(id.xy%2u)*.5;
 // Each tap's weight, then its four words, each the four slots of a
 // layer, weighted at once.
 var sums=mat4x4<f32>();
 var total=0.;
 for (var tap=0u;tap<UPSAMPLE_TAPS;tap++) {
  let texel=min(base+vec2(tap%2u,tap/2u),reduced-1u);
  let along=select(1.-fraction,fraction,vec2(tap%2u,tap/2u)==vec2(1u));
  let closeness=max(UPSAMPLE_LEAST_WEIGHT,1.-saturate(abs(textureLoad(upsample_half_depth,texel,0).x-depth)*UPSAMPLE_DEPTH_FALLOFF));
  let weight=along.x*along.y*closeness;
  let words=textureLoad(upsample_visibility,texel,0);
  sums[0]+=traced_unpack(words.x)*weight;
  sums[1]+=traced_unpack(words.y)*weight;
  sums[2]+=traced_unpack(words.z)*weight;
  sums[3]+=traced_unpack(words.w)*weight;
  total+=weight;
 }
 upsample_store(id.xy,0u,sums[0]/total);
 upsample_store(id.xy,1u,sums[1]/total);
 upsample_store(id.xy,2u,sums[2]/total);
 upsample_store(id.xy,3u,sums[3]/total);
}
