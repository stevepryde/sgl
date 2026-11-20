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
// vertically, so local_shadow_cube_lookup negates cubemap_to_uv's v.
//
// The camera's soft filter (shadow_sampling.wgsl) follows Godot b130438's
// omni and spot soft shadows
// (servers/rendering/renderer_rd/shaders/scene_forward_lights_inc.glsl,
// MIT, src/LICENSE-godot.txt): the penumbra is the light's radius times
// (receiver - blocker) / blocker at the receiver, a spot's taps lie on its
// face, clamped to it, and a cube's in the plane across the direction to
// its light. The search covers the light's radius at the receiver, as
// Godot, Bevy 9d12036 (shadow_sampling.wesl) and Filament ef1a133's
// perspective path (surface_shadowing.fs) search. Changes: a cube's taps
// are looked up each in its own face of the atlas, at its own depth, where
// Godot's are directions into a dual paraboloid, and its blockers'
// distances are averaged as fractions of their taps' own, since its faces
// measure depth along different axes.
//
// The soft filter's taps follow the receiver's plane (shadow_sampling.wgsl's
// receiver plane depth bias). A spot's face is a perspective map, whose
// depth is affine in its UV across a plane, so the slope at the receiver
// is the plane's across the face (local_shadow_spot_plane_slope). A cube's
// taps are compared each at the plane's point on its own ray from the
// light (local_shadow_plane_depth), which is the same bias without a slope
// per face. An unoccluded 45 degree slope 4 m below a spot or point light
// of radius 0.7 to 1 m reads 1 (a spot 0.997 to 0.91 and a point light as
// low as 0.992 at the receiver's one depth, #841).
//
// Limit, shared with those engines. Projected from the light's centre, a
// caster b metres from a light of radius R can hide part of it from a
// receiver D metres away anywhere within R (D - b) / b of the receiver,
// which is R only at b = D / 2: a caster nearer the light than half way
// casts a penumbra wider than the search reaches, whose lit side is cut
// off about R from its hard edge (a 0.2 m spot 4 m above the floor and an
// edge 1 m below it: lit from 0.25 m out, where the penumbra reaches
// 0.6 m).

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
// The atlas rectangle a kernel's taps stay in on the face at `corner`.
fn local_shadow_bounds(shadow:LocalShadow,corner:vec2<f32>)->vec4<f32> {
 let border=LOCAL_SHADOW_BORDER/f32(textureDimensions(local_shadow_atlas).x);
 return vec4(corner+vec2(border),corner+vec2(shadow.size-border));
}
// The reversed-Z depth of a point `distance` metres along a face's axis
// from a light reaching `range`, and its inverse.
fn local_shadow_depth(shadow:LocalShadow,range:f32,distance:f32)->f32 {
 return shadow.near*(range-distance)/((range-shadow.near)*distance);
}
fn local_shadow_distance(shadow:LocalShadow,range:f32,depth:f32)->f32 {
 return shadow.near*range/(depth*(range-shadow.near)+shadow.near);
}
// Where a point is looked up in the atlas: its atlas UV, its depth, and its
// face's rectangle (local_shadow_bounds).
struct LocalShadowLookup {
 uv:vec2<f32>,
 depth:f32,
 bounds:vec4<f32>,
}
// The cube lookup of the point `frag_ls` from a cube's light reaching
// `range`, beyond its near plane: its face, where on it, and its depth.
fn local_shadow_cube_lookup(shadow:LocalShadow,range:f32,frag_ls:vec3<f32>)->LocalShadowLookup {
 let major_axis_magnitude=max(abs(frag_ls.x),max(abs(frag_ls.y),abs(frag_ls.z)));
 let uv_slice=cubemap_to_uv(frag_ls);
 let corner=local_shadow_corner(shadow,u32(uv_slice.z));
 let face_uv=vec2(uv_slice.x,1.-uv_slice.y);
 return LocalShadowLookup(corner+face_uv*shadow.size,local_shadow_depth(shadow,range,major_axis_magnitude),local_shadow_bounds(shadow,corner));
}
// The depth the plane of a receiver at `clip` on a spot's face, with
// `normal`, gains per atlas UV (receiver_plane_depth_slope).
fn local_shadow_spot_plane_slope(shadow:LocalShadow,clip:vec4<f32>,normal:vec3<f32>)->vec2<f32> {
 let across=receiver_plane_directions(normal);
 let ndc=clip.xyz/clip.w;
 let first=shadow.clip_from_world*vec4(across[0],0.);
 let second=shadow.clip_from_world*vec4(across[1],0.);
 // The face's atlas UV and depth per unit of NDC, as its lookup takes them.
 let scale=vec3(vec2(.5,-.5)*shadow.size,1.);
 // NDC's derivative along each direction, times clip.w, which both share.
 return receiver_plane_depth_slope(cross((first.xyz-ndc*first.w)*scale,(second.xyz-ndc*second.w)*scale));
}
// The soft filter on a spot's face for a light of `light_radius` metres,
// along the receiver's plane (`slope`): the blockers within the light's
// radius at the receiver, the engines' search region (the header's limit),
// and Godot's penumbra, the radius times (receiver - blocker) / blocker.
fn local_shadow_spot_soft(shadow:LocalShadow,range:f32,lookup:LocalShadowLookup,slope:vec2<f32>,pixel:vec2<f32>,light_radius:f32)->f32 {
 let receiver=local_shadow_distance(shadow,range,lookup.depth);
 // Atlas UV per metre at the receiver.
 let uv_per_metre=1./(shadow.texel_scale*receiver*vec2<f32>(textureDimensions(local_shadow_atlas)));
 let blockers=search_shadow_blockers(local_shadow_atlas,lookup.uv,lookup.depth,slope,0,lookup.bounds,pixel,light_radius*uv_per_metre);
 var penumbra=vec2(0.);
 if blockers.y>0. {
  let blocker=local_shadow_distance(shadow,range,blockers.x);
  penumbra=light_radius*(receiver-blocker)/blocker*uv_per_metre;
 }
 return sample_shadow_map_soft(local_shadow_atlas,shadow_sampler,lookup.uv,lookup.depth,slope,0,lookup.bounds,pixel,SPOT_SHADOW_TEXEL_SIZE,penumbra);
}
// The depth of the receiver's plane on the ray from a cube's light through
// a point `distance` metres along its face's axis, where the plane lies
// `nearer` the light there, as a share of the receiver's distance (below
// 0, farther): the ray meets it 1 / (1 + nearer) of the way to the point,
// or no farther than the light reaches, where the map holds nothing beyond.
fn local_shadow_plane_depth(shadow:LocalShadow,range:f32,distance:f32,nearer:f32)->f32 {
 return local_shadow_depth(shadow,range,distance/max(1.+nearer,distance/range));
}
// The soft filter of a cube for a light of `light_radius` metres, at the
// point `frag_ls` from it that `lookup` looks up, on a receiver with
// `normal`: the spiral turned in the plane across the direction to the
// light (Godot's basis), each tap looked up in its own face and compared
// at the depth of the receiver's plane on its ray. Blockers within the
// light's radius at the receiver give Godot's penumbra; within the fixed
// filter's radius, or with none, it is the fixed filter.
fn local_shadow_cube_soft(shadow:LocalShadow,range:f32,frag_ls:vec3<f32>,normal:vec3<f32>,lookup:LocalShadowLookup,pixel:vec2<f32>,light_radius:f32)->f32 {
 let direction=normalize(frag_ls);
 let v0=select(vec3(0.,1.,0.),vec3(0.,0.,1.),abs(direction.z)<.999);
 let tangent=normalize(cross(v0,direction));
 let bitangent=cross(tangent,direction);
 let rotation=random_rotation_matrix(pixel,true);
 // The share of the receiver's distance by which its plane nears the light
 // per metre across the direction to it (receiver_plane_depth_slope, in
 // the taps' frame: the two directions across, then toward the light).
 let slope=receiver_plane_depth_slope(vec3(dot(normal,tangent),dot(normal,bitangent),-dot(normal,direction)))/length(frag_ls);
 // Each blocker's distance from the light over its tap's, along the tap's
 // face's axis, and how many.
 var fraction=vec2(0.);
 for(var tap=0u;tap<SOFT_SHADOW_TAPS;tap++) {
  let disc=(rotation*spiral_offset(tap))*light_radius;
  let tap_lookup=local_shadow_cube_lookup(shadow,range,frag_ls+tangent*disc.x+bitangent*disc.y);
  let across=local_shadow_distance(shadow,range,tap_lookup.depth);
  let blocker=shadow_blocker_texel(local_shadow_atlas,tap_lookup.uv,local_shadow_plane_depth(shadow,range,across,dot(slope,disc)),0,tap_lookup.bounds);
  if blocker.y>0. {
   fraction+=vec2(local_shadow_distance(shadow,range,blocker.x)/across,1.);
  }
 }
 // The radii in metres at the receiver.
 let metres_per_uv=shadow.texel_scale*local_shadow_distance(shadow,range,lookup.depth)*f32(textureDimensions(local_shadow_atlas).x);
 let fixed=calculate_uv_offset_scale_jimenez_fourteen(local_shadow_atlas,SPOT_SHADOW_TEXEL_SIZE,1.).x*metres_per_uv;
 var penumbra=0.;
 if fraction.y>0. {
  let blocker=fraction.x/fraction.y;
  penumbra=light_radius*(1.-blocker)/blocker;
 }
 let radius=soft_shadow_radius(vec2(fixed),vec2(penumbra)).x;
 if radius<=fixed {
  return sample_shadow_map(local_shadow_atlas,shadow_sampler,lookup.uv,lookup.depth,0,lookup.bounds,pixel,SPOT_SHADOW_TEXEL_SIZE,SHADOW_FILTER_SOFT);
 }
 var sum=0.;
 for(var tap=0u;tap<SOFT_SHADOW_TAPS;tap++) {
  let disc=(rotation*spiral_offset(tap))*radius;
  let tap_lookup=local_shadow_cube_lookup(shadow,range,frag_ls+tangent*disc.x+bitangent*disc.y);
  let across=local_shadow_distance(shadow,range,tap_lookup.depth);
  sum+=sample_shadow_map_hardware(local_shadow_atlas,shadow_sampler,tap_lookup.uv,local_shadow_plane_depth(shadow,range,across,dot(slope,disc)),0,tap_lookup.bounds);
 }
 return sum/f32(SOFT_SHADOW_TAPS);
}
// The fraction of scene light `index`, at `light_position`, `light_radius`
// metres in radius and reaching `range`, that reaches `receiver`
// (SHADOW_RECEIVER_*) at `world` with geometry normal `normal` past its
// shadow's casters; 1 for a light without a shadow. The camera's surfaces
// and fog sample the frame's atlas at `pixel`; probe captures and ray hits
// sample its static layers, where it has them. The camera's soft filter
// widens to the penumbra of a light of nonzero radius.
fn local_shadow_visibility(index:u32,light_position:vec3<f32>,light_radius:f32,range:f32,world:vec3<f32>,normal:vec3<f32>,pixel:vec2<f32>,receiver:u32)->f32 {
 if index>=arrayLength(&local_shadows) {
  return 1.;
 }
 let shadow=local_shadows[index];
 if shadow.kind==LOCAL_SHADOW_NONE || (receiver==SHADOW_RECEIVER_CAPTURE && shadow.layers==0u) {
  return 1.;
 }
 let surface_to_light=light_position-world;
 let toward_light=normalize(surface_to_light);
 let shadow_filtering=shadow_filter(receiver);
 let soft=shadow_filtering==SHADOW_FILTER_SOFT && light_radius>0.;
 var lookup:LocalShadowLookup;
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
  lookup=local_shadow_cube_lookup(shadow,range,frag_ls);
  if soft {
   return local_shadow_cube_soft(shadow,range,frag_ls,normal,lookup,pixel,light_radius);
  }
 } else {
  // The spot face's w is the distance along the spot's direction.
  let distance_to_light=(shadow.clip_from_world*vec4(world,1.)).w;
  let offset_position=shadow_receiver_offset(world,normal,toward_light,shadow.texel_scale*distance_to_light,SPOT_SHADOW_NORMAL_BIAS,SPOT_SHADOW_DEPTH_BIAS);
  let clip=shadow.clip_from_world*vec4(offset_position,1.);
  if clip.w<=0. {
   return 1.;
  }
  let ndc=clip.xyz/clip.w;
  let face_uv=ndc.xy*vec2(.5,-.5)+vec2(.5);
  let corner=local_shadow_corner(shadow,0u);
  lookup=LocalShadowLookup(corner+face_uv*shadow.size,ndc.z,local_shadow_bounds(shadow,corner));
  if soft {
   return local_shadow_spot_soft(shadow,range,lookup,local_shadow_spot_plane_slope(shadow,clip,normal),pixel,light_radius);
  }
 }
 return sample_shadow_map(local_shadow_atlas,shadow_sampler,lookup.uv,lookup.depth,0,lookup.bounds,pixel,SPOT_SHADOW_TEXEL_SIZE,shadow_filtering);
}
