// World-space reflection rays for what screen-space reflections cannot see:
// moving objects, which baked probes cannot hold, or everything. Ported
// from Wicked Engine (revision 2ff1d9e7b36091d6edf9f823af77e6bc9af20e3b,
// MIT, see LICENSE-wicked.txt): shaders/rtreflectionCS.hlsl, traced through
// the scene ray function set (the portable scene BVH, as ddgi_raytraceCS.hlsl
// traces Wicked's software BVH, or the hardware path's TLAS), with
// ReflectionDir_GGX, ImportanceSampleVisibleGGX, GetTangentBasis and
// SampleDisk from stochasticSSRHF.hlsli. Modified: translated to WGSL; hits
// are shaded as raster shades surfaces (surface_ray.wgsl); rays test all
// scene geometry and reach what `Settings::world_space_reflections` says:
// `Moving`, the closest moving hit, then static any-hit visibility to that
// hit; `All`, the closest hit of either kind, as Wicked's ray traces every
// instance in its reflection mask (rtreflectionCS.hlsl 74–81).
// Every sample reads the same full-resolution receiver pixel (Wicked samples
// depth at a differently offset UV); hashes replace the blue-noise texture;
// a miss stores no radiance and no coverage (a = 0) and a zero length, so
// the composition keeps probe and sky specular there. Which tracing pixels
// trace is the classification's (world_reflections_classify.wgsl), which
// lists them and writes every pixel's miss first; the trace runs over its
// list, a ray a thread, as FidelityFX SSSR intersects its ray list and the
// AMD FidelityFX SDK 1.1.4 (revision
// c6efa6bf7f2027b3ec94f28578bb5965eabb9e55, MIT, see
// LICENSE-amd-fidelityfx.txt) Hybrid Reflections sample traces its hardware
// rays from theirs (samples/hybridreflections/shaders/Intersect.hlsl
// 273–300).
// Raster identity of each receiver's triangle, excluded from its own ray.
@group(3) @binding(6) var world_source_id:texture_2d<u32>;
// The classification's ray list (world_ray_texel) and the trace's targets.
@group(3) @binding(9) var world_rays:texture_2d<u32>;
@group(3) @binding(10) var world_indirect:texture_storage_2d<rgba16float,write>;
@group(3) @binding(11) var world_direction_pdf:texture_storage_2d<rgba16float,write>;
@group(3) @binding(12) var world_length:texture_storage_2d<r32float,write>;

// The rays reach static geometry too (`WorldSpaceReflections::All`), else
// moving objects alone.
override world_reach_all:bool;

// Bias used on the GGX importance sample when denoising, to remove part of the
// tail that creates much more noise.
const WORLD_GGX_IMPORTANCE_SAMPLE_BIAS:f32=0.1;

