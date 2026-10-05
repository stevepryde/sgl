// The dynamic GI stage's ray allocation: how many rays each probe traces
// this frame, the list of every ray, and the trace's indirect dispatch.
//
// Ports Wicked Engine df44c3db4c4927492bc9c791eac715d98d7ed091,
// WickedEngine/shaders/ddgi_rayallocationCS.hlsl (the probe's most
// inconsistent irradiance texel scales the most rays 24–47, a tenth outside
// the camera's frustum 49–55, buckets of four and at least four 57–58, the
// most for a probe not yet blended as for every probe on the first frame
// 60–61, the atomic allocation and the list 63–80) and
// ddgi_indirectprepareCS.hlsl, MIT (src/LICENSE-wicked.txt). Changed: the
// inconsistency's maximum taken by one invocation a probe in place of a
// wave; the list is a texture of ray entries (DdgiRayEntry), the counts a
// buffer of rays rather than buckets, and the count stays apart from the
// dispatch; a two-dimensional dispatch where a row of workgroups would not
// hold them.
//
// Ports Wicked Engine 4323a33c94d021d45404adaf863e9b01673ab365's surfel GI
// ray budget, MIT (src/LICENSE-wicked.txt): its frame's most rays
// (ShaderInterop_SurfelGI.h SURFEL_RAY_BUDGET 52, surfel_updateCS.hlsl
// 263–268, surfel_indirectprepareCS.hlsl 18–22), past which a request
// traces nothing, and its temporal amortization (surfel_updateCS.hlsl
// 214–255, SURFEL_RAY_UPDATE_PERIOD_MAX 54 and _CAP 56, surfel_hash01
// 272–278): each probe traces on its turn, once in a period that grows
// with its distance level from 1 to DDGI_PERIOD_LEVELS levels out, then
// doubles a level beyond, up to DDGI_PERIOD_CAP, at a phase its hash
// staggers, keeping its light between turns. Changed: the level is the
// log2 of the probe's distance from the camera in the volume's least
// spacings, where a surfel's is of its radius in the finest cell's; and
// where Wicked's requests past its budget trace nothing in dispatch order,
// here every period is lengthened by the least power of two
// (DDGI_STRIDES) under which the blended probes' requests fit the budget
// beside the probes that start, so every probe keeps its turns and none
// starves; the request past the budget that remains traces nothing, as
// Wicked's. The budget replaces the restart's own (#152): probes not yet
// blended start, the nearest the camera first (`rank` and `threshold`
// choose them), at the most rays with what the blended probes leave, which
// is at least half the budget, and all of it after a restart; a probe not
// yet started traces nothing and weighs nothing in the sample, so its
// receivers keep their fallback. The blends run over the probes that
// trace, where Wicked's run over every probe, each of which always traces.
// Added: each probe that traces also traces DDGI_FIXED_RAYS_PER_FRAME
// fixed rays after its others, the next of its cycle, which classify it,
// as NVIDIA RTXGI's probes trace their fixed rays beside their others
// (RTXGI-DDGI f33e496ca31b3f0eec1c4e2cbaa8bb620e337fa6, docs/DDGIVolume.md
// 736-767; practice only). Changed: an inactive probe (ddgi_probe_active)
// traces the fewest others, a bucket, beside its fixed rays, and still
// blends them, where RTXGI's inactive probes trace their fixed rays alone
// and blend nothing: its depth and irradiance stay warm, so it lights
// rightly the turn it becomes active again. A dormant probe, with no
// surface in its cell, traces the fewest too, staying warm for the moving
// receivers it lights, unless a moving instance's bounds reach into its
// cell: then it traces as an active probe does, so the light about the
// instance keeps up with it. A volume that has converged while what its
// light follows held still traces nothing (ddgi_paused; update.wgsl's
// settle), as RTXGI's sample pauses a volume whose variability has settled
// (samples/test-harness/src/graphics/DDGI_VK.cpp 1629-1637 and
// DDGI_D3D12.cpp 1239-1246).
@group(0) @binding(0) var<uniform> volume:DdgiVolume;
@group(0) @binding(1) var<storage,read> variance:array<u32>;
@group(0) @binding(2) var<storage,read> probe_states:array<vec4<u32>>;
// Each blended probe's request and period (`rank`), then the rays it
// traces this frame (`allocate`).
@group(0) @binding(3) var<storage,read_write> ray_counts:array<u32>;
@group(0) @binding(4) var<storage,read_write> allocation:DdgiAllocation;
@group(0) @binding(5) var ray_list:texture_storage_2d<rg32uint,write>;
// The probes that trace rays this frame, which the blends gather.
@group(0) @binding(6) var<storage,read_write> traced_probes:array<u32>;
@group(0) @binding(7) var<storage,read> moving_bounds:array<DdgiBounds>;
@group(0) @binding(8) var<storage,read_write> convergence:DdgiConvergence;
// Whether the volume pauses this frame: it has converged, nothing its light
// follows has changed, and every probe has started.
fn ddgi_paused()->bool {
 return volume.changed==0u && convergence.converged!=0u && atomicLoad(&allocation.unblended)==0u;
}
// Whether a moving instance's bounds reach into the cell about a probe at
// `position`.
fn ddgi_near_moving(position:vec3<f32>)->bool {
 for (var i=0u;i<min(volume.moving_count,DDGI_MOST_MOVING_BOUNDS);i++) {
  let bounds=moving_bounds[i];
  if all(bounds.min<=position+volume.spacing) && all(bounds.max>=position-volume.spacing) {
   return true;
  }
 }
 return false;
}
// The probes not yet blended, by their distance from the camera in
// spacings, which start nearest first.
const RAMP_BINS:u32=1024u;
// The lengthenings of every period the allocation weighs: by 1, 2, 4 and
// so on to 64.
const DDGI_STRIDES:u32=7u;
// The distance levels over which a probe's period grows from 1 to
// DDGI_PERIOD_MAX, as Wicked's surfel period grows over its cascade's
// levels (SURFEL_GRID_LEVELS 8), and the longest period beyond them.
const DDGI_PERIOD_LEVELS:f32=7.;
const DDGI_PERIOD_MAX:f32=8.;
const DDGI_PERIOD_CAP:f32=32.;
// The trace's indirect dispatch and the rays the frame traces; the blends'
// and the probes that trace them; the starting probes: the nearer bins
// whose probes all start, how many of the probes in the bin after them
// start, how many of those have, the probes not yet blended, and each
// bin's of them; whether the volume pauses, the stride the frame's periods
// take (as a power of two), the rays the frame reserves against the
// budget, and the blended probes' requests on their turns under each
// stride.
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
 paused:u32,
 stride:u32,
 reserved:atomic<u32>,
 demand:array<atomic<u32>,DDGI_STRIDES>,
 bins:array<atomic<u32>,RAMP_BINS>,
}
const ALLOCATION_THREADS:u32=32u;
var<workgroup> shared_ray_count:u32;
var<workgroup> shared_ray_allocation:u32;
var<workgroup> shared_cycle:u32;
// Whether a sphere about `center` of `radius` reaches the camera's frustum.
fn camera_frustum_intersects(center:vec3<f32>,radius:f32)->bool {
 for (var plane=0u;plane<6u;plane++) {
  if dot(volume.frustum[plane].xyz,center)+volume.frustum[plane].w<-radius {
   return false;
  }
 }
 return true;
}
// A probe at rest at `position`'s distance from the camera in the volume's
// least spacings.
fn ddgi_spacings_away(position:vec3<f32>)->f32 {
 let spacing=min(volume.spacing.x,min(volume.spacing.y,volume.spacing.z));
 return distance(position,volume.eye)/spacing;
}
// The bin of a probe that far away.
fn ramp_bin(spacings:f32)->u32 {
 return u32(min(spacings,f32(RAMP_BINS-1u)));
}
// Wicked's surfel_hash01, a stable hash of a probe's stored index: its
// turns' phase.
fn ddgi_phase_hash(index:u32)->u32 {
 var x=index;
 x^=x>>16u;
 x*=0x7feb352du;
 x^=x>>15u;
 x*=0x846ca68bu;
 x^=x>>16u;
 return x&0xffffffu;
}
// The period of a probe that far away: 1 within a spacing, growing over
// DDGI_PERIOD_LEVELS doublings of its distance to DDGI_PERIOD_MAX, then
// doubling with each beyond, up to DDGI_PERIOD_CAP (Wicked's ray_period).
fn ddgi_period(spacings:f32)->u32 {
 let level=log2(max(spacings,1.));
 var period=mix(1.,DDGI_PERIOD_MAX,saturate(level/DDGI_PERIOD_LEVELS));
 period*=exp2(max(0.,level-DDGI_PERIOD_LEVELS));
 return u32(clamp(round(period),1.,DDGI_PERIOD_CAP));
}
// Whether probe `index`, of period `period`, traces this frame.
fn ddgi_turn(index:u32,period:u32)->bool {
 let phase=u32(f32(ddgi_phase_hash(index))/16777216.*f32(period));
 return (volume.frame+phase)%period==0u;
}
// The rays a probe traces beside its fixed rays at most this frame.
fn ddgi_most_rays()->u32 {
 return min(volume.max_rays,DDGI_MOST_RAYS);
}
// A blended probe's request (Wicked's allocation): the most rays scaled by
// its most inconsistent texel, a tenth outside the camera's frustum, in
// buckets, at least one; the fewest where it is inactive, or dormant with
// no moving instance about it.
fn ddgi_request(probe_index:u32,probe:DdgiProbe,position:vec3<f32>)->u32 {
 let texels=DDGI_COLOR_RESOLUTION*DDGI_COLOR_RESOLUTION;
 var inconsistency=0.;
 for (var i=0u;i<texels;i++) {
  let at=(probe_index*texels+i)*DDGI_VARIANCE_WORDS+5u;
  inconsistency=max(inconsistency,unpack2x16float(variance[at]).x);
 }
 let most_rays=ddgi_most_rays();
 var ray_count=u32(saturate(inconsistency)*f32(most_rays));
 let spacing=volume.spacing;
 if !camera_frustum_intersects(position,max(spacing.x,max(spacing.y,spacing.z))*2.) {
  ray_count=u32(f32(ray_count)*.1);
 }
 ray_count=(ray_count+DDGI_RAY_BUCKET_COUNT-1u)/DDGI_RAY_BUCKET_COUNT*DDGI_RAY_BUCKET_COUNT;
 ray_count=clamp(ray_count,DDGI_RAY_BUCKET_COUNT,most_rays);
 // An inactive probe traces the fewest, which follow what it sees, as
 // RTXGI's inactive probes trace their fixed rays alone.
 if !ddgi_probe_active(probe) {
  ray_count=DDGI_RAY_BUCKET_COUNT;
 }
 // One with nothing in its cell stays warm at the fewest, unless a moving
 // instance reaches into its cell.
 if !probe.surfaced && !ddgi_near_moving(position) {
  ray_count=DDGI_RAY_BUCKET_COUNT;
 }
 return ray_count;
}
// Counts the probes not yet blended in each bin, and finds each blended
// probe's request and period and what its turns ask under each stride.
@compute @workgroup_size(64)
fn rank(@builtin(global_invocation_id) id:vec3<u32>,@builtin(num_workgroups) groups:vec3<u32>) {
 let probe_index=id.x+id.y*groups.x*64u;
 if probe_index>=volume.probe_count {
  return;
 }
 let probe=ddgi_unpack_probe(probe_states[probe_index]);
 let lattice=ddgi_probe_lattice(ddgi_probe_coord(probe_index,volume.probes),volume.probes,volume.scroll);
 let spacings=ddgi_spacings_away(ddgi_probe_position_rest(lattice,volume.origin,volume.spacing));
 if !probe.blended {
  atomicAdd(&allocation.bins[ramp_bin(spacings)],1u);
  atomicAdd(&allocation.unblended,1u);
  return;
 }
 let request=ddgi_request(probe_index,probe,ddgi_probe_position(lattice,volume.origin,volume.spacing,probe.offset));
 let period=ddgi_period(spacings);
 ray_counts[probe_index]=request|(period<<16u);
 for (var stride=0u;stride<DDGI_STRIDES;stride++) {
  if ddgi_turn(probe_index,period<<stride) {
   atomicAdd(&allocation.demand[stride],request+DDGI_FIXED_RAYS_PER_FRAME);
  }
 }
}
// The stride the frame's periods take: the least under which the blended
// probes' requests fit the budget beside at least half of it, or all of it
// where fewer start, for the probes that start; and those that start: the
// bins whose probes all start, and how many of the next bin's do.
@compute @workgroup_size(1)
fn threshold() {
 allocation.paused=select(0u,1u,ddgi_paused());
 let starting_rays=ddgi_most_rays()+DDGI_FIXED_RAYS_PER_FRAME;
 let unblended=atomicLoad(&allocation.unblended);
 let budget=volume.budget;
 let room=budget-min(unblended*starting_rays,budget/2u);
 var stride=0u;
 loop {
  if stride+1u>=DDGI_STRIDES || atomicLoad(&allocation.demand[stride])<=room {
   break;
  }
  stride++;
 }
 allocation.stride=stride;
 let starts=(budget-min(atomicLoad(&allocation.demand[stride]),room))/starting_rays;
 var started=0u;
 var bin=0u;
 // Where every probe not yet blended fits, all start without the scan.
 if unblended<=starts {
  bin=RAMP_BINS;
 }
 loop {
  if bin>=RAMP_BINS {
   break;
  }
  let count=atomicLoad(&allocation.bins[bin]);
  if started+count>starts {
   break;
  }
  started+=count;
  bin++;
 }
 allocation.ramp_bins=bin;
 allocation.ramp_room=starts-started;
}
// Whether a probe not yet blended that far away starts this frame.
fn ramp_starts(spacings:f32)->bool {
 let bin=ramp_bin(spacings);
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
 if group_index==0u {
  let probe=ddgi_unpack_probe(probe_states[probe_index]);
  var ray_count=0u;
  if !probe.blended {
   let lattice=ddgi_probe_lattice(ddgi_probe_coord(probe_index,volume.probes),volume.probes,volume.scroll);
   if ramp_starts(ddgi_spacings_away(ddgi_probe_position_rest(lattice,volume.origin,volume.spacing))) {
    ray_count=ddgi_most_rays();
   }
  } else {
   let requested=ray_counts[probe_index];
   if ddgi_turn(probe_index,(requested>>16u)<<allocation.stride) {
    ray_count=requested&0xffffu;
   }
  }
  // A paused volume traces nothing; its probes keep what they hold.
  if allocation.paused!=0u {
   ray_count=0u;
  }
  // A probe that traces traces its fixed rays after its others, within
  // the budget, past which it traces nothing, as Wicked's surfels do.
  var traced=select(0u,ray_count+DDGI_FIXED_RAYS_PER_FRAME,ray_count>0u);
  if traced>0u && atomicAdd(&allocation.reserved,traced)+traced>volume.budget {
   ray_count=0u;
   traced=0u;
  }
  ray_counts[probe_index]=ray_count;
  shared_ray_count=ray_count;
  shared_cycle=probe.fixed_frames;
  shared_ray_allocation=atomicAdd(&allocation.rays,traced);
  if ray_count>0u {
   traced_probes[atomicAdd(&allocation.traced,1u)]=probe_index;
  }
 }
 let ray_count=workgroupUniformLoad(&shared_ray_count);
 let ray_allocation=workgroupUniformLoad(&shared_ray_allocation);
 let cycle=workgroupUniformLoad(&shared_cycle);
 let traced=select(0u,ray_count+DDGI_FIXED_RAYS_PER_FRAME,ray_count>0u);
 for (var i=group_index;i<traced;i+=ALLOCATION_THREADS) {
  textureStore(ray_list,ddgi_ray_texel(ray_allocation+i),vec4(ddgi_pack_ray_entry(DdgiRayEntry(probe_index,i,ray_count,cycle)),0u,0u));
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
