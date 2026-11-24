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
// 214–255, SURFEL_RAY_UPDATE_PERIOD_MAX 55 and _CAP 56, surfel_hash01
// 272–278): each probe traces on its turn, once in a period that grows
// with its distance level from 1 to DDGI_PERIOD_LEVELS levels out, then
// doubles a level beyond, up to DDGI_PERIOD_CAP, at a phase its hash
// staggers, keeping its light between turns. Its distance level also
// scales the most rays a probe traces, from the tier's most within a
// spacing to an eighth of it DDGI_PERIOD_LEVELS levels out, as Wicked
// scales each surfel's rays by its level (surfel_updateCS.hlsl 196–214,
// SURFEL_RAY_BOOST_MAX 64 to SURFEL_RAY_BOOST_MIN 8, 53–54), for a probe
// that starts and one whose light changes alike. Changed:
// - The level is the log2 of the probe's distance from the camera in the
//   volume's least spacings, where a surfel's is of its radius in the
//   finest cell's.
// - Where Wicked's requests past its budget trace nothing in dispatch
//   order, here every period is lengthened by the least power of two
//   (the stride, up to 2^(DDGI_STRIDES-1)) under which the blended probes'
//   requests on their turns fit the budget beside the probes that start,
//   so every probe keeps its turns and none starves; the request past the
//   budget that remains traces nothing, as Wicked's, but in the claim's
//   order (`claim`), not dispatch order. A probe's phase is
//   Wicked's within its period plus whole periods its hash's high bits
//   choose, so its turns under a stride are among its turns under every
//   shorter one and a stride that changes from frame to frame never skips
//   them (Wicked has one period a surfel, so its phases need not nest).
// - The budget replaces the restart's own (#152): probes not yet blended
//   start, the nearest the camera first (`rank` and `threshold` choose
//   them), at the most rays with what the blended probes leave, which is at
//   least half the budget, and all of it after a restart; a probe not yet
//   started traces nothing and weighs nothing in the sample, so its
//   receivers keep their fallback. The blends run over the probes that
//   trace, where Wicked's run over every probe, each of which always
//   traces.
// Added, SGL3D's own (neither Wicked, whose inconsistency sets a surfel's
// rays a turn, nor RTXGI, which leaves scheduling to the application,
// shortens a period): a probe whose light is changing, its most
// inconsistent texel above the estimator's noise (DDGI_INCONSISTENCY_NOISE),
// also takes turns at its period shortened toward 1 as its inconsistency
// rises, from the rays the distance turns and the starting probes leave, so
// a light moved is answered sooner without lengthening any probe's distance
// turns. Shortened turns counted toward the stride doubled every probe's
// period on Hyperdrive's course from noise alone (#196); and below the
// estimator's noise a probe takes none, where taking them at any
// inconsistency cost 0.3 ms more a frame there with the camera still, for
// no measurably quicker answer to a moved light in the dynamic_gi example.
// Added: each probe that traces also traces DDGI_FIXED_RAYS_PER_FRAME
// fixed rays after its others, the next of its cycle, which classify it, as
// NVIDIA RTXGI's probes trace their fixed rays among their others each
// update (RTXGI-DDGI f33e496ca31b3f0eec1c4e2cbaa8bb620e337fa6,
// docs/DDGIVolume.md 736-767; practice only), and as RTXGI's they are not
// blended. Changed: a probe's first turn traces none, its other rays
// classifying it; its second all DDGI_FIXED_RAYS, which classify it, and
// its next DDGI_FIXED_CYCLE - 1 none, so its first cycles cost what any
// other's do (ddgi_turn_fixed_rays). Its first turn tracing them all, as
// RTXGI's every update does, tripled the probes waiting to start in motion
// at High and multiplied them about 6.7 times at Low on Hyperdrive's course
// (#196), as the budget leaves starting probes their
// share. Changed: an inactive probe (ddgi_probe_active) traces the fewest
// others, a bucket, beside its fixed rays, and still blends them, where
// RTXGI's inactive probes trace their fixed rays alone and blend nothing:
// its depth and irradiance stay warm, so it lights rightly the turn it
// becomes active again. A dormant probe, with no
// surface in its cell, traces the fewest too, staying warm for the moving
// receivers it lights, unless a moving instance's bounds reach into its
// cell: then it traces as an active probe does, so the light about the
// instance keeps up with it. A volume that has converged while what its
// light follows held still traces nothing (ddgi_paused; update.wgsl's
// settle), as RTXGI's sample pauses a volume whose variability has settled
// (samples/test-harness/src/graphics/DDGI_VK.cpp 1629-1637 and
// DDGI_D3D12.cpp 1239-1246).
// Changed: Wicked reserves its rays by atomics in the order the GPU runs its
// probes (ddgi_rayallocationCS.hlsl 63-80, surfel_updateCS.hlsl 263-268),
// which differs from run to run. Here each limit is shared by a claim in a
// fixed order (`claim`), an exclusive prefix sum over the probes that
// compete for it, the deterministic form of stream compaction (Harris,
// Sengupta and Owens, GPU Gems 3 chapter 39), so the probes that trace,
// and how many rays each, are a function of the frame alone.
@group(0) @binding(0) var<uniform> volume:DdgiVolume;
@group(0) @binding(1) var<storage,read> variance:array<u32>;
@group(0) @binding(2) var<storage,read> probe_states:array<vec4<u32>>;
// Each blended probe's request and periods, and each probe not yet
// blended's starting rays and bin (`rank`), which the claim reads so that
// every pass agrees on them; then the rays it traces this frame beside its
// fixed rays (`claim`), which it blends.
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
// The rays the probes not yet blended start with, by their distance from
// the camera in spacings, which start nearest first.
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
// whose probes all start and how many rays of the probes in the bin after
// them may start, the probes not yet blended and the rays they start with;
// whether the volume pauses, the stride the frame's periods take (as a
// power of two), the rays the turns may take, the rays the shortened turns
// may take, the blended probes' requests on their turns under each stride,
// each bin's rays, and each block of the claim order's rays in each pool.
struct DdgiAllocation {
 groups:array<u32,3>,
 rays:atomic<u32>,
 blend_groups:array<u32,3>,
 traced:atomic<u32>,
 ramp_bins:u32,
 ramp_room:u32,
 // The probes not yet blended, and the rays they start with.
 unblended:atomic<u32>,
 unblended_rays:atomic<u32>,
 paused:u32,
 stride:u32,
 turn_room:u32,
 spare:u32,
 demand:array<atomic<u32>,DDGI_STRIDES>,
 bins:array<atomic<u32>,RAMP_BINS>,
 claims:array<vec4<u32>>,
}
const ALLOCATION_THREADS:u32=32u;
var<workgroup> shared_entry_rays:u32;
var<workgroup> shared_traced:u32;
var<workgroup> shared_ray_allocation:u32;
var<workgroup> shared_cycle:u32;
// Whether a sphere about `center` of `radius` reaches the camera's frustum.
fn camera_frustum_intersects(center:vec3<f32>,radius:f32)->bool {
 for (var plane=0u;plane<DDGI_FRUSTUM_PLANES;plane++) {
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
// A probe that far away's distance level: Wicked's continuous level, the
// log2 of its distance in the least spacings.
fn ddgi_level(spacings:f32)->f32 {
 return log2(max(spacings,1.));
}
// The share of the most rays the farthest probes trace: Wicked's
// SURFEL_RAY_BOOST_MIN over SURFEL_RAY_BOOST_MAX.
const DDGI_FARTHEST_RAYS:f32=.125;
// Wicked's surfel_hash01's hash of a probe's stored index: its low 24 bits
// are its phase within its period, its high bits the stride's (ddgi_turn).
fn ddgi_phase_hash(index:u32)->u32 {
 var x=index;
 x^=x>>16u;
 x*=0x7feb352du;
 x^=x>>15u;
 x*=0x846ca68bu;
 x^=x>>16u;
 return x;
}
// The period of a probe that far away: 1 within a spacing, growing over
// DDGI_PERIOD_LEVELS doublings of its distance to DDGI_PERIOD_MAX, then
// doubling with each beyond, up to DDGI_PERIOD_CAP (Wicked's ray_period).
fn ddgi_period(spacings:f32)->u32 {
 let level=ddgi_level(spacings);
 var period=mix(1.,DDGI_PERIOD_MAX,saturate(level/DDGI_PERIOD_LEVELS));
 period*=exp2(max(0.,level-DDGI_PERIOD_LEVELS));
 return u32(clamp(round(period),1.,DDGI_PERIOD_CAP));
}
// Whether probe `index`, of period `period` lengthened by 2^`stride`,
// traces in frame `frame`: at Wicked's phase within its period, plus the
// whole periods the stride's bits of its hash choose, so its turns under a
// stride are among its turns under every shorter one.
fn ddgi_turn(index:u32,period:u32,stride:u32,frame:u32)->bool {
 let hash=ddgi_phase_hash(index);
 let within=u32(f32(hash&0xffffffu)/16777216.*f32(period));
 let periods=(hash>>24u)&((1u<<stride)-1u);
 return (frame+within+periods*period)%(period<<stride)==0u;
}
// The rays a probe that far away traces beside its fixed rays at most this
// frame: the tier's most within a spacing, falling over DDGI_PERIOD_LEVELS
// levels to DDGI_FARTHEST_RAYS of it (Wicked's ray_boost), in buckets.
fn ddgi_most_rays(spacings:f32)->u32 {
 let most=f32(min(volume.max_rays,DDGI_MOST_RAYS));
 let rays=most*mix(1.,DDGI_FARTHEST_RAYS,saturate(ddgi_level(spacings)/DDGI_PERIOD_LEVELS));
 let buckets=u32(round(rays/f32(DDGI_RAY_BUCKET_COUNT)));
 return clamp(buckets*DDGI_RAY_BUCKET_COUNT,DDGI_RAY_BUCKET_COUNT,u32(most));
}
// The rays a probe traces on its turn beside `fixed_rays` fixed rays, of
// `rays` it asks for: within the slots each probe's rays take
// (ddgi_ray_slot), which its turn tracing all its fixed rays may cut.
fn ddgi_turn_rays(rays:u32,fixed_rays:u32)->u32 {
 let slots=min(volume.max_rays,DDGI_MOST_RAYS)+DDGI_FIXED_RAYS_PER_FRAME;
 return min(rays,max(slots,fixed_rays+DDGI_RAY_BUCKET_COUNT)-fixed_rays);
}
// A blended probe's most inconsistent irradiance texel's inconsistency.
fn ddgi_inconsistency(probe_index:u32)->f32 {
 let texels=DDGI_COLOR_RESOLUTION*DDGI_COLOR_RESOLUTION;
 var inconsistency=0.;
 for (var i=0u;i<texels;i++) {
  let at=(probe_index*texels+i)*DDGI_VARIANCE_WORDS+5u;
  inconsistency=max(inconsistency,unpack2x16float(variance[at]).x);
 }
 return inconsistency;
}
// A blended probe's request (Wicked's allocation): its most rays scaled by
// its most inconsistent texel, a tenth outside the camera's frustum, in
// buckets, at least one; the fewest where it is inactive, or dormant with
// no moving instance about it.
fn ddgi_request(inconsistency:f32,probe:DdgiProbe,position:vec3<f32>,spacings:f32)->u32 {
 let most_rays=ddgi_most_rays(spacings);
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
// The period of a blended probe's shortened turns: its distance's `period`
// shortened toward 1 as its inconsistency rises from the estimator's noise
// to 1; its distance's where it requests the fewest rays.
fn ddgi_shortened_period(period:u32,inconsistency:f32,request:u32)->u32 {
 if request<=DDGI_RAY_BUCKET_COUNT {
  return period;
 }
 let change=saturate((inconsistency-DDGI_INCONSISTENCY_NOISE)/(1.-DDGI_INCONSISTENCY_NOISE));
 return u32(round(1.+f32(period-1u)*(1.-change)));
}
// Counts the rays the probes not yet blended start with in each bin, and
// finds each blended probe's request and periods, what its turns ask under
// each stride, and the longest period among the active probes.
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
  let starting=ddgi_most_rays(spacings);
  let bin=ramp_bin(spacings);
  ray_counts[probe_index]=starting|(bin<<16u);
  atomicAdd(&allocation.bins[bin],starting);
  atomicAdd(&allocation.unblended,1u);
  atomicAdd(&allocation.unblended_rays,starting);
  return;
 }
 let inconsistency=ddgi_inconsistency(probe_index);
 let request=ddgi_request(inconsistency,probe,ddgi_probe_position(lattice,volume.origin,volume.spacing,probe.offset),spacings);
 let period=ddgi_period(spacings);
 let shortened=ddgi_shortened_period(period,inconsistency,request);
 ray_counts[probe_index]=request|(period<<16u)|(shortened<<24u);
 let fixed_rays=ddgi_turn_fixed_rays(probe);
 let traced=ddgi_turn_rays(request,fixed_rays)+fixed_rays;
 for (var stride=0u;stride<DDGI_STRIDES;stride++) {
  if ddgi_turn(probe_index,period,stride,volume.frame) {
   atomicAdd(&allocation.demand[stride],traced);
  }
 }
 // The probes whose variability the convergence follows (update.wgsl).
 if ddgi_probe_active(probe) && probe.surfaced {
  atomicMax(&convergence.longest,period);
 }
}
// The stride the frame's periods take: the least under which the blended
// probes' requests fit the budget beside at least half of it, or all of it
// where fewer ask, for the probes that start; those that start: the bins
// whose probes all start, and how many rays of the next bin's start; and
// what the shortened turns may take.
@compute @workgroup_size(1)
fn threshold() {
 allocation.paused=select(0u,1u,ddgi_paused());
 let budget=volume.budget;
 let unblended_rays=atomicLoad(&allocation.unblended_rays);
 let room=budget-min(unblended_rays,budget/2u);
 var stride=0u;
 loop {
  if stride+1u>=DDGI_STRIDES || atomicLoad(&allocation.demand[stride])<=room {
   break;
  }
  stride++;
 }
 allocation.stride=stride;
 atomicStore(&convergence.longest,atomicLoad(&convergence.longest)<<stride);
 // The turns take their requests, or the room where those exceed it at
 // the longest stride; the starting probes and the shortened turns what
 // that leaves, so the frame's rays never pass the budget.
 allocation.turn_room=min(atomicLoad(&allocation.demand[stride]),room);
 let starts=budget-allocation.turn_room;
 // What the blended probes' turns and the starting probes leave, which the
 // shortened turns take: nothing while probes wait to start.
 allocation.spare=select(0u,starts-unblended_rays,unblended_rays<=starts);
 var started=0u;
 var bin=0u;
 // Where every probe not yet blended fits, all start without the scan.
 if unblended_rays<=starts {
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
// The pools in which probes compete for the frame's rays (`claim`): a
// probe not yet blended in the boundary bin for the starting probes' room,
// a blended probe on its turn for the turns' room, and one on a shortened
// turn for the spare. A probe in none traces its rays without competing,
// or traces none. Each pool is a lane of the claim's sums.
const DDGI_POOL_NONE:u32=0u;
const DDGI_POOL_RAMP:u32=1u;
const DDGI_POOL_TURN:u32=2u;
const DDGI_POOL_SPARE:u32=3u;
// A probe's claim on the frame's rays: its pool, the rays it traces if it
// is granted them, its fixed rays included, and those it blends.
struct DdgiAsk {
 pool:u32,
 traced:u32,
 blended:u32,
}
// Probe `probe_index`'s claim this frame.
fn ddgi_ask(probe_index:u32)->DdgiAsk {
 // A paused volume traces nothing; its probes keep what they hold.
 if allocation.paused!=0u {
  return DdgiAsk(DDGI_POOL_NONE,0u,0u);
 }
 let probe=ddgi_unpack_probe(probe_states[probe_index]);
 if !probe.blended {
  // Those in the nearer bins all start; those in the boundary bin within
  // its room. Its first turn traces no fixed rays. Its starting rays and
  // bin are `rank`'s, in integers, so the claim counts what the bins do.
  let starting=ray_counts[probe_index]&0xffffu;
  let bin=ray_counts[probe_index]>>16u;
  if bin<allocation.ramp_bins {
   return DdgiAsk(DDGI_POOL_NONE,starting,starting);
  }
  if bin==allocation.ramp_bins {
   return DdgiAsk(DDGI_POOL_RAMP,starting,starting);
  }
  return DdgiAsk(DDGI_POOL_NONE,0u,0u);
 }
 let requested=ray_counts[probe_index];
 let rays=min(requested&0xffffu,min(volume.max_rays,DDGI_MOST_RAYS));
 let period=(requested>>16u)&0xffu;
 let shortened=requested>>24u;
 let fixed_rays=ddgi_turn_fixed_rays(probe);
 let turn_rays=ddgi_turn_rays(rays,fixed_rays);
 // Its fixed rays after its others: on its turn, or on its turn at
 // `shortened` where that is shorter than its distance's `period`.
 if ddgi_turn(probe_index,period,allocation.stride,volume.frame) {
  return DdgiAsk(DDGI_POOL_TURN,turn_rays+fixed_rays,turn_rays);
 }
 if shortened<period && ddgi_turn(probe_index,shortened,0u,volume.frame) {
  return DdgiAsk(DDGI_POOL_SPARE,turn_rays+fixed_rays,turn_rays);
 }
 return DdgiAsk(DDGI_POOL_NONE,0u,0u);
}
// The rays a pool's probes may take this frame.
fn ddgi_pool_room(pool:u32)->u32 {
 switch pool {
  case DDGI_POOL_RAMP: {
   return allocation.ramp_room;
  }
  case DDGI_POOL_TURN: {
   return allocation.turn_room;
  }
  default: {
   return allocation.spare;
  }
 }
}
// A claim's rays in its pool's lane.
fn ddgi_pool_rays(ask:DdgiAsk)->vec4<u32> {
 let lanes=vec4(DDGI_POOL_NONE,DDGI_POOL_RAMP,DDGI_POOL_TURN,DDGI_POOL_SPARE);
 var rays=select(vec4(0u),vec4(ask.traced),lanes==vec4(ask.pool));
 rays.x=0u;
 return rays;
}
// The probe at `position` of the frame's claim order: every probe once, in
// index order from one the frame's hash chooses, so no probe always claims
// last.
fn ddgi_claimant(position:u32)->u32 {
 return (position+ddgi_phase_hash(volume.frame)%volume.probe_count)%volume.probe_count;
}
// The claim's workgroups each take DDGI_CLAIM_THREADS probes of the claim
// order, a block.
const DDGI_CLAIM_THREADS:u32=256u;
// The blocks each invocation of `claim` sums before its own block, at
// most: 256 times 256 blocks of 256 probes, past the 16.5 million probes
// whose rays at the most fit a u32 (`shading::dynamic_gi::fits`), the most
// a volume holds (AR-12).
const DDGI_CLAIM_WALK:u32=256u;
var<workgroup> claim_rays:array<vec4<u32>,DDGI_CLAIM_THREADS>;
var<workgroup> claim_earlier:array<vec4<u32>,DDGI_CLAIM_THREADS>;
// Each block of the claim order's rays in each pool, which `claim` sums
// over the blocks before its own.
@compute @workgroup_size(DDGI_CLAIM_THREADS)
fn claim_sums(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
 let block=ddgi_group_probe(group);
 let position=block*DDGI_CLAIM_THREADS+lane;
 var rays=vec4(0u);
 if position<volume.probe_count {
  rays=ddgi_pool_rays(ddgi_ask(ddgi_claimant(position)));
 }
 claim_rays[lane]=rays;
 for (var half=DDGI_CLAIM_THREADS/2u;half>0u;half/=2u) {
  workgroupBarrier();
  if lane<half {
   claim_rays[lane]+=claim_rays[lane+half];
  }
 }
 if lane==0u && block*DDGI_CLAIM_THREADS<volume.probe_count {
  allocation.claims[block]=claim_rays[0];
 }
}
// Grants each probe its claim where it fits its pool: where the rays of the
// probes before it in the claim order that claim in its pool, and its own,
// fit the pool's room. Each probe's grant is a function of the frame, not
// of the order its invocations run in, and the rays the frame traces never
// pass its budget, which the pools' rooms share. A claim past its room
// traces nothing, as does every later one in its pool that frame, leaving
// the pool less than one claim's rays.
@compute @workgroup_size(DDGI_CLAIM_THREADS)
fn claim(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
 let block=ddgi_group_probe(group);
 let position=block*DDGI_CLAIM_THREADS+lane;
 var ask=DdgiAsk(DDGI_POOL_NONE,0u,0u);
 if position<volume.probe_count {
  ask=ddgi_ask(ddgi_claimant(position));
 }
 let rays=ddgi_pool_rays(ask);
 // The earlier blocks' rays in each pool, summed by the workgroup.
 var earlier=vec4(0u);
 for (var walk=0u;walk<DDGI_CLAIM_WALK;walk++) {
  let before=lane+walk*DDGI_CLAIM_THREADS;
  if before>=block {
   break;
  }
  earlier+=allocation.claims[before];
 }
 claim_earlier[lane]=earlier;
 claim_rays[lane]=rays;
 for (var half=DDGI_CLAIM_THREADS/2u;half>0u;half/=2u) {
  workgroupBarrier();
  if lane<half {
   claim_earlier[lane]+=claim_earlier[lane+half];
  }
 }
 // The block's own, through the claim order (Hillis and Steele's scan).
 for (var reach=1u;reach<DDGI_CLAIM_THREADS;reach*=2u) {
  workgroupBarrier();
  var behind=vec4(0u);
  if lane>=reach {
   behind=claim_rays[lane-reach];
  }
  workgroupBarrier();
  claim_rays[lane]+=behind;
 }
 workgroupBarrier();
 if position>=volume.probe_count {
  return;
 }
 // Every pool's rays claimed before this probe's.
 let before=claim_earlier[0]+claim_rays[lane]-rays;
 var granted=ask.traced>0u;
 if ask.pool!=DDGI_POOL_NONE {
  // A block past the walk's cap refuses: its sum would be short.
  granted=block<=DDGI_CLAIM_WALK*DDGI_CLAIM_THREADS && before[ask.pool]+ask.traced<=ddgi_pool_room(ask.pool);
 }
 ray_counts[ddgi_claimant(position)]=select(0u,ask.blended,granted);
}
// Lists the rays of each probe its claim granted: the rays it blends, then
// its fixed rays, and the probes that trace them.
@compute @workgroup_size(ALLOCATION_THREADS)
fn allocate(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_index) group_index:u32) {
 let probe_index=ddgi_group_probe(group);
 if probe_index>=volume.probe_count {
  return;
 }
 if group_index==0u {
  let probe=ddgi_unpack_probe(probe_states[probe_index]);
  // The rays it blends, before its fixed rays, its fixed rays, and which of
  // its cycle's it traces, a whole cycle's marked DDGI_FIXED_CYCLE. A
  // probe not yet blended traces none.
  let blended=ray_counts[probe_index];
  let fixed_rays=select(0u,ddgi_turn_fixed_rays(probe),blended>0u);
  let traced=blended+fixed_rays;
  shared_entry_rays=blended;
  shared_traced=traced;
  shared_cycle=select(probe.fixed_frames,DDGI_FIXED_CYCLE,fixed_rays==DDGI_FIXED_RAYS);
  shared_ray_allocation=atomicAdd(&allocation.rays,traced);
  if traced>0u {
   traced_probes[atomicAdd(&allocation.traced,1u)]=probe_index;
  }
 }
 let entry_rays=workgroupUniformLoad(&shared_entry_rays);
 let ray_allocation=workgroupUniformLoad(&shared_ray_allocation);
 let cycle=workgroupUniformLoad(&shared_cycle);
 // At most a probe's slots, whatever the buffers hold (AR-12).
 let traced=min(workgroupUniformLoad(&shared_traced),DDGI_MOST_RAYS+DDGI_FIXED_RAYS_PER_FRAME);
 for (var i=group_index;i<traced;i+=ALLOCATION_THREADS) {
  textureStore(ray_list,ddgi_ray_texel(ray_allocation+i),vec4(ddgi_pack_ray_entry(DdgiRayEntry(probe_index,i,entry_rays,cycle)),0u,0u));
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
