// The baseline form's query (the architecture's Hardware ray tracing,
// *Baseline form*), the composition root of the hardware path on every
// native backend. Every BLAS geometry is opaque, and a query asks the
// hardware for its cull mask and interval and for nothing else: naga 29's
// MSL writer sets no triangle cull mode, and a global cull would be wrong
// for mirrored instances and double-sided materials, so the shared
// predicate judges the committed hit (scene_hardware_trace).
// The committed hit of one query over kinds `mask` (SCENE_KIND_*, their
// instance masks) in [t_min,t_max], the nearest or, with `first_hit`, the
// first the hardware finds (Metal's `accept_any_intersection`). The ray forces opacity, so no
// candidate reaches the shader and one proceed ends the traversal on every
// backend: naga's MSL writer intersects at initialisation and returns true
// from every proceed until the query terminates, so a proceed is never
// looped on (`back/msl/writer.rs` 4111–4146).
fn scene_hardware_query(origin:vec3<f32>,direction:vec3<f32>,t_min:f32,t_max:f32,mask:u32,first_hit:bool)->RawSceneHit {
 var query:ray_query;
 let flags=RAY_FLAG_FORCE_OPAQUE|select(RAY_FLAG_NONE,RAY_FLAG_TERMINATE_ON_FIRST_HIT,first_hit);
 rayQueryInitialize(&query,scene_tlas,RayDesc(flags,mask,t_min,t_max,origin,direction));
 _=rayQueryProceed(&query);
 let committed=rayQueryGetCommittedIntersection(&query);
 if committed.kind!=RAY_QUERY_INTERSECTION_TRIANGLE {
  return RawSceneHit(vec4(0u),vec4(0.));
 }
 return RawSceneHit(vec4(1u,committed.instance_custom_data,committed.geometry_index,committed.primitive_index),vec4(committed.t,committed.barycentrics,0.));
}
