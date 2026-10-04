// Baked specular probe layout (scene/probes.rs), shared by every
// shader that binds a collection.
struct BakedProbe {
 world_to_local:mat4x4<f32>,
 // xyz capture position; w 1 when the proxy box corrects parallax.
 center:vec4<f32>,
 influence_min:vec4<f32>, influence_max:vec4<f32>, blend:vec4<f32>,
 proxy_min:vec4<f32>, proxy_max:vec4<f32>,
 // The influence's world-space bounding sphere (xyz centre, w radius), for
 // tiled culling (probe_culling.wgsl).
 sphere:vec4<f32>,
}
// Probes in one collection (baked_specular_probe.rs).
const MAX_PROBES:u32=256u;
struct ProbeCollection {
 counts:vec4<u32>,
 // The world grid over the probes (probe_grid.wgsl): the minimum corner of
 // cell (0,0,0), cells per metre and cells on each axis (none without probes).
 grid_origin:vec3<f32>,
 grid_scale:f32,
 grid_size:vec3<u32>,
 padding:u32,
 probes:array<BakedProbe,MAX_PROBES>,
 // The world grid's cells and their probe lists (probe_grid.wgsl,
 // scene/probe_grid.rs), in the collection's buffer.
 grid:array<u32>,
}
// Tiled culling (probe_culling.wgsl) records each tile's probes as 32-probe
// buckets, in tiles of PROBE_TILE_SIZE by PROBE_TILE_SIZE pixels.
const PROBE_BUCKETS:u32=MAX_PROBES/32u;
const PROBE_TILE_SIZE:u32=32u;
// Box projection (Lagarde and Zanuttini, "Local Image-based Lighting With
// Parallax-corrected Cubemaps", SIGGRAPH 2012): the reflected ray from the
// receiver meets the proxy box, and the probe is sampled toward that point.
fn probe_parallax_direction(world:vec3<f32>,direction:vec3<f32>,probe:BakedProbe)->vec3<f32> {
 let p=(probe.world_to_local*vec4(world,1.)).xyz;
 let d=(probe.world_to_local*vec4(direction,0.)).xyz;
 let safe=select(vec3(-1.),vec3(1.),d>=vec3(0.))*max(abs(d),vec3(0.000001));
 // Outside the proxy, project to its closest boundary, so receivers just
 // past it keep a continuous lookup instead of a seam.
 let projected=clamp(p,probe.proxy_min.xyz,probe.proxy_max.xyz);
 let t0=(probe.proxy_min.xyz-projected)/safe;
 let t1=(probe.proxy_max.xyz-projected)/safe;
 let far_axis=max(t0,t1);
 let t_far=max(0.,min(far_axis.x,min(far_axis.y,far_axis.z)));
 let rotation=mat3x3(probe.world_to_local[0].xyz,probe.world_to_local[1].xyz,probe.world_to_local[2].xyz);
 let hit=world+transpose(rotation)*(projected-p)+direction*t_far;
 let displacement=hit-probe.center.xyz;
 return displacement/max(length(displacement),0.000001);
}
// Three.js 0.185.1 PMREMUtils / PMREMNode. PMREMGenerator output is retained
// verbatim in layer zero.
fn pmrem_direction(direction:vec3<f32>,rotation:f32)->vec3<f32> {
 let c=cos(rotation);
 let s=sin(rotation);
 return vec3(c*direction.x-s*direction.z,-direction.y,s*direction.x+c*direction.z);
}
fn pmrem_mip(rough:f32)->f32 {
 if rough>=.8 {
  return (1.-rough)/.2-2.;
 }
 if rough>=.4 {
  return (.8-rough)*3./.4-1.;
 }
 if rough>=.305 {
  return (.4-rough)/.095+2.;
 }
 if rough>=.21 {
  return (.305-rough)/.095+3.;
 }
 return -2.*log2(1.16*max(rough,0.00000001));
}
fn pmrem_bilinear(map:texture_2d_array<f32>,filtering:sampler,d:vec3<f32>,mip_level:f32)->vec3<f32> {
 let a=abs(d);
 var face=0.;
 var uv=vec2(0.);
 if a.x>a.z {
  if a.x>a.y {
   face=select(3.,0.,d.x>0.);
  } else {
   face=select(4.,1.,d.y>0.);
  }
 } else {
  if a.z>a.y {
   face=select(5.,2.,d.z>0.);
  } else {
   face=select(4.,1.,d.y>0.);
  }
 }
 if face==0. {
  uv=vec2(d.z,d.y)/a.x;
 } else if face==1. {
  uv=vec2(-d.x,-d.z)/a.y;
 } else if face==2. {
  uv=vec2(-d.x,d.y)/a.z;
 } else if face==3. {
  uv=vec2(-d.z,d.y)/a.x;
 } else if face==4. {
  uv=vec2(-d.x,d.z)/a.y;
 } else {
  uv=vec2(d.x,d.y)/a.z;
 }
 let size=vec2<f32>(textureDimensions(map));
 let max_face=size.y*.25;
 let filter_mip=max(4.-mip_level,0.);
 let face_size=exp2(max(mip_level,4.));
 uv=(uv*.5+vec2(.5))*(face_size-2.)+vec2(1.);
 if face>2. {
  uv.y+=face_size;
  face-=3.;
 }
 uv.x+=face*face_size+filter_mip*48.;
 uv.y+=4.*(max_face-face_size);
 return textureSampleLevel(map,filtering,uv/size,0,0.).rgb;
}
fn pmrem_sample(map:texture_2d_array<f32>,filtering:sampler,direction:vec3<f32>,rough:f32)->vec3<f32> {
 let max_mip=log2(f32(textureDimensions(map).y)) - 2.;
 let mip=clamp(pmrem_mip(rough),-2.,max_mip);
 let whole=floor(mip);
 let fraction=fract(mip);
 let low=pmrem_bilinear(map,filtering,direction,whole);
 if fraction==0. {
  return low;
 }
 return mix(low,pmrem_bilinear(map,filtering,direction,whole+1.),fraction);
}
