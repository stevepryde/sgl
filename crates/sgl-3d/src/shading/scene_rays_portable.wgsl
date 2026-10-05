// The portable implementation of the scene ray function set (the
// architecture's Ray source): every ray through the portable walk
// (scene_rays_walk.wgsl), the composition root of a tracing pipeline where
// the hardware path is not in effect. The hardware path's root
// (scene_rays_query_opaque.wgsl) defines the same functions.
// Both kinds: the static BVH, then the moving one within its nearest hit,
// accepting `sides` (SCENE_SIDES_*).
fn scene_trace_portable(ray:SceneRay,any_hit:bool,sides:u32)->RawSceneHit {
 let miss=RawSceneHit(vec4(0u),vec4(0.));
 if !scene_ray_valid(ray) {
  return miss;
 }
 var visits=0u;
 let statics=scene_trace_instances(scene_source[SCENE_HEADER_STATIC_ROOT],ray,any_hit,vec2(0u),sides,false,miss,&visits);
 if scene_bvh_exhausted(visits) {
  return miss;
 }
 if any_hit && statics.intersection.x!=0u {
  return statics;
 }
 let hit=scene_trace_instances(scene_source[SCENE_HEADER_MOVING_ROOT],ray,any_hit,vec2(0u),sides,false,statics,&visits);
 if scene_bvh_exhausted(visits) {
  return miss;
 }
 return hit;
}

fn scene_trace_moving_except_receiver(ray:SceneRay,receiver:vec2<u32>)->RawSceneHit {
 let miss=RawSceneHit(vec4(0u),vec4(0.));
 if !scene_ray_valid(ray) {
  return miss;
 }
 var visits=0u;
 let hit=scene_trace_instances(scene_source[SCENE_HEADER_MOVING_ROOT],ray,false,receiver,SCENE_SIDES_AS_RASTER,false,miss,&visits);
 if scene_bvh_exhausted(visits) {
  return miss;
 }
 return hit;
}

// Visibility to a moving hit needs only a static any-hit in [t_min,t_hit).
// Like Wicked's TraceRay_Any (raytracingHF.hlsli, revision
// 2ff1d9e7b36091d6edf9f823af77e6bc9af20e3b, MIT), stop on the first blocker.
fn scene_static_segment_visible_except_receiver(ray:SceneRay,receiver:vec2<u32>)->bool {
 let miss=RawSceneHit(vec4(0u),vec4(0.));
 if !scene_ray_valid(ray) {
  return true;
 }
 var visits=0u;
 let hit=scene_trace_instances(scene_source[SCENE_HEADER_STATIC_ROOT],ray,true,receiver,SCENE_SIDES_AS_RASTER,true,miss,&visits);
 return hit.intersection.x==0u || scene_bvh_exhausted(visits);
}

// The nearest hit accepting `sides` (SCENE_SIDES_*).
fn scene_trace_nearest(ray:SceneRay,sides:u32)->RawSceneHit {
 return scene_trace_portable(ray,false,sides);
}

// Closed [t_min,t_max] visibility interval, blocked by the triangles whose
// `sides` (SCENE_SIDES_*) it accepts. Direction may be non-unit. Callers
// choose geometric ray-origin offsets and emitter endpoints; this adds no bias.
fn scene_segment_visible(origin:vec3<f32>,direction:vec3<f32>,t_min:f32,t_max:f32,sides:u32)->bool {
 return scene_trace_portable(SceneRay(vec4(origin,t_min),vec4(direction,t_max)),true,sides).intersection.x==0u;
}
