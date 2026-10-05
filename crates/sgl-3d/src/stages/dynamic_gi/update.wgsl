// The dynamic GI stage's blends: each probe that traced rays this frame
// gathers them into its irradiance map through each texel's estimator, and
// into its depth map, moving the probe away from surfaces it nears. One
// workgroup per probe that traced; the others keep what they hold.
//
// Ports Wicked Engine df44c3db4c4927492bc9c791eac715d98d7ed091,
// WickedEngine/shaders/ddgi_updateCS.hlsl (the ray cache loop 107–165, the
// depth weight 144–147, the depth blend of 0.02 173–177 and its border copy
// 183–189, relocation 136–139 and 206–211; DDGI_DEPTH_BORDER_OFFSETS in
// ShaderInterop_DDGI.h) and ddgi_updateCS_depth.hlsl, with revision
// 95e357f73f28d24e70ce7a1ab8f9fb954de457e4's irradiance map (ddgi_updateCS
// 215–234: each 6x6 texel's estimator and the border copy by
// DDGI_COLOR_BORDER_OFFSETS), MIT (src/LICENSE-wicked.txt). Not taken: the
// voxel grid's nudge in relocation (195–204), as SGL3D has no voxel grid,
// and the colour map's BC6H compression (248), as the sample reads what the
// blend writes. Changed: a probe not yet blended starts its estimator,
// depth and offset afresh, where Wicked starts every probe on its first
// frame; each map's history is the stage's buffers, which the blend writes
// on into the probe texture.
@group(0) @binding(0) var<uniform> volume:DdgiVolume;
@group(0) @binding(1) var<storage,read> ray_counts:array<u32>;
@group(0) @binding(2) var ray_results:texture_2d<u32>;
@group(0) @binding(3) var<storage,read_write> variance:array<u32>;
@group(0) @binding(4) var<storage,read_write> depth_history:array<u32>;
@group(0) @binding(5) var<storage,read_write> probe_states:array<vec2<u32>>;
@group(0) @binding(6) var probes_out:texture_storage_2d<rgba16float,write>;
@group(0) @binding(7) var<storage,read> traced_probes:array<u32>;
// The probe a workgroup of the blends' dispatch over the probes that trace
// blends, or none past them.
fn traced_probe(group:vec3<u32>)->u32 {
 let slot=ddgi_group_probe(group);
 if slot>=volume.traced {
  return volume.probe_count;
 }
 return traced_probes[slot];
}
const DDGI_COLOR_BORDER_OFFSETS=array<vec4<u32>,28>(
 vec4(6u,1u,1u,0u),
 vec4(5u,1u,2u,0u),
 vec4(4u,1u,3u,0u),
 vec4(3u,1u,4u,0u),
 vec4(2u,1u,5u,0u),
 vec4(1u,1u,6u,0u),
 vec4(6u,6u,1u,7u),
 vec4(5u,6u,2u,7u),
 vec4(4u,6u,3u,7u),
 vec4(3u,6u,4u,7u),
 vec4(2u,6u,5u,7u),
 vec4(1u,6u,6u,7u),
 vec4(1u,1u,0u,6u),
 vec4(1u,2u,0u,5u),
 vec4(1u,3u,0u,4u),
 vec4(1u,4u,0u,3u),
 vec4(1u,5u,0u,2u),
 vec4(1u,6u,0u,1u),
 vec4(6u,1u,7u,6u),
 vec4(6u,2u,7u,5u),
 vec4(6u,3u,7u,4u),
 vec4(6u,4u,7u,3u),
 vec4(6u,5u,7u,2u),
 vec4(6u,6u,7u,1u),
 vec4(1u,1u,7u,7u),
 vec4(6u,1u,0u,7u),
 vec4(1u,6u,7u,0u),
 vec4(6u,6u,0u,0u),
);
const DDGI_DEPTH_BORDER_OFFSETS=array<vec4<u32>,68>(
 vec4(16u,1u,1u,0u),
 vec4(15u,1u,2u,0u),
 vec4(14u,1u,3u,0u),
 vec4(13u,1u,4u,0u),
 vec4(12u,1u,5u,0u),
 vec4(11u,1u,6u,0u),
 vec4(10u,1u,7u,0u),
 vec4(9u,1u,8u,0u),
 vec4(8u,1u,9u,0u),
 vec4(7u,1u,10u,0u),
 vec4(6u,1u,11u,0u),
 vec4(5u,1u,12u,0u),
 vec4(4u,1u,13u,0u),
 vec4(3u,1u,14u,0u),
 vec4(2u,1u,15u,0u),
 vec4(1u,1u,16u,0u),
 vec4(16u,16u,1u,17u),
 vec4(15u,16u,2u,17u),
 vec4(14u,16u,3u,17u),
 vec4(13u,16u,4u,17u),
 vec4(12u,16u,5u,17u),
 vec4(11u,16u,6u,17u),
 vec4(10u,16u,7u,17u),
 vec4(9u,16u,8u,17u),
 vec4(8u,16u,9u,17u),
 vec4(7u,16u,10u,17u),
 vec4(6u,16u,11u,17u),
 vec4(5u,16u,12u,17u),
 vec4(4u,16u,13u,17u),
 vec4(3u,16u,14u,17u),
 vec4(2u,16u,15u,17u),
 vec4(1u,16u,16u,17u),
 vec4(1u,16u,0u,1u),
 vec4(1u,15u,0u,2u),
 vec4(1u,14u,0u,3u),
 vec4(1u,13u,0u,4u),
 vec4(1u,12u,0u,5u),
 vec4(1u,11u,0u,6u),
 vec4(1u,10u,0u,7u),
 vec4(1u,9u,0u,8u),
 vec4(1u,8u,0u,9u),
 vec4(1u,7u,0u,10u),
 vec4(1u,6u,0u,11u),
 vec4(1u,5u,0u,12u),
 vec4(1u,4u,0u,13u),
 vec4(1u,3u,0u,14u),
 vec4(1u,2u,0u,15u),
 vec4(1u,1u,0u,16u),
 vec4(16u,16u,17u,1u),
 vec4(16u,15u,17u,2u),
 vec4(16u,14u,17u,3u),
 vec4(16u,13u,17u,4u),
 vec4(16u,12u,17u,5u),
 vec4(16u,11u,17u,6u),
 vec4(16u,10u,17u,7u),
 vec4(16u,9u,17u,8u),
 vec4(16u,8u,17u,9u),
 vec4(16u,7u,17u,10u),
 vec4(16u,6u,17u,11u),
 vec4(16u,5u,17u,12u),
 vec4(16u,4u,17u,13u),
 vec4(16u,3u,17u,14u),
 vec4(16u,2u,17u,15u),
 vec4(16u,1u,17u,16u),
 vec4(16u,16u,0u,0u),
 vec4(1u,16u,17u,0u),
 vec4(16u,1u,0u,17u),
 vec4(1u,1u,17u,17u),
);
// Ray `ray` of probe `probe` in the ray results.
fn ddgi_load_ray(probe:u32,ray:u32)->DdgiRay {
 return ddgi_unpack_ray(textureLoad(ray_results,ddgi_ray_texel(probe*volume.max_rays+ray),0));
}
fn ddgi_load_variance(index:u32)->DdgiVariance {
 let at=index*DDGI_VARIANCE_WORDS;
 return ddgi_unpack_variance(array<u32,6>(variance[at],variance[at+1u],variance[at+2u],variance[at+3u],variance[at+4u],variance[at+5u]));
}
fn ddgi_store_variance(index:u32,data:DdgiVariance) {
 let at=index*DDGI_VARIANCE_WORDS;
 let words=ddgi_pack_variance(data);
 for (var word=0u;word<DDGI_VARIANCE_WORDS;word++) {
  variance[at+word]=words[word];
 }
}
const IRRADIANCE_THREADS:u32=8u;
const IRRADIANCE_CACHE:u32=IRRADIANCE_THREADS*IRRADIANCE_THREADS;
var<workgroup> irradiance_ray_count:u32;
var<workgroup> irradiance_cache:array<DdgiRay,IRRADIANCE_CACHE>;
var<workgroup> shared_texels:array<vec3<f32>,IRRADIANCE_CACHE>;
@compute @workgroup_size(IRRADIANCE_THREADS,IRRADIANCE_THREADS)
fn update_irradiance(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_id) thread:vec3<u32>,@builtin(local_invocation_index) group_index:u32) {
 let probe_index=traced_probe(group);
 if probe_index>=volume.probe_count {
  return;
 }
 if group_index==0u {
  irradiance_ray_count=min(ray_counts[probe_index],volume.max_rays);
 }
 let ray_count=workgroupUniformLoad(&irradiance_ray_count);
 if ray_count==0u {
  return;
 }
 let probe_coord=ddgi_probe_coord(probe_index,volume.probes);
 let probe=ddgi_unpack_probe(probe_states[probe_index]);
 let texel_direction=ddgi_decode_oct(((vec2<f32>(thread.xy%DDGI_COLOR_RESOLUTION)+.5)/f32(DDGI_COLOR_RESOLUTION))*2.-1.);
 var result=vec3(0.);
 var total_weight=0.;
 var remaining_rays=ray_count;
 var offset=0u;
 while remaining_rays>0u {
  let num_rays=min(IRRADIANCE_CACHE,remaining_rays);
  if group_index<num_rays {
   irradiance_cache[group_index]=ddgi_load_ray(probe_index,group_index+offset);
  }
  workgroupBarrier();
  for (var r=0u;r<num_rays;r++) {
   let ray=irradiance_cache[r];
   let weight=saturate(dot(texel_direction,ray.direction));
   if weight>DDGI_WEIGHT_EPSILON {
    result+=ray.radiance*weight;
    total_weight+=weight;
   }
  }
  workgroupBarrier();
  remaining_rays-=num_rays;
  offset+=num_rays;
 }
 if total_weight>DDGI_WEIGHT_EPSILON {
  result/=total_weight;
 }
 if thread.x<DDGI_COLOR_RESOLUTION && thread.y<DDGI_COLOR_RESOLUTION {
  let index=probe_index*DDGI_COLOR_RESOLUTION*DDGI_COLOR_RESOLUTION+thread.x+thread.y*DDGI_COLOR_RESOLUTION;
  var data=ddgi_load_variance(index);
  if !probe.blended {
   data=DdgiVariance(result,result,0.,vec3(0.),1.);
  }
  multiscale_mean_estimator(result,&data,DDGI_BLEND_SPEED);
  ddgi_store_variance(index,data);
  shared_texels[(1u+thread.x)+(1u+thread.y)*DDGI_COLOR_TEXELS]=data.mean;
 }
 workgroupBarrier();
 // Copy the colour borders.
 for (var index=group_index;index<28u;index+=IRRADIANCE_CACHE) {
  let border=DDGI_COLOR_BORDER_OFFSETS[index];
  shared_texels[border.z+border.w*DDGI_COLOR_TEXELS]=shared_texels[border.x+border.y*DDGI_COLOR_TEXELS];
 }
 workgroupBarrier();
 let tile=ddgi_probe_color_pixel(probe_coord,volume.probes)-vec2(1u);
 textureStore(probes_out,tile+thread.xy,vec4(shared_texels[group_index],1.));
}
const DEPTH_THREADS:u32=16u;
const DEPTH_CACHE:u32=DEPTH_THREADS*DEPTH_THREADS;
var<workgroup> depth_ray_count:u32;
var<workgroup> depth_cache:array<DdgiRay,DEPTH_CACHE>;
var<workgroup> shared_depths:array<vec2<f32>,DEPTH_CACHE>;
@compute @workgroup_size(DEPTH_THREADS,DEPTH_THREADS)
fn update_depth(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_id) thread:vec3<u32>,@builtin(local_invocation_index) group_index:u32) {
 let probe_index=traced_probe(group);
 if probe_index>=volume.probe_count {
  return;
 }
 if group_index==0u {
  depth_ray_count=min(ray_counts[probe_index],volume.max_rays);
 }
 let ray_count=workgroupUniformLoad(&depth_ray_count);
 if ray_count==0u {
  return;
 }
 let probe_coord=ddgi_probe_coord(probe_index,volume.probes);
 let probe=ddgi_unpack_probe(probe_states[probe_index]);
 let max_distance=volume.max_distance;
 let probe_limit=volume.spacing*.5;
 var probe_offset_new=vec3(0.);
 let probe_offset_distance=max_distance*DDGI_KEEP_DISTANCE;
 let texel_direction=ddgi_decode_oct(((vec2<f32>(thread.xy)+.5)/f32(DDGI_DEPTH_RESOLUTION))*2.-1.);
 var result=vec2(0.);
 var total_weight=0.;
 var remaining_rays=ray_count;
 var offset=0u;
 while remaining_rays>0u {
  let num_rays=min(DEPTH_CACHE,remaining_rays);
  if group_index<num_rays {
   depth_cache[group_index]=ddgi_load_ray(probe_index,group_index+offset);
  }
  workgroupBarrier();
  for (var r=0u;r<num_rays;r++) {
   let ray=depth_cache[r];
   var depth=max_distance;
   if ray.depth>0. {
    depth=clamp(ray.depth-.01,0.,max_distance);
   }
   if depth<probe_offset_distance {
    probe_offset_new-=ray.direction*(probe_offset_distance-depth);
   }
   let weight=pow(saturate(dot(texel_direction,ray.direction)),64.);
   if weight>DDGI_WEIGHT_EPSILON {
    result+=vec2(depth,depth*depth)*weight;
    total_weight+=weight;
   }
  }
  workgroupBarrier();
  remaining_rays-=num_rays;
  offset+=num_rays;
 }
 if total_weight>DDGI_WEIGHT_EPSILON {
  result/=total_weight;
 }
 let history=probe_index*DDGI_DEPTH_RESOLUTION*DDGI_DEPTH_RESOLUTION+thread.x+thread.y*DDGI_DEPTH_RESOLUTION;
 if probe.blended {
  result=mix(unpack2x16float(depth_history[history]),result,.02);
 }
 depth_history[history]=pack2x16float(result);
 shared_depths[group_index]=result;
 let pixel_topleft=ddgi_probe_depth_pixel(probe_coord,volume.probes);
 textureStore(probes_out,pixel_topleft+thread.xy,vec4(result,0.,1.));
 workgroupBarrier();
 // Copy the depth borders.
 let copy_coord=pixel_topleft-vec2(1u);
 for (var index=group_index;index<68u;index+=DEPTH_CACHE) {
  let border=DDGI_DEPTH_BORDER_OFFSETS[index];
  let source=(border.x-1u)+(border.y-1u)*DDGI_DEPTH_RESOLUTION;
  textureStore(probes_out,copy_coord+border.zw,vec4(shared_depths[source],0.,1.));
 }
 if group_index==0u {
  var probe_offset=probe.offset*probe_limit;
  if !probe.blended {
   probe_offset=vec3(0.);
  }
  probe_offset=mix(probe_offset,probe_offset_new,.01);
  probe_offset=clamp(probe_offset,-probe_limit,probe_limit);
  probe_offset/=probe_limit;
  probe_states[probe_index]=ddgi_pack_probe(DdgiProbe(probe_offset,true));
  textureStore(probes_out,ddgi_probe_data_pixel(probe_coord,volume.probes),vec4(probe_offset,1.));
 }
}
