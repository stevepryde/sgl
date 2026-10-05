// The ray-traced shadow trace (the architecture's Ray-traced shadows): per
// pixel of half the render size, one visibility ray toward each slot's
// light that reaches the surface, through the scene ray function set of
// the hardware path (scene_segment_visible under SCENE_SIDES_SHADOW), into
// the slot's 8 bits of the texel's four words. Group 0 is the camera's lit
// group (its frame's directional lights and the scene's lights), group 1
// the scene's, group 3 the stage's own with the TLAS.
//
// Ports Wicked Engine 2ff1d9e's rtshadowCS.hlsl, which is
// screenspaceshadowCS.hlsl under RTSHADOW and RTAPI (9–14, 45–46, 76–84,
// 97–210, 215–257, 298–301, 309–319; MIT, src/LICENSE-wicked.txt): half
// resolution (DOWNSAMPLE 2), a ray from the surface with TMin 0.01 toward a
// point drawn on a point, spot or rectangle light or a direction within a
// directional light's disc, to infinity for the latter (light_surface.wgsl),
// cast where the light reaches the surface, culling front faces, its first
// hit ending it, 8 bits a light, and the denoised lights' bits gathered by
// 8×4 tile. Changed at the port boundary: a pixel's lights are the
// slot table's (shadow_mask_slots.wgsl), not the first sixteen of a
// sorted entity array; a light reaches the surface as the lit library's
// light_reach says (range, cone, and the lit side of the shading normal; a
// rectangle the half-space before its face), a directional light where
// either the shading or the geometry normal faces it, since the coat
// takes it along the geometry normal; the depth and normals are the
// G-buffer's at the full-resolution pixel 2q of tracing pixel q, where
// Wicked samples depth linearly between four; the side policy and
// cut-out texels are the shared predicate's, where Wicked's query culls
// front faces and alpha-tests candidates; a pixel the G-buffer drew
// nothing lit at (an unlit material) casts nothing and is the sky to the
// passes after, which Wicked, without unlit pixels, traces; the draw on
// the light is a hash of the pixel and the frame (hash.wgsl), where Wicked
// reads blue noise, a departure the owner judges (RD-5); the tile's bits
// gather through workgroup atomics into a storage texture, where Wicked
// ORs them into a buffer; and Wicked's half-resolution normals copy is not
// written, the denoiser reading the G-buffer's.
@group(3) @binding(0) var traced_depth:texture_depth_2d;
@group(3) @binding(1) var traced_normal:texture_2d<f32>;
@group(3) @binding(2) var traced_f0:texture_2d<f32>;
@group(3) @binding(3) var<uniform> traced:TracedParams;
@group(3) @binding(4) var<uniform> shadow_mask_slots:ShadowMaskSlots;
@group(3) @binding(5) var traced_raw:texture_storage_2d<rgba32uint,write>;
@group(3) @binding(6) var traced_half_depth:texture_storage_2d<r32float,write>;
// Each 8×4 tile's mask of the pixels that see the light of each slot the
// denoiser filters, one word a slot (tileclassification's
// ReadRaytracedShadowMask).
@group(3) @binding(7) var traced_tiles:texture_storage_2d<rgba32uint,write>;
// Wicked's ray.TMin: where a shadow ray starts along its direction, in
// metres, past the surface it leaves.
const TRACED_T_MIN:f32=.01;
// Wicked's FLT_MAX: a directional light's ray reaches as far as the scene,
// whatever the cascades' distance, which bounds the maps alone.
const TRACED_FAR:f32=3.402823466e+38;

// The index of the directional light with the frame's cascades, which
// slot 0 holds; 2 for none.
fn traced_directional_light()->u32 {
 for (var index=0u;index<2u;index++) {
  if (frame.directional_lights[index].flags&DIRECTIONAL_LIGHT_SHADOW)!=0u {
   return index;
  }
 }
 return 2u;
}

