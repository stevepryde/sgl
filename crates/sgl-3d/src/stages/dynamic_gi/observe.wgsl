// The dynamic GI stage's observation (feature diagnostics): sums what the
// observed trace's rays cost (trace.wgsl's trace_observed, DDGI_COST_*)
// into the frame's observation, one invocation a ray as the trace
// dispatches them, which the stage copies for readback beside its
// allocation's counts and its convergence.
@group(0) @binding(0) var<uniform> volume:DdgiVolume;
@group(0) @binding(1) var ray_list:texture_2d<u32>;
@group(0) @binding(2) var ray_costs:texture_2d<u32>;
@group(0) @binding(3) var<storage,read_write> observation:DdgiObservation;
// The probes' bins of rays traced beside their fixed rays: 4, 5-8, 9-16,
// 17-32, 33-64, 65-128, 129 to one fewer than DDGI_MOST_RAYS, and
// DDGI_MOST_RAYS.
const DDGI_OBSERVED_RAY_BINS:u32=8u;
// The frame's sums: the rays and fixed rays traced, those that hit, the
// visibility rays their hits cast, the nodes each kind's walks visited (a
// low and a high word) and the most one query visited, the queries whose
// walks stopped at SCENE_BVH_MOST_VISITS, and the probes that traced by
// their rays.
struct DdgiObservation {
 rays:atomic<u32>,
 fixed_rays:atomic<u32>,
 hits:atomic<u32>,
 visibility_rays:atomic<u32>,
 ray_visits:atomic<u32>,
 ray_visits_high:atomic<u32>,
 visibility_visits:atomic<u32>,
 visibility_visits_high:atomic<u32>,
 most_ray_visits:atomic<u32>,
 most_visibility_visits:atomic<u32>,
 exhausted:atomic<u32>,
 probes:array<atomic<u32>,DDGI_OBSERVED_RAY_BINS>,
}
@compute @workgroup_size(DDGI_TRACE_THREADS)
fn observe(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
 let id=ddgi_group_probe(group)*DDGI_TRACE_THREADS+lane;
 if id>=volume.rays {
  return;
 }
 let ray_alloc=textureLoad(ray_list,ddgi_ray_texel(id),0).xy;
 let probe_index=ray_alloc.x;
 let ray_index=ray_alloc.y&0xffffu;
 let ray_count=ray_alloc.y>>16u;
 let costs=textureLoad(ray_costs,ddgi_ray_texel(ddgi_ray_slot(probe_index,ray_index,volume.max_rays)),0).xy;
 let own=costs.x;
 let visibility=costs.y;
 if (own&DDGI_COST_FIXED)!=0u {
  atomicAdd(&observation.fixed_rays,1u);
 } else {
  atomicAdd(&observation.rays,1u);
 }
 if (own&DDGI_COST_HIT)!=0u {
  atomicAdd(&observation.hits,1u);
 }
 let own_visits=own&DDGI_COST_VISITS;
 if atomicAdd(&observation.ray_visits,own_visits)>0xffffffffu-own_visits {
  atomicAdd(&observation.ray_visits_high,1u);
 }
 atomicMax(&observation.most_ray_visits,own_visits);
 if (visibility&DDGI_COST_QUERIED)!=0u {
  atomicAdd(&observation.visibility_rays,1u);
  let visibility_visits=visibility&DDGI_COST_VISITS;
  if atomicAdd(&observation.visibility_visits,visibility_visits)>0xffffffffu-visibility_visits {
   atomicAdd(&observation.visibility_visits_high,1u);
  }
  atomicMax(&observation.most_visibility_visits,visibility_visits);
 }
 let exhausted=select(0u,1u,(own&DDGI_COST_EXHAUSTED)!=0u)+select(0u,1u,(visibility&DDGI_COST_EXHAUSTED)!=0u);
 if exhausted>0u {
  atomicAdd(&observation.exhausted,exhausted);
 }
 // A probe's first ray counts the probe.
 if ray_index==0u {
  let bin=select(min(firstLeadingBit(max(ray_count,4u)-1u)-1u,DDGI_OBSERVED_RAY_BINS-2u),DDGI_OBSERVED_RAY_BINS-1u,ray_count>=DDGI_MOST_RAYS);
  atomicAdd(&observation.probes[bin],1u);
 }
}
