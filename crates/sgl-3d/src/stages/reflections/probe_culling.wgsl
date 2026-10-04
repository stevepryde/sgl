// Tiled culling of the baked specular probe collection: the probe portion of
// Wicked Engine's lightCullingCS.hlsl (MIT, Turanszki Janos, commit 4323a33)
// with its ADVANCED 2.5D depth mask (Harada et al., "Forward+", SIGGRAPH
// 2012), which Wicked enables by default. Each tile of BLOCKSIZE by BLOCKSIZE
// pixels (Wicked's TILED_CULLING_BLOCKSIZE) records the probes whose influence
// can reach its pixels as PROBE_BUCKETS 32-probe buckets (Wicked's
// SHADER_ENTITY_TILE_BUCKET_COUNT).
// Divergences: WebGPU guarantees no wave intrinsics, so each thread performs
// Wicked's per-wave atomics; SGL's infinite reversed-Z far plane puts sky depth
// (0) at infinity, so sky pixels, which take no probes, stay out of the depth
// bounds, and a tile of sky alone takes none.
struct CullingCamera {
 // World to Wicked's left-handed view space (x right, y up, z forward), back,
 // and clip to that view space.
 view:mat4x4<f32>,
 inverse_view:mat4x4<f32>,
 inverse_projection:mat4x4<f32>,
 // x: the near clip plane's view depth.
 z_near:vec4<f32>,
}
@group(0) @binding(0) var depth:texture_depth_2d;
@group(0) @binding(1) var<uniform> camera:CullingCamera;
@group(0) @binding(2) var<storage,read> collection:ProbeCollection;
@group(0) @binding(3) var<storage,read_write> tiles:array<u32>;
const BLOCKSIZE:u32=PROBE_TILE_SIZE;
const THREADSIZE:u32=8u;
const GRANULARITY:u32=4u;
const BUCKETS:u32=PROBE_BUCKETS;
const FLT_EPSILON:f32=1.192092896e-07;
var<workgroup> min_depth:atomic<u32>;
var<workgroup> max_depth:atomic<u32>;
var<workgroup> depth_mask:atomic<u32>;
var<workgroup> tile:array<atomic<u32>,BUCKETS>;

fn construct_entity_mask(depth_range_min:f32,depth_range_recip:f32,center_z:f32,radius:f32)->u32 {
 let f_min=center_z-radius;
 let f_max=center_z+radius;
 let start=u32(clamp(floor((f_min-depth_range_min)*depth_range_recip),0.,31.));
 let end=u32(clamp(floor((f_max-depth_range_min)*depth_range_recip),0.,31.));
 var mask=0xffffffffu;
 mask>>=31u-(end-start);
 mask<<=start;
 return mask;
}
struct Plane {
 n:vec3<f32>,
 d:f32,
}
fn compute_plane(p0:vec3<f32>,p1:vec3<f32>,p2:vec3<f32>)->Plane {
 let v0=p1-p0;
 let v2=p2-p0;
 let n=normalize(cross(v0,v2));
 return Plane(n,dot(n,p0));
}
fn screen_to_view(screen:vec4<f32>,dim_rcp:vec2<f32>)->vec3<f32> {
 let tex=screen.xy*dim_rcp;
 let clip=vec4(vec2(tex.x,1.-tex.y)*2.-1.,screen.z,screen.w);
 let view=camera.inverse_projection*clip;
 return view.xyz/view.w;
}
fn sphere_inside_plane(center:vec3<f32>,radius:f32,plane:Plane)->bool {
 return dot(plane.n,center)-plane.d < -radius;
}
fn sphere_inside_frustum(center:vec3<f32>,radius:f32,planes:array<Plane,4>,z_near:f32,z_far:f32)->bool {
 var result=!(center.z+radius<z_near || center.z-radius>z_far);
 result=result && !sphere_inside_plane(center,radius,planes[0]);
 result=result && !sphere_inside_plane(center,radius,planes[1]);
 result=result && !sphere_inside_plane(center,radius,planes[2]);
 result=result && !sphere_inside_plane(center,radius,planes[3]);
 return result;
}
struct Aabb {
 c:vec3<f32>,
 e:vec3<f32>,
}
fn intersect_aabb(a:Aabb,b:Aabb)->bool {
 return all(abs(a.c-b.c)<=a.e+b.e);
}
fn aabb_from_min_max(lo:vec3<f32>,hi:vec3<f32>)->Aabb {
 let c=(lo+hi)*0.5;
 return Aabb(c,abs(hi-c));
}
fn aabb_transform(aabb:Aabb,m:mat4x4<f32>)->Aabb {
 let lo=aabb.c-aabb.e;
 let hi=aabb.c+aabb.e;
 var a=vec3(1000000.);
 var b=vec3(-1000000.);
 for(var i=0u;i<8u;i++) {
  let corner=select(lo,hi,vec3((i&1u)!=0u,(i&2u)!=0u,(i&4u)!=0u));
  let p=(m*vec4(corner,1.)).xyz;
  a=min(a,p);
  b=max(b,p);
 }
 return aabb_from_min_max(a,b);
}

