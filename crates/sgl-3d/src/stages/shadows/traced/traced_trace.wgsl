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
// 97–210, 215–257, 298–301; MIT, src/LICENSE-wicked.txt): half resolution
// (DOWNSAMPLE 2), a ray from the surface with TMin 0.01 to a point or spot
// light's position and a directional light at infinity, cast where the
// light reaches the surface, culling front faces, its first hit ending it,
// 8 bits a light. Changed at the port boundary: a pixel's lights are the
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
// nothing lit at casts nothing. The ray ends at the light's centre: a hard
// shadow. Its normals copy and tile mask feed the denoiser, which this
// stage does not run yet, and are not written.
@group(3) @binding(0) var traced_depth:texture_depth_2d;
@group(3) @binding(1) var traced_normal:texture_2d<f32>;
@group(3) @binding(2) var traced_f0:texture_2d<f32>;
@group(3) @binding(3) var<uniform> traced:TracedParams;
@group(3) @binding(4) var<uniform> shadow_mask_slots:ShadowMaskSlots;
@group(3) @binding(5) var traced_raw:texture_storage_2d<rgba32uint,write>;
@group(3) @binding(6) var traced_half_depth:texture_storage_2d<r32float,write>;
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

// Whether slot `key`'s light, which reaches it, is unoccluded from the
// surface at `position` with shading normal `normal` and geometry normal
// `geometry_normal`; false where it does not reach the surface.
fn traced_visible(key:u32,position:vec3<f32>,normal:vec3<f32>,geometry_normal:vec3<f32>)->bool {
 if key==SHADOW_MASK_DIRECTIONAL {
  let index=traced_directional_light();
  if index>=2u {
   return false;
  }
  let to_light=normalize(frame.directional_lights[index].direction_to_light);
  if dot(normal,to_light)<=0. && dot(geometry_normal,to_light)<=0. {
   return false;
  }
  return scene_segment_visible(position,to_light,TRACED_T_MIN,TRACED_FAR,SCENE_SIDES_SHADOW);
 }
 if key>=arrayLength(&lights) {
  return false;
 }
 let light=lights[key];
 if light_reach(light,position,normal,false).attenuation<=0. {
  return false;
 }
 let to_light=light.position-position;
 let distance=length(to_light);
 return scene_segment_visible(position,to_light/distance,TRACED_T_MIN,distance,SCENE_SIDES_SHADOW);
}

@compute @workgroup_size(8,4) fn traced_shadow_rays(@builtin(global_invocation_id) id:vec3<u32>) {
 let reduced=vec2<u32>(traced.reduced.xy);
 if any(id.xy>=reduced) {
  return;
 }
 let pixel=min(id.xy*2u,vec2<u32>(traced.full.xy)-1u);
 let z=textureLoad(traced_depth,pixel,0);
 if z<=0. {
  textureStore(traced_raw,id.xy,vec4(0u));
  textureStore(traced_half_depth,id.xy,vec4(TRACED_SKY_DEPTH));
  return;
 }
 let uv=(vec2<f32>(pixel)+.5)*traced.full.zw;
 let position=traced_position(uv,z);
 textureStore(traced_half_depth,id.xy,vec4(traced_linear_depth(position)));
 if !gbuffer_lit(textureLoad(traced_f0,pixel,0)) {
  textureStore(traced_raw,id.xy,vec4(0u));
  return;
 }
 let normals=textureLoad(traced_normal,pixel,0);
 let normal=gbuffer_base_normal(normals);
 let geometry_normal=gbuffer_coat_normal(normals);
 var words=vec4(0u);
 for (var slot=0u;slot<RT_SHADOW_LIGHTS;slot++) {
  let key=shadow_mask_slot_key(slot);
  if key!=SHADOW_MASK_EMPTY && traced_visible(key,position,normal,geometry_normal) {
   words=traced_store(words,slot,1.);
  }
 }
 textureStore(traced_raw,id.xy,words);
}
