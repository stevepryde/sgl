// The ray-traced shadow stage's upsample (the architecture's Ray-traced
// shadows): each slot's blended visibility from the tracing resolution to
// the render size's shadow mask, slot s in channel s % 4 of layer s / 4
// (shadow_mask_slots.wgsl). Group 0 is the pass's own.
//
// Ports Wicked Engine 2ff1d9e's rtshadow_upsampleCS.hlsl 35–55 (MIT,
// src/LICENSE-wicked.txt): the four tracing texels about the pixel,
// bilinearly weighted and each weighted again by how close its linear
// depth is to the pixel's (1 less twice the difference in metres, at least
// 0.001, traced_closeness), normalised. Changed at the port boundary:
// SGL3D's tracing texel q holds a pixel of its block, 2q but where the
// trace takes another (traced_texel_pixel), where Wicked's samples
// between pixels 2q and 2q + 1, so pixel p's taps
// are texels p / 2 and the next, at a fraction of half for an odd p and
// none for an even one, the fraction of a grid of pixels 2q; Wicked's taps
// (p / 2 and the next, at the fraction of a grid half a texel over) and
// its top row (its gather order's x reversed) do not agree with its
// fraction. Only the taps that hold a receiver are weighed, and a pixel
// with none about it takes 1 in every slot, the trace's rule for a pixel it
// casts no ray from (traced_visible), where Wicked weighs every tap, at
// least 0.001, and so carries what a sky texel holds (never written there,
// as its trace returns early; SGL3D's trace writes 0) onto a lit pixel
// whose taps are all sky: a thin lit feature at an odd pixel against the
// sky or an unlit surface read as fully shadowed (#771). Every layer with a
// slot that holds a light is written, four slots at once, where Wicked
// writes the lights of the pixel's tile; the lighting pass reads only the
// slots that hold a light. A pixel none of whose weighed taps is its
// surface (each at the least weight, or none holding a receiver), which
// its footprint does not represent, as where its block's texel stands for
// a thin feature in front of it or it shows a far surface through a slit
// one pixel wide, is weighed instead from the 3×3 texels about its block
// that are its surface, by the same closeness and by the bilinear tent
// widened to the search's reach of their pixels' distance from it, so the
// pixels behind a feature one pixel wide take the blocks beside it, where
// Wicked's four taps blend the feature's in at the least weight; where
// none is either, it keeps the four taps' answer.
@group(0) @binding(0) var upsample_visibility:texture_2d<u32>;
@group(0) @binding(1) var upsample_half_depth:texture_2d<f32>;
@group(0) @binding(2) var upsample_depth:texture_depth_2d;
@group(0) @binding(3) var<uniform> traced:TracedParams;
@group(0) @binding(4) var upsample_mask:texture_storage_2d_array<rgba8unorm,write>;
@group(0) @binding(5) var<uniform> shadow_mask_slots:ShadowMaskSlots;
// The trace's half-resolution normals, whose fourth component records the
// pixel each texel stands for.
@group(0) @binding(6) var upsample_normal:texture_2d<f32>;

// The taps: (0,0), (1,0), (0,1), (1,1) from texel p / 2.
const UPSAMPLE_TAPS:u32=4u;
// The search: the 3×3 texels about texel p / 2, nine taps, a fixed count
// no texel's contents change (AR-12).
const UPSAMPLE_SEARCH_TAPS:u32=9u;
// The search's tent: weight falls to zero this many full-resolution
// pixels from the pixel, twice the four taps' (one tracing texel), so the
// farthest of the search's pixels, three away, keeps a quarter.
const UPSAMPLE_SEARCH_REACH:f32=4.;

// Layer `layer` of the mask at `pixel`, its four slots' visibility
// `sums[layer] / total`, unless none of them holds a light, which no pass
// reads.
fn upsample_store(pixel:vec2<u32>,layer:u32,sums:mat4x4<f32>,total:f32) {
 if any(shadow_mask_slots.lights[layer]!=vec4(SHADOW_MASK_EMPTY)) {
  textureStore(upsample_mask,pixel,layer,sums[layer]/total);
 }
}
// Every layer of the mask at `pixel` lit, 1 in every slot.
fn upsample_store_lit(pixel:vec2<u32>) {
 let lit=mat4x4(vec4(1.),vec4(1.),vec4(1.),vec4(1.));
 for (var layer=0u;layer<SHADOW_MASK_LAYERS;layer++) {
  upsample_store(pixel,layer,lit,1.);
 }
}

@compute @workgroup_size(8,8) fn traced_shadow_upsample(@builtin(global_invocation_id) id:vec3<u32>) {
 let full=vec2<u32>(traced.full.xy);
 if any(id.xy>=full) {
  return;
 }
 let z=textureLoad(upsample_depth,id.xy,0);
 if z<=0. {
  upsample_store_lit(id.xy);
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
 // Whether a weighed tap is the pixel's surface.
 var represented=false;
 for (var tap=0u;tap<UPSAMPLE_TAPS;tap++) {
  let texel=min(base+vec2(tap%2u,tap/2u),reduced-1u);
  let along=select(1.-fraction,fraction,vec2(tap%2u,tap/2u)==vec2(1u));
  let tap_depth=textureLoad(upsample_half_depth,texel,0).x;
  let closeness=traced_closeness(tap_depth,depth);
  // A sky or unlit texel holds no receiver: its zeros are no visibility.
  let weight=select(0.,along.x*along.y*closeness,traced_holds_receiver(tap_depth));
  sums+=traced_unpack_words(textureLoad(upsample_visibility,texel,0))*weight;
  total+=weight;
  represented|=weight>0. && closeness>TRACED_LEAST_WEIGHT;
 }
 if !represented {
  var found=mat4x4<f32>();
  var found_total=0.;
  let pixel=vec2<f32>(id.xy);
  for (var tap=0u;tap<UPSAMPLE_SEARCH_TAPS;tap++) {
   let near=vec2<i32>(base)+vec2(i32(tap%3u)-1,i32(tap/3u)-1);
   if any(near<vec2(0)) || any(near>=vec2<i32>(reduced)) {
    continue;
   }
   let texel=vec2<u32>(near);
   let tap_depth=textureLoad(upsample_half_depth,texel,0).x;
   let closeness=traced_closeness(tap_depth,depth);
   if !traced_holds_receiver(tap_depth) || closeness<=TRACED_LEAST_WEIGHT {
    continue;
   }
   let away=abs(vec2<f32>(traced_texel_pixel(texel,textureLoad(upsample_normal,texel,0)))-pixel);
   let tent=saturate(1.-away/UPSAMPLE_SEARCH_REACH);
   let weight=tent.x*tent.y*closeness;
   found+=traced_unpack_words(textureLoad(upsample_visibility,texel,0))*weight;
   found_total+=weight;
  }
  if found_total>0. {
   sums=found;
   total=found_total;
  }
 }
 if total<=0. {
  upsample_store_lit(id.xy);
  return;
 }
 upsample_store(id.xy,0u,sums,total);
 upsample_store(id.xy,1u,sums,total);
 upsample_store(id.xy,2u,sums,total);
 upsample_store(id.xy,3u,sums,total);
}
