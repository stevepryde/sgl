// The baseline form's query (the architecture's Hardware ray tracing,
// *Baseline form*), the composition root of the hardware path on Vulkan
// and DX12 and on a device that fell back from the candidate form. Every
// BLAS geometry is opaque, and a query asks the
// hardware for its cull mask and interval and for nothing else: naga 30's
// MSL writer sets no triangle cull mode, and a global cull would be wrong
// for mirrored instances and double-sided materials, so the shared
// predicate judges the committed hit (scene_hardware_trace).
// The committed hit of one query of `ray` from `t_min` over kinds `mask`
// (SCENE_KIND_*, their instance masks), the nearest or, with `first_hit`,
// the first the hardware finds (Metal's `accept_any_intersection`), for
// the shared predicate to judge: no candidate reaches the shader, so this
// form takes no step of `steps` and judges nothing itself (`receiver`,
// `sides` and `open_end` are the candidate form's). The ray forces
// opacity, so one proceed ends the traversal on every backend: forced-opaque
// geometry yields no candidate, so the first proceed completes it. On
// Metal, naga 30 passes the cull mask to the query's `reset` and writes a
// proceed as one `intersection_query::next()` (`back/msl/ray.rs` 365–368,
// 386–417), which finishes traversal for opaque geometry, so a proceed is
// never looped on.
fn scene_hardware_query(ray:SceneRay,t_min:f32,mask:u32,first_hit:bool,receiver:vec2<u32>,sides:u32,open_end:bool,steps:ptr<function,u32>)->RawSceneHit {
 var query:ray_query;
 let flags=RAY_FLAG_FORCE_OPAQUE|select(RAY_FLAG_NONE,RAY_FLAG_TERMINATE_ON_FIRST_HIT,first_hit);
 rayQueryInitialize(&query,scene_tlas,RayDesc(flags,mask,t_min,ray.direction.w,ray.origin.xyz,ray.direction.xyz));
 // A query whose proceed reports a candidate, which forced opacity over
 // triangles never does, has not finished traversal: its committed hit is
 // not to be read (wgpu's ShaderRuntimeChecks), so it reports a miss.
 if rayQueryProceed(&query) {
  return RawSceneHit(vec4(0u),vec4(0.));
 }
 let committed=rayQueryGetCommittedIntersection(&query);
 if committed.kind!=RAY_QUERY_INTERSECTION_TRIANGLE {
  return RawSceneHit(vec4(0u),vec4(0.));
 }
 return RawSceneHit(vec4(SCENE_HARDWARE_COMMITTED,committed.instance_custom_data,committed.geometry_index,committed.primitive_index),vec4(committed.t,committed.barycentrics,0.));
}