// Whether slot `key`'s light is unoccluded from the surface at `position`
// with shading normal `normal` and geometry normal `geometry_normal`, along
// a ray toward the point of the light `random` draws (light_surface.wgsl);
// false where the light does not reach the surface.
fn traced_visible(key:u32,position:vec3<f32>,normal:vec3<f32>,geometry_normal:vec3<f32>,random:vec2<f32>)->bool {
 if key==SHADOW_MASK_DIRECTIONAL {
  let index=traced_directional_light();
  if index>=2u {
   return false;
  }
  let light=frame.directional_lights[index];
  let to_light=normalize(light.direction_to_light);
  if dot(normal,to_light)<=0. && dot(geometry_normal,to_light)<=0. {
   return false;
  }
  let direction=directional_ray_direction(to_light,light.disc_radius,random);
  return scene_segment_visible(position,direction,TRACED_T_MIN,TRACED_FAR,SCENE_SIDES_SHADOW);
 }
 if key>=arrayLength(&lights) {
  return false;
 }
 let light=lights[key];
 if light_reach(light,position,normal,false).attenuation<=0. {
  return false;
 }
 let to_light=light_ray_end(light,position,random)-position;
 let distance=length(to_light);
 if distance<=0. {
  return true;
 }
 return scene_segment_visible(position,to_light/distance,TRACED_T_MIN,distance,SCENE_SIDES_SHADOW);
}

// The visibility words of tracing pixel `q`, and its linear depth, which
// it stores: none for a pixel beyond the tracing size.
fn traced_pixel(q:vec2<u32>)->vec4<u32> {
 if any(q>=vec2<u32>(traced.reduced.xy)) {
  return vec4(0u);
 }
 let pixel=traced_full_pixel(q);
 let z=textureLoad(traced_depth,pixel,0);
 var words=vec4(0u);
 // A pixel the G-buffer drew nothing lit at is no receiver: the lighting
 // reads none of its slots, and it records the sky's depth, so the
 // upsample and the denoiser weigh it as a sky texel.
 if z<=0. || !gbuffer_lit(textureLoad(traced_f0,pixel,0)) {
  textureStore(traced_raw,q,words);
  textureStore(traced_half_depth,q,vec4(TRACED_SKY_DEPTH));
  return words;
 }
 let uv=(vec2<f32>(pixel)+.5)*traced.full.zw;
 let position=traced_position(uv,z);
 textureStore(traced_half_depth,q,vec4(traced_linear_depth(position)));
 let normals=textureLoad(traced_normal,pixel,0);
 let normal=gbuffer_base_normal(normals);
 let geometry_normal=gbuffer_coat_normal(normals);
 // One draw on every light a pixel and frame, where Wicked reads its blue
 // noise.
 let random=hash33_unit(vec3(q,traced.seed)).xy;
 for (var slot=0u;slot<RT_SHADOW_LIGHTS;slot++) {
  let key=shadow_mask_slot_key(slot);
  if key!=SHADOW_MASK_EMPTY && traced_visible(key,position,normal,geometry_normal,random) {
   words=traced_store(words,slot,1.);
  }
 }
 textureStore(traced_raw,q,words);
 return words;
}

// The tile's bits of the pixels that see each denoised slot's light.
var<workgroup> traced_tile:array<atomic<u32>,TRACED_DENOISED_SLOTS>;

// A workgroup is one 8×4 tile of the denoiser: its pixels' bits are
// gathered as Wicked's trace gathers them into its tile buffer (309–319),
// lane (y % 4) · 8 + x % 8 (ffx_denoiser_shadows_util.h
// FFX_DNSR_Shadows_GetBitMaskFromPixelPosition).
@compute @workgroup_size(8,4) fn traced_shadow_rays(@builtin(global_invocation_id) id:vec3<u32>,@builtin(workgroup_id) tile:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
 // The denoised slots are word 0.
 let denoised=traced_unpack(traced_pixel(id.xy).x);
 for (var slot=0u;slot<TRACED_DENOISED_SLOTS;slot++) {
  if denoised[slot]>0. {
   atomicOr(&traced_tile[slot],1u<<lane);
  }
 }
 workgroupBarrier();
 if lane==0u {
  var masks=vec4(0u);
  for (var slot=0u;slot<TRACED_DENOISED_SLOTS;slot++) {
   masks[slot]=atomicLoad(&traced_tile[slot]);
  }
  textureStore(traced_tiles,tile.xy,masks);
 }
}
