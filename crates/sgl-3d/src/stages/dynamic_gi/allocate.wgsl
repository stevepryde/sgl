// The dynamic GI stage's ray allocation: how many rays each probe traces
// this frame, the list of every ray, and the trace's indirect dispatch.
//
// Ports Wicked Engine df44c3db4c4927492bc9c791eac715d98d7ed091,
// WickedEngine/shaders/ddgi_rayallocationCS.hlsl (the probe's most
// inconsistent irradiance texel scales the most rays 24–47, a tenth outside
// the camera's frustum 49–55, buckets of four and at least four 57–58, the
// most for a probe not yet blended as for every probe on the first frame
// 60–61, the atomic allocation and the list 63–80) and
// ddgi_indirectprepareCS.hlsl, MIT (src/LICENSE-wicked.txt). Changed: a
// workgroup reduction in place of the wave maximum; the list is a texture
// of (probe, ray and the probe's ray count), the counts a buffer of rays
// rather than buckets, and the count stays apart from the dispatch; a
// two-dimensional dispatch where a row of workgroups would not hold them.
// Improved: Wicked starts every probe of a restarted volume in one frame,
// each at the most rays; here at most `volume.ramp_probes` start a frame,
// the nearest the camera first, and a probe not yet started traces nothing
// and weighs nothing in the sample, so its receivers keep their fallback
// (`rank` and `threshold` choose them). The blends run over the probes that
// trace, where Wicked's run over every probe, each of which always traces.
@group(0) @binding(0) var<uniform> volume:DdgiVolume;
@group(0) @binding(1) var<storage,read> variance:array<u32>;
@group(0) @binding(2) var<storage,read> probe_states:array<vec2<u32>>;
@group(0) @binding(3) var<storage,read_write> ray_counts:array<u32>;
@group(0) @binding(4) var<storage,read_write> allocation:DdgiAllocation;
@group(0) @binding(5) var ray_list:texture_storage_2d<rg32uint,write>;
// The probes that trace rays this frame, which the blends gather.
@group(0) @binding(6) var<storage,read_write> traced_probes:array<u32>;
// The probes not yet blended, by their distance from the camera in
// spacings, which the ramp starts nearest first.
const RAMP_BINS:u32=1024u;
// The trace's indirect dispatch and the rays the frame traces; the blends'
// and the probes that trace them; and the ramp: the nearer bins whose
// probes all start, how many of the probes in the bin after them start,
// how many of those have, the probes not yet blended, and each bin's of
// them.
struct DdgiAllocation {
 groups:array<u32,3>,
 rays:atomic<u32>,
 blend_groups:array<u32,3>,
 traced:atomic<u32>,
 ramp_bins:u32,
 ramp_room:u32,
 ramp_taken:atomic<u32>,
 // The probes not yet blended.
 unblended:atomic<u32>,
 bins:array<atomic<u32>,RAMP_BINS>,
}
const ALLOCATION_THREADS:u32=32u;
var<workgroup> shared_inconsistency:array<f32,ALLOCATION_THREADS>;
var<workgroup> shared_ray_count:u32;
var<workgroup> shared_ray_allocation:u32;
// Whether a sphere about `center` of `radius` reaches the camera's frustum.
fn camera_frustum_intersects(center:vec3<f32>,radius:f32)->bool {
 for (var plane=0u;plane<6u;plane++) {
  if dot(volume.frustum[plane].xyz,center)+volume.frustum[plane].w<-radius {
   return false;
  }
 }
 return true;
}
// The ramp's bin of a probe at `position`: its distance from the camera in
// the volume's least spacings.
fn ramp_bin(position:vec3<f32>)->u32 {
 let spacing=min(volume.spacing.x,min(volume.spacing.y,volume.spacing.z));
 return u32(min(distance(position,volume.eye)/spacing,f32(RAMP_BINS-1u)));
}
// Counts the probes not yet blended in each bin.
@compute @workgroup_size(64)
fn rank(@builtin(global_invocation_id) id:vec3<u32>,@builtin(num_workgroups) groups:vec3<u32>) {
 let probe_index=id.x+id.y*groups.x*64u;
 if probe_index>=volume.probe_count {
  return;
 }
 let probe=ddgi_unpack_probe(probe_states[probe_index]);
 if probe.blended {
  return;
 }
 let lattice=ddgi_probe_lattice(ddgi_probe_coord(probe_index,volume.probes),volume.probes,volume.scroll);
 let position=ddgi_probe_position_rest(lattice,volume.origin,volume.spacing);
 atomicAdd(&allocation.bins[ramp_bin(position)],1u);
 atomicAdd(&allocation.unblended,1u);
}
// The bins whose probes all start this frame, and how many of the next
// bin's do: `volume.ramp_probes` at most.
@compute @workgroup_size(1)
fn threshold() {
 var started=0u;
 var bin=0u;
 // Where every probe not yet blended fits, all start without the scan.
 if atomicLoad(&allocation.unblended)<=volume.ramp_probes {
  bin=RAMP_BINS;
 }
 loop {
  if bin>=RAMP_BINS {
   break;
  }
  let count=atomicLoad(&allocation.bins[bin]);
  if started+count>volume.ramp_probes {
   break;
  }
  started+=count;
  bin++;
 }
 allocation.ramp_bins=bin;
 allocation.ramp_room=volume.ramp_probes-started;
}
// Whether a probe not yet blended at `position` starts this frame.
fn ramp_starts(position:vec3<f32>)->bool {
 let bin=ramp_bin(position);
 if bin<allocation.ramp_bins {
  return true;
 }
 return bin==allocation.ramp_bins && atomicAdd(&allocation.ramp_taken,1u)<allocation.ramp_room;
}
@compute @workgroup_size(ALLOCATION_THREADS)
fn allocate(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_index) group_index:u32) {
 let probe_index=ddgi_group_probe(group);
 if probe_index>=volume.probe_count {
  return;
 }
 // Its place on the lattice, from where its scroll stores it.
 let probe_coord=ddgi_probe_lattice(ddgi_probe_coord(probe_index,volume.probes),volume.probes,volume.scroll);
 let probe=ddgi_unpack_probe(probe_states[probe_index]);
 let probe_pos=ddgi_probe_position(probe_coord,volume.origin,volume.spacing,probe.offset);
 var inconsistency=0.;
 let texels=DDGI_COLOR_RESOLUTION*DDGI_COLOR_RESOLUTION;
 for (var i=group_index;i<texels;i+=ALLOCATION_THREADS) {
  let at=(probe_index*texels+i)*DDGI_VARIANCE_WORDS+5u;
  inconsistency=max(inconsistency,unpack2x16float(variance[at]).x);
 }
 shared_inconsistency[group_index]=inconsistency;
 workgroupBarrier();
 if group_index==0u {
  var max_inconsistency=inconsistency;
  for (var i=0u;i<ALLOCATION_THREADS;i++) {
   max_inconsistency=max(max_inconsistency,shared_inconsistency[i]);
  }
  // At most DDGI_MOST_RAYS, so the loops over a probe's rays end whatever
  // the volume says.
  let most_rays=min(volume.max_rays,DDGI_MOST_RAYS);
  var ray_count=u32(saturate(max_inconsistency)*f32(most_rays));
  let spacing=volume.spacing;
  if !camera_frustum_intersects(probe_pos,max(spacing.x,max(spacing.y,spacing.z))*2.) {
   ray_count=u32(f32(ray_count)*.1);
  }
  ray_count=(ray_count+DDGI_RAY_BUCKET_COUNT-1u)/DDGI_RAY_BUCKET_COUNT*DDGI_RAY_BUCKET_COUNT;
  ray_count=clamp(ray_count,DDGI_RAY_BUCKET_COUNT,most_rays);
  if !probe.blended {
   ray_count=select(0u,most_rays,ramp_starts(ddgi_probe_position_rest(probe_coord,volume.origin,volume.spacing)));
  }
  ray_counts[probe_index]=ray_count;
  shared_ray_count=ray_count;
  shared_ray_allocation=atomicAdd(&allocation.rays,ray_count);
  if ray_count>0u {
   traced_probes[atomicAdd(&allocation.traced,1u)]=probe_index;
  }
 }
 let ray_count=workgroupUniformLoad(&shared_ray_count);
 let ray_allocation=workgroupUniformLoad(&shared_ray_allocation);
 for (var i=group_index;i<ray_count;i+=ALLOCATION_THREADS) {
  textureStore(ray_list,ddgi_ray_texel(ray_allocation+i),vec4(probe_index,i|(ray_count<<16u),0u,0u));
 }
}
// The trace's indirect dispatch, a workgroup for each DDGI_TRACE_THREADS
// rays, and the blends', one for each probe that traces, in rows of
// DDGI_GROUP_ROW.
@compute @workgroup_size(1)
fn prepare_trace() {
 let rays=atomicLoad(&allocation.rays);
 let groups=(rays+DDGI_TRACE_THREADS-1u)/DDGI_TRACE_THREADS;
 allocation.groups[0]=min(groups,DDGI_GROUP_ROW);
 allocation.groups[1]=(groups+DDGI_GROUP_ROW-1u)/DDGI_GROUP_ROW;
 allocation.groups[2]=1u;
 let traced=atomicLoad(&allocation.traced);
 allocation.blend_groups[0]=min(traced,DDGI_GROUP_ROW);
 allocation.blend_groups[1]=(traced+DDGI_GROUP_ROW-1u)/DDGI_GROUP_ROW;
 allocation.blend_groups[2]=1u;
}
