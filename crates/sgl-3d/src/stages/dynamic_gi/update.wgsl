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
// on into the probe texture. Added: the scroll pass, which clears the planes
// of probes that enter a scrolled volume, as NVIDIA RTXGI's probe blending
// clears the planes its scroll offsets bring in (DDGIClearScrolledPlane;
// practice only, its code not copied): each starts again as a probe not yet
// blended, which the sample skips and the allocation starts through its
// ramp. Added: the depth blend classifies each probe, as NVIDIA RTXGI's
// probe classification does (RTXGI-DDGI
// f33e496ca31b3f0eec1c4e2cbaa8bb620e337fa6, rtxgi-sdk/shaders/ddgi/
// ProbeClassificationCS.hlsl 133-161, its first phase; practice only): a
// probe more than probeBackfaceThreshold (0.25) of whose fixed rays meet
// single-sided surfaces from behind, one inside geometry or beyond a wall,
// is inactive. Changed: RTXGI traces all 32 fixed rays every update; here a
// probe's first turn is classified from the share of its rays that met
// back faces, its second traces all 32, which classify it, and its next 7
// none, then it traces 4 each turn and is classified again from all 32
// once a cycle of 8 turns, so they cost an eighth; as RTXGI's, they are not
// blended. The share of each frame's rotated rays, blended as
// the depths are, flickered: in a room whose probes beyond the walls see a
// quarter of back faces, 66 changes of class in 200 frames among 125
// probes; and a far probe's first turn traces as few as 32 rotated rays,
// whose class it kept for its whole first cycle where now for one turn.
// Its second phase (172-214) finds whether a fixed ray met a front face
// within the probe's cell, the spacing about it along each axis
// (ddgi_in_cell); RTXGI deactivates a probe where none did. Improved: such
// a probe is dormant, not inactive: static
// receivers skip it, so a probe diagonally beyond the edge or corner of a
// room's single-sided walls, which sees few of their backs and so passes
// the first phase, lights no wall; moving receivers keep it, so a moving
// instance in open space is lit from its first frame where RTXGI's probes
// about it wait for its fixed rays to find it (a box appearing in open air
// took 7 frames); and it traces the fewest rays but for a moving instance
// near it (allocate.wgsl). Added: RTXGI's probe variability, the mean
// coefficient of variation of the active probes' irradiance texels
// (ProbeBlendingCS.hlsl 552-562, averaged as ReductionCS.hlsl averages it),
// which `settle` takes over windows of 16 turns of every active probe, as
// RTXGI's sample waits 16 frames of it, each updating every probe, before
// pausing a volume (samples/test-harness/src/graphics/DDGI_VK.cpp
// 1629-1637 and DDGI_D3D12.cpp 1239-1246): a window closes after 16 times
// the longest period among the active probes, so the slowest has taken 16
// turns, where a window of 16 frames closed before the slower probes'
// bounces had settled (a closed room's wall stopped at 0.298 of a bound
// between 0.300 and 0.381), and one of 16 turns of the average probe while
// far probes were still catching up. Each frame's average is weighed by the
// share of the probes that blended. Changed:
// RTXGI's sample pauses below a threshold each scene sets (0.03 to 0.4 in
// its configurations), where the variability settles; SGL3D has no scene to
// ask, so the volume has converged once a window's mean falls by less than
// a tenth from the last's, the plateau RTXGI describes the variability
// settling to (DDGIVolume.md 774-782). In a lit room it settled at 0.03 and
// in an open one at 0.004-0.014, so no one threshold would serve.
@group(0) @binding(0) var<uniform> volume:DdgiVolume;
@group(0) @binding(1) var<storage,read> ray_counts:array<u32>;
@group(0) @binding(2) var ray_results:texture_2d<u32>;
@group(0) @binding(3) var<storage,read_write> variance:array<u32>;
@group(0) @binding(4) var<storage,read_write> depth_history:array<u32>;
@group(0) @binding(5) var<storage,read_write> probe_states:array<vec4<u32>>;
@group(0) @binding(6) var probes_out:texture_storage_2d<rgba16float,write>;
@group(0) @binding(7) var<storage,read> traced_probes:array<u32>;
@group(0) @binding(8) var<storage,read_write> convergence:DdgiConvergence;
// Fixed-point units of variability in the sums, and the most a texel adds,
// which keeps the sum of a lattice's worth within a word.
const DDGI_VARIABILITY_UNIT:f32=1024.;
const DDGI_MOST_VARIABILITY:f32=4.;
var<workgroup> probe_variability:atomic<u32>;
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
 return ddgi_unpack_ray(textureLoad(ray_results,ddgi_ray_texel(ddgi_ray_slot(probe,ray,volume.max_rays)),0));
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
  irradiance_ray_count=min(ray_counts[probe_index],min(volume.max_rays,DDGI_MOST_RAYS));
  atomicStore(&probe_variability,0u);
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
  let previous_mean=data.mean;
  multiscale_mean_estimator(result,&data,DDGI_BLEND_SPEED);
  ddgi_store_variance(index,data);
  // RTXGI's coefficient of variation of the texel (ProbeBlendingCS.hlsl
  // 552-562): the sample's spread about the means before and after it, over
  // the mean.
  let spread=dot(ddgi_luminance_weights(),(result-previous_mean)*(result-data.mean));
  let luminance=dot(ddgi_luminance_weights(),data.mean);
  var variation=0.;
  if luminance>1./1024. {
   variation=sqrt(max(spread,0.))/luminance;
  }
  atomicAdd(&probe_variability,u32(min(variation,DDGI_MOST_VARIABILITY)*DDGI_VARIABILITY_UNIT));
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
 // An active probe's mean over its texels joins the volume's.
 if group_index==0u && ddgi_probe_active(probe) && probe.surfaced {
  atomicAdd(&convergence.variability,atomicLoad(&probe_variability)/(DDGI_COLOR_RESOLUTION*DDGI_COLOR_RESOLUTION));
  atomicAdd(&convergence.probes,1u);
 }
}
// Averages the frame's variability over the active probes that blended,
// none counting as none, and finds whether the volume has converged: once
// a window's mean falls by less than DDGI_CONVERGENCE_FALL from the last's,
// until what the probes' light follows changes. A window holds
// DDGI_CONVERGENCE_WINDOW turns of the active probe whose period, the
// stride included, is longest (`rank` and `threshold`).
@compute @workgroup_size(1)
fn settle() {
 if volume.changed!=0u {
  convergence.window_sum=0.;
  convergence.window_updates=0.;
  convergence.window_turns=0.;
  convergence.previous=-1.;
  convergence.converged=0u;
 }
 // A frame whose blends did not run adds nothing.
 if volume.traced==0u {
  return;
 }
 let probes=atomicLoad(&convergence.probes);
 var average=0.;
 if probes>0u {
  average=f32(atomicLoad(&convergence.variability))/(f32(probes)*DDGI_VARIABILITY_UNIT);
 }
 convergence.average=average;
 let share=f32(volume.traced)/f32(max(volume.probe_count,1u));
 convergence.window_sum+=average*share;
 convergence.window_updates+=share;
 convergence.window_turns+=1./f32(max(atomicLoad(&convergence.longest),1u));
 if convergence.window_turns>=DDGI_CONVERGENCE_WINDOW {
  let mean=convergence.window_sum/convergence.window_updates;
  let previous=convergence.previous;
  convergence.converged=select(0u,1u,previous>=0. && mean>=previous*(1.-DDGI_CONVERGENCE_FALL));
  convergence.previous=mean;
  convergence.window_sum=0.;
  convergence.window_updates=0.;
  convergence.window_turns=0.;
 }
}
// Whether `ray` met a front face within its probe's cell, the spacing
// about it along each axis: RTXGI's second phase.
fn ddgi_in_cell(ray:DdgiRay)->bool {
 let reach_axes=volume.spacing/max(abs(ray.direction),vec3(.000001));
 let reach=min(reach_axes.x,min(reach_axes.y,reach_axes.z));
 return !ray.backface && ray.depth>0. && ray.depth<=reach;
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
  depth_ray_count=min(ray_counts[probe_index],min(volume.max_rays,DDGI_MOST_RAYS));
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
 let depth_unit=ddgi_depth_unit(volume.spacing);
 var result=vec2(0.);
 var total_weight=0.;
 var backfaces=0u;
 var nearby=0u;
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
   backfaces+=select(0u,1u,ray.backface);
   nearby+=select(0u,1u,ddgi_in_cell(ray));
   var depth=max_distance;
   if ray.depth>0. {
    depth=clamp(ray.depth-.01,0.,max_distance);
   }
   if depth<probe_offset_distance {
    probe_offset_new-=ray.direction*(probe_offset_distance-depth);
   }
   let weight=pow(saturate(dot(texel_direction,ray.direction)),64.);
   if weight>DDGI_WEIGHT_EPSILON {
    let moment=depth/depth_unit;
    result+=vec2(moment,moment*moment)*weight;
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
  result=mix(unpack2x16float(depth_history[history]),result,DDGI_DEPTH_BLEND);
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
  var blended=probe;
  blended.offset=probe_offset;
  // Its class: on its first turn from the share of its rays that met back
  // faces, then from its fixed rays' share over each cycle of
  // DDGI_FIXED_CYCLE turns it traces, as RTXGI classifies from its fixed
  // rays: its second turn traced all of them, a whole cycle, its next
  // DDGI_FIXED_CYCLE - 1 none, and each turn since the next
  // DDGI_FIXED_RAYS_PER_FRAME, after the rays it blends
  // (ddgi_turn_fixed_rays).
  let fixed_rays=ddgi_turn_fixed_rays(probe);
  blended.fixed_rest=max(probe.fixed_rest,1u)-1u;
  if !probe.blended {
   blended=ddgi_fresh_probe();
   blended.offset=probe_offset;
   blended.backfaces=f32(backfaces)/f32(ray_count);
   blended.surfaced=nearby>0u;
   blended.fixed_rest=DDGI_FIXED_CYCLE;
  }
  blended.blended=true;
  for (var ray=0u;ray<DDGI_FIXED_RAYS;ray++) {
   if ray>=fixed_rays {
    break;
   }
   let fixed=ddgi_load_ray(probe_index,ray_count+ray);
   blended.fixed_backfaces+=select(0u,1u,fixed.backface);
   blended.fixed_nearby+=select(0u,1u,ddgi_in_cell(fixed));
  }
  blended.fixed_frames+=fixed_rays/DDGI_FIXED_RAYS_PER_FRAME;
  if blended.fixed_frames>=DDGI_FIXED_CYCLE {
   blended.backfaces=f32(blended.fixed_backfaces)/f32(DDGI_FIXED_RAYS);
   blended.surfaced=blended.fixed_nearby>0u;
   blended.fixed_backfaces=0u;
   blended.fixed_nearby=0u;
   blended.fixed_frames=0u;
  }
  probe_states[probe_index]=ddgi_pack_probe(blended);
  // 1 lights every receiver, 0.5 moving ones alone, 0 none.
  let lights=select(0.,select(.5,1.,blended.surfaced),ddgi_probe_active(blended));
  textureStore(probes_out,ddgi_probe_data_pixel(probe_coord,volume.probes),vec4(probe_offset,lights));
 }
}
// Whether the probe at lattice coordinate `coord` entered the volume with
// its move of `scrolled` whole spacings: on an axis it moved forward, the
// last planes, and on one it moved back, the first.
fn ddgi_entered(coord:vec3<u32>,scrolled:vec3<i32>)->bool {
 let at=vec3<i32>(coord);
 let count=vec3<i32>(volume.probes);
 return any(((scrolled>vec3(0))&(at>=count-scrolled))|((scrolled<vec3(0))&(at<-scrolled)));
}
// Clears the probes that entered with the frame's scroll: not blended, at
// rest, and so lighting nothing until they trace.
@compute @workgroup_size(64)
fn scroll(@builtin(global_invocation_id) id:vec3<u32>) {
 let probe_index=id.x;
 if probe_index>=volume.probe_count {
  return;
 }
 let stored=ddgi_probe_coord(probe_index,volume.probes);
 if !ddgi_entered(ddgi_probe_lattice(stored,volume.probes,volume.scroll),volume.scrolled) {
  return;
 }
 probe_states[probe_index]=ddgi_pack_probe(ddgi_fresh_probe());
 textureStore(probes_out,ddgi_probe_data_pixel(stored,volume.probes),vec4(0.));
}
