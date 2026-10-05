// Local-light shadows: a scene light's faces in the local-light shadow atlas
// (`local_shadow_atlas`), where `local_shadows` places them, filtered with
// the shared shadow filters (shadow_sampling.wgsl). Reads `frame`,
// `local_shadows`, `local_shadow_atlas` and `shadow_sampler`.
//
// The receiver offsets are Bevy 9d12036's crates/bevy_pbr/src/render/
// shadows.wesl fetch_point_shadow and fetch_spot_shadow, MIT OR Apache-2.0
// (src/LICENSE-bevy.txt), through shadow_receiver_offset. Changes: a cube's
// faces lie in the 2D atlas rather than a cube map, so, as Wicked Engine
// filters its atlas's cube faces, they take the 2D filters and with them
// Bevy's spot biases and texel size, which Bevy tunes for those filters;
// Bevy's point biases go with its cube-map filter. The cube's depth is the
// faces' finite reversed-Z projection rather than an infinite one.
//
// A cube's face lookup is Wicked Engine 4323a33's
// WickedEngine/shaders/globals.hlsli cubemap_to_uv, and its clamp to the
// face's texels shadowHF.hlsli's shadow_border_clamp, MIT
// (src/LICENSE-wicked.txt). Wicked's faces are left-handed views; SGL's are
// right-handed (View::local_shadow_face), which store each face flipped
// vertically, so local_shadow_visibility negates cubemap_to_uv's v.

// Bevy's SpotLight::DEFAULT_SHADOW_DEPTH_BIAS, in metres toward the light.
const SPOT_SHADOW_DEPTH_BIAS:f32=0.02;
// Bevy's SpotLight::DEFAULT_SHADOW_NORMAL_BIAS, 1.8 texels along the
// normal, times SQRT_2 for the worst-case diagonal offset
// (crates/bevy_pbr/src/render/light.rs).
const SPOT_SHADOW_NORMAL_BIAS:f32=2.5455844;
// Bevy's SPOT_SHADOW_TEXEL_SIZE (shadow_sampling.wesl): the texel size its
// spot lights' temporal filter takes.
const SPOT_SHADOW_TEXEL_SIZE:f32=0.0134277345;
// Wicked's shadow_border_clamp border, in atlas texels: a kernel's taps
// stay this far inside their face.
const LOCAL_SHADOW_BORDER:f32=0.75;

// Wicked's cubemap_to_uv, with its names and order: the face a direction
// from a cube's light falls in (z, View::local_shadow_face's order) and
// where on the face (xy in [0,1], with v as Wicked's left-handed faces
// store it).
fn cubemap_to_uv(r:vec3<f32>)->vec3<f32> {
 var faceIndex=0.;
 let absr=abs(r);
 var uvw=vec3(0.);
 if absr.x>absr.y && absr.x>absr.z {
  // x major
  let negx=step(r.x,0.);
  uvw=vec3(r.zy,absr.x)*vec3(mix(-1.,1.,negx),-1.,1.);
  faceIndex=negx;
 } else if absr.y>absr.z {
  // y major
  let negy=step(r.y,0.);
  uvw=vec3(r.xz,absr.y)*vec3(1.,mix(1.,-1.,negy),1.);
  faceIndex=2.+negy;
 } else {
  // z major
  let negz=step(r.z,0.);
  uvw=vec3(r.xy,absr.z)*vec3(mix(1.,-1.,negz),-1.,1.);
  faceIndex=4.+negz;
 }
 return vec3((uvw.xy/uvw.z+1.)*.5,faceIndex);
}
// The atlas UV of face `face`'s top-left corner in a light's shadow.
fn local_shadow_corner(shadow:LocalShadow,face:u32)->vec2<f32> {
 let pair=shadow.corners[face/2u];
 return select(pair.xy,pair.zw,(face&1u)==1u);
}
// The fraction of scene light `index`, at `light_position` and reaching
// `range`, that reaches `receiver` (SHADOW_RECEIVER_*) at `world` with
// geometry normal `normal` past its shadow's casters; 1 for a light without
// a shadow. The camera's surfaces and fog sample the frame's atlas at
// `pixel`; probe captures and ray hits sample its static layers, where it
// has them.
fn local_shadow_visibility(index:u32,light_position:vec3<f32>,range:f32,world:vec3<f32>,normal:vec3<f32>,pixel:vec2<f32>,receiver:u32)->f32 {
 if index>=arrayLength(&local_shadows) {
  return 1.;
 }
 let shadow=local_shadows[index];
 if shadow.kind==LOCAL_SHADOW_NONE || (receiver==SHADOW_RECEIVER_CAPTURE && shadow.layers==0u) {
  return 1.;
 }
 let surface_to_light=light_position-world;
 let toward_light=normalize(surface_to_light);
 var face_uv=vec2(0.);
 var face=0u;
 var depth=0.;
 if shadow.kind==LOCAL_SHADOW_CUBE {
  // The texel size grows with the distance along the face's axis.
  let distance_to_light=max(abs(surface_to_light.x),max(abs(surface_to_light.y),abs(surface_to_light.z)));
  let offset_position=shadow_receiver_offset(world,normal,toward_light,shadow.texel_scale*distance_to_light,SPOT_SHADOW_NORMAL_BIAS,SPOT_SHADOW_DEPTH_BIAS);
  let frag_ls=offset_position-light_position;
  let major_axis_magnitude=max(abs(frag_ls.x),max(abs(frag_ls.y),abs(frag_ls.z)));
  // A receiver inside the near plane has no represented occlusion interval.
  if major_axis_magnitude<=shadow.near {
   return 1.;
  }
  let uv_slice=cubemap_to_uv(frag_ls);
  face_uv=vec2(uv_slice.x,1.-uv_slice.y);
  face=u32(uv_slice.z);
  depth=shadow.near*(range-major_axis_magnitude)/((range-shadow.near)*major_axis_magnitude);
 } else {
  // The spot face's w is the distance along the spot's direction.
  let distance_to_light=(shadow.clip_from_world*vec4(world,1.)).w;
  let offset_position=shadow_receiver_offset(world,normal,toward_light,shadow.texel_scale*distance_to_light,SPOT_SHADOW_NORMAL_BIAS,SPOT_SHADOW_DEPTH_BIAS);
  let clip=shadow.clip_from_world*vec4(offset_position,1.);
  if clip.w<=0. {
   return 1.;
  }
  let ndc=clip.xyz/clip.w;
  face_uv=ndc.xy*vec2(.5,-.5)+vec2(.5);
  depth=ndc.z;
 }
 let corner=local_shadow_corner(shadow,face);
 let border=LOCAL_SHADOW_BORDER/f32(textureDimensions(local_shadow_atlas).x);
 let bounds=vec4(corner+vec2(border),corner+vec2(shadow.size-border));
 let uv=corner+face_uv*shadow.size;
 return sample_shadow_map(local_shadow_atlas,shadow_sampler,uv,depth,0,bounds,pixel,SPOT_SHADOW_TEXEL_SIZE,shadow_filter(receiver));
}