// Duff et al. 2017, "Building an Orthonormal Basis, Revisited".
fn world_tangent_basis(z:vec3<f32>)->mat3x3<f32> {
 let s=select(-1.,1.,z.z>=0.);
 let a=-1./(s+z.z);
 let b=z.x*z.y*a;
 return mat3x3(vec3(1.+s*a*z.x*z.x,s*b,-s*z.x),vec3(b,s+a*z.y*z.y,-z.y),z);
}
fn world_sample_disk(xi:vec2<f32>)->vec2<f32> {
 let theta=2.*WORLD_PI*xi.x;
 let radius=sqrt(xi.y);
 return radius*vec2(cos(theta),sin(theta));
}
// Heitz 2018, "Sampling the GGX Distribution of Visible Normals".
fn world_sample_visible_ggx(disk:vec2<f32>,roughness:f32,v:vec3<f32>)->vec4<f32> {
 let alpha=clamp(roughness,WORLD_MIN_ROUGHNESS,1.)*clamp(roughness,WORLD_MIN_ROUGHNESS,1.);
 let alpha2=alpha*alpha;
 let vh=normalize(vec3(alpha*v.xy,v.z));
 let t0=select(vec3(1.,0.,0.),normalize(cross(vec3(0.,0.,1.),vh)),vh.z<0.9999);
 let t1=cross(vh,t0);
 var p=disk;
 let s=.5+.5*vh.z;
 p.y=(1.-s)*sqrt(1.-p.x*p.x)+s*p.y;
 var h=p.x*t0+p.y*t1+sqrt(clamp(1.-dot(p,p),0.,1.))*vh;
 h=normalize(vec3(alpha*h.xy,max(0.,h.z)));
 let nv=v.z;
 let nh=h.z;
 let vh_dot=dot(v,h);
 let f=(nh*alpha2-nh)*nh+1.;
 let d=alpha2/(WORLD_PI*f*f);
 let masking=2.*nv/(sqrt(nv*(nv-nv*alpha2)+alpha2)+nv);
 return vec4(h,masking*vh_dot*d/nv);
}
fn world_reflection_ggx(v:vec3<f32>,n:vec3<f32>,roughness_in:f32,random:vec2<f32>)->vec4<f32> {
 let roughness=clamp(roughness_in,WORLD_MIN_ROUGHNESS,1.);
 if roughness>.05 {
  let basis=world_tangent_basis(n);
  let tangent_v=transpose(basis)*v;
  var xi=random;
  xi.y=mix(xi.y,0.,WORLD_GGX_IMPORTANCE_SAMPLE_BIAS);
  let h=world_sample_visible_ggx(world_sample_disk(xi),roughness,tangent_v);
  return vec4(reflect(-v,basis*h.xyz),h.w);
 }
 return vec4(reflect(-v,n),1.);
}
struct WorldRay {
 indirect:vec4<f32>,
 direction_pdf:vec4<f32>,
 length:f32,
}
// The ray of the tracing pixel `tracing`, which the classification listed.
fn world_trace_ray(tracing:vec2<u32>)->WorldRay {
 var output=WorldRay(vec4(0.),vec4(0.),0.);
 let pixel=world_traced_pixel(tracing);
 let receiver=world_receiver(pixel);
 let uv=(vec2<f32>(pixel)+.5)*world.full.zw;
 let p=world_position(uv,receiver.depth);
 let v=normalize(world.eye.xyz-p);
 let ggx=world_reflection_ggx(v,receiver.normal,receiver.roughness,world_random(tracing,world.frame+1u));
 let direction=normalize(ggx.xyz);
 output.direction_pdf=vec4(direction,ggx.w);
 let ray=SceneRay(vec4(p,.01),vec4(direction,world.range));
 let receiver_id=textureLoad(world_source_id,pixel,0).xy;
 var raw:RawSceneHit;
 if world_reach_all {
  raw=scene_trace_nearest_except_receiver(ray,receiver_id);
 } else {
  // Moving misses need no static traversal: probes/sky already supply fallback.
  raw=scene_trace_moving_except_receiver(ray,receiver_id);
 }
 if raw.intersection.x==0u {
  return output;
 }
 // Under `Moving`, a static blocker before the closest moving hit leaves
 // probes/sky in charge.
 let segment=SceneRay(ray.origin,vec4(direction,raw.coords.x));
 if !world_reach_all && !scene_static_segment_visible_except_receiver(segment,receiver_id) {
  return output;
 }
 let hit=scene_decode_hit(raw,p,direction);
 output.indirect=vec4(shade_ray_hit(hit,-direction,SHADOW_RECEIVER_CAPTURE,vec3(0.)),1.);
 output.length=hit.distance;
 return output;
}
@compute @workgroup_size(WORLD_TRACE_THREADS)
fn world_trace(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
 let index=(group.y*WORLD_GROUP_ROW+group.x)*WORLD_TRACE_THREADS+lane;
 // A ray a tracing pixel at most, whatever the count holds (AR-12).
 if index>=min(world.rays,u32(world.reduced.x)*u32(world.reduced.y)) {
  return;
 }
 let tracing=world_unpack_ray(textureLoad(world_rays,world_ray_texel(index),0).x);
 let ray=world_trace_ray(tracing);
 textureStore(world_indirect,tracing,ray.indirect);
 textureStore(world_direction_pdf,tracing,ray.direction_pdf);
 textureStore(world_length,tracing,vec4(ray.length));
}