@compute @workgroup_size(8,8,1)
fn main(@builtin(workgroup_id) gid:vec3<u32>,@builtin(global_invocation_id) dtid:vec3<u32>,@builtin(local_invocation_index) group_index:u32) {
 let dim=textureDimensions(depth);
 let dim_rcp=1./vec2<f32>(dim);
 if group_index<BUCKETS {
  atomicStore(&tile[group_index],0u);
 }
 if group_index==0u {
  atomicStore(&min_depth,0xffffffffu);
  atomicStore(&max_depth,0u);
  atomicStore(&depth_mask,0u);
 }
 // Min and max depth in the tile.
 var depths:array<f32,16>;
 var depth_min_unrolled=1.;
 var depth_max_unrolled=0.;
 for(var g=0u;g<GRANULARITY*GRANULARITY;g++) {
  let pixel=min(dtid.xy*GRANULARITY+vec2(g%GRANULARITY,g/GRANULARITY),dim-1u);
  depths[g]=textureLoad(depth,vec2<i32>(pixel),0);
  if depths[g]>0. {
   depth_min_unrolled=min(depth_min_unrolled,depths[g]);
   depth_max_unrolled=max(depth_max_unrolled,depths[g]);
  }
 }
 workgroupBarrier();
 atomicMin(&min_depth,bitcast<u32>(depth_min_unrolled));
 atomicMax(&max_depth,bitcast<u32>(depth_max_unrolled));
 workgroupBarrier();
 let geometry=atomicLoad(&max_depth)!=0u;
 // Reversed depth.
 let f_min_depth=saturate(bitcast<f32>(atomicLoad(&max_depth))+FLT_EPSILON);
 // The far bound stays finite under the infinite far plane.
 let f_max_depth=max(bitcast<f32>(atomicLoad(&min_depth))-FLT_EPSILON,FLT_EPSILON);

 // View space frustum corners, near then far: top left, top right, bottom
 // left, bottom right.
 var view_space:array<vec3<f32>,8>;
 for(var i=0u;i<8u;i++) {
  let corner=vec2<f32>(gid.xy+vec2(i&1u,(i>>1u)&1u))*f32(BLOCKSIZE);
  view_space[i]=screen_to_view(vec4(corner,select(f_min_depth,f_max_depth,i>=4u),1.),dim_rcp);
 }
 // Left, right, top, bottom.
 let planes=array(
  compute_plane(view_space[2],view_space[0],view_space[4]),
  compute_plane(view_space[1],view_space[3],view_space[5]),
  compute_plane(view_space[0],view_space[1],view_space[4]),
  compute_plane(view_space[3],view_space[2],view_space[6]),
 );
 // An AABB around the min-max depth bounds for tighter culling; the frustum
 // is asymmetric, so it takes every corner.
 var min_aabb=vec3(10000000.);
 var max_aabb=vec3(-10000000.);
 for(var i=0u;i<8u;i++) {
  min_aabb=min(min_aabb,view_space[i]);
  max_aabb=max(max_aabb,view_space[i]);
 }
 let group_aabb_ws=aabb_transform(aabb_from_min_max(min_aabb,max_aabb),camera.inverse_view);
 let min_depth_vs=view_space[0].z;
 let max_depth_vs=view_space[4].z;
 let near_clip_vs=camera.z_near.x;

 // Occupied slices of 32 between the depth bounds, in view space.
 let depth_range_recip=31./(max_depth_vs-min_depth_vs);
 var depth_mask_unrolled=0u;
 for(var g=0u;g<GRANULARITY*GRANULARITY;g++) {
  if depths[g]>0. {
   let real_depth_vs=screen_to_view(vec4(0.,0.,depths[g],1.),dim_rcp).z;
   let cell=u32(clamp(floor((real_depth_vs-min_depth_vs)*depth_range_recip),0.,31.));
   depth_mask_unrolled|=1u<<cell;
  }
 }
 atomicOr(&depth_mask,depth_mask_unrolled);
 workgroupBarrier();
 let occupied=atomicLoad(&depth_mask);

 if geometry {
  for(var i=group_index;i<collection.counts.x;i+=THREADSIZE*THREADSIZE) {
   let probe=collection.probes[i];
   let center=(camera.view*vec4(probe.sphere.xyz,1.)).xyz;
   let radius=probe.sphere.w;
   if sphere_inside_frustum(center,radius,planes,near_clip_vs,max_depth_vs) {
    // The tile's world-space AABB in the space of the probe's influence box.
    let b=aabb_transform(group_aabb_ws,probe.world_to_local);
    let a=aabb_from_min_max(probe.influence_min.xyz,probe.influence_max.xyz);
    if intersect_aabb(a,b) && (occupied&construct_entity_mask(min_depth_vs,depth_range_recip,center.z,radius))!=0u {
     atomicOr(&tile[i/32u],1u<<(i%32u));
    }
   }
  }
 }
 workgroupBarrier();
 let tiles_x=(dim.x+BLOCKSIZE-1u)/BLOCKSIZE;
 if group_index<BUCKETS {
  tiles[(gid.y*tiles_x+gid.x)*BUCKETS+group_index]=atomicLoad(&tile[group_index]);
 }
}
