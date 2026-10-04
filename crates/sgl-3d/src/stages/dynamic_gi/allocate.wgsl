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
@group(0) @binding(0) var<uniform> volume:DdgiVolume;
@group(0) @binding(1) var<storage,read> variance:array<u32>;
@group(0) @binding(2) var<storage,read> probe_states:array<vec2<u32>>;
@group(0) @binding(3) var<storage,read_write> ray_counts:array<u32>;
@group(0) @binding(4) var<storage,read_write> allocation:DdgiAllocation;
@group(0) @binding(5) var ray_list:texture_storage_2d<rg32uint,write>;
// The trace's indirect dispatch and the rays the frame traces.
struct DdgiAllocation {
 groups:array<u32,3>,
 rays:atomic<u32>,
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
@compute @workgroup_size(ALLOCATION_THREADS)
fn allocate(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_index) group_index:u32) {
 let probe_index=ddgi_group_probe(group);
 if probe_index>=volume.probe_count {
  return;
 }
 let probe_coord=ddgi_probe_coord(probe_index,volume.probes);
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
  var ray_count=u32(saturate(max_inconsistency)*f32(volume.max_rays));
  let spacing=volume.spacing;
  if !camera_frustum_intersects(probe_pos,max(spacing.x,max(spacing.y,spacing.z))*2.) {
   ray_count=u32(f32(ray_count)*.1);
  }
  ray_count=(ray_count+DDGI_RAY_BUCKET_COUNT-1u)/DDGI_RAY_BUCKET_COUNT*DDGI_RAY_BUCKET_COUNT;
  ray_count=clamp(ray_count,DDGI_RAY_BUCKET_COUNT,volume.max_rays);
  if !probe.blended {
   ray_count=volume.max_rays;
  }
  ray_counts[probe_index]=ray_count;
  shared_ray_count=ray_count;
  shared_ray_allocation=atomicAdd(&allocation.rays,ray_count);
 }
 let ray_count=workgroupUniformLoad(&shared_ray_count);
 let ray_allocation=workgroupUniformLoad(&shared_ray_allocation);
 for (var i=group_index;i<ray_count;i+=ALLOCATION_THREADS) {
  textureStore(ray_list,ddgi_ray_texel(ray_allocation+i),vec4(probe_index,i|(ray_count<<16u),0u,0u));
 }
}
// The trace's indirect dispatch: a workgroup for each DDGI_TRACE_THREADS
// rays, in rows of DDGI_GROUP_ROW.
@compute @workgroup_size(1)
fn prepare_trace() {
 let rays=atomicLoad(&allocation.rays);
 let groups=(rays+DDGI_TRACE_THREADS-1u)/DDGI_TRACE_THREADS;
 allocation.groups[0]=min(groups,DDGI_GROUP_ROW);
 allocation.groups[1]=(groups+DDGI_GROUP_ROW-1u)/DDGI_GROUP_ROW;
 allocation.groups[2]=1u;
}
