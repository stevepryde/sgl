// The candidate form's query (the architecture's Hardware ray tracing,
// *Candidate form*), the composition root of the hardware path where the
// backend lowers a candidate loop: naga 29's SPIR-V writer (Vulkan;
// `back/spv/ray/query.rs`, proceed 1037–1160, confirm 1460–1590, candidate
// and committed reads 109–594) and HLSL writer (DX12; `back/hlsl/ray.rs`,
// `Proceed` 424, `CommitNonOpaqueTriangleHit` 529, candidate reads
// 143–222), never its MSL writer, which runs no candidate loop. Every BLAS
// geometry is opaque but a masked mesh's, so the hardware reports each
// triangle of a masked mesh a query crosses as a candidate, and the loop
// runs the whole shared predicate on it, the cut-out test included,
// confirming an accepted one, as Wicked Engine 4323a33c confirms its
// alpha-tested candidates (`rtreflectionCS.hlsl` 82–109;
// `screenspaceshadowCS.hlsl` 232–256). Opaque geometry never yields a
// candidate: its committed hit goes to the shared predicate and the
// re-trace as the baseline's does (scene_hardware_trace), with the side
// rules, since the per-instance cull exemptions Wicked gives mirrored
// instances and double-sided materials are not available.
// The committed hit of one query of `ray` from `t_min` over kinds `mask`
// (SCENE_KIND_*, their instance masks), the nearest or, with `first_hit`,
// the first committed, each candidate on the way judged by the shared
// predicate (with `receiver`, `sides` and `open_end`) and confirmed where
// it accepts it. Each candidate is a step of `steps` (AR-12): at the cap
// the query stops and the ray reports a miss. It stops by returning, never
// through `rayQueryTerminate`: naga 29.0.4's SPIR-V writer caches its
// terminate helper under proceed's key (`back/spv/ray/query.rs` 1869–1870),
// so a proceed it writes after one would call the terminate helper.
fn scene_hardware_query(ray:SceneRay,t_min:f32,mask:u32,first_hit:bool,receiver:vec2<u32>,sides:u32,open_end:bool,steps:ptr<function,u32>)->RawSceneHit {
 let miss=RawSceneHit(vec4(0u),vec4(0.));
 var query:ray_query;
 let flags=select(RAY_FLAG_NONE,RAY_FLAG_TERMINATE_ON_FIRST_HIT,first_hit);
 rayQueryInitialize(&query,scene_tlas,RayDesc(flags,mask,t_min,ray.direction.w,ray.origin.xyz,ray.direction.xyz));
 // The triangle (entry, geometry, primitive) last confirmed. The committed
 // hit is one the predicate accepted where it is still that triangle: an
 // opaque triangle the hardware commits after it is never a candidate. No
 // entry's index reaches 2^24, so at first it names no triangle.
 var confirmed=vec3(0xffffffffu);
 loop {
  if !rayQueryProceed(&query) {
   break;
  }
  if !scene_hardware_step(steps) {
   return miss;
  }
  let candidate=rayQueryGetCandidateIntersection(&query);
  if candidate.kind!=RAY_QUERY_INTERSECTION_TRIANGLE {
   continue;
  }
  let triangle=vec3(candidate.instance_custom_data,candidate.geometry_index,candidate.primitive_index);
  let hit=RawSceneHit(vec4(SCENE_HARDWARE_COMMITTED,triangle),vec4(candidate.t,candidate.barycentrics,0.));
  if scene_hardware_accepts(ray,hit,receiver,sides,open_end) {
   rayQueryConfirmIntersection(&query);
   confirmed=triangle;
  }
 }
 let committed=rayQueryGetCommittedIntersection(&query);
 if committed.kind!=RAY_QUERY_INTERSECTION_TRIANGLE {
  return miss;
 }
 let triangle=vec3(committed.instance_custom_data,committed.geometry_index,committed.primitive_index);
 let judged=select(SCENE_HARDWARE_COMMITTED,SCENE_HARDWARE_ACCEPTED,all(triangle==confirmed));
 return RawSceneHit(vec4(judged,triangle),vec4(committed.t,committed.barycentrics,0.));
}
