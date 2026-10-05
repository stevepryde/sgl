// The portable implementation of the scene ray function set (the
// architecture's Ray source): every ray through the portable walk
// (scene_rays_walk.wgsl), the composition root of a tracing pipeline where
// the hardware path is not in effect. The hardware path's root
// (scene_rays_query_opaque.wgsl) defines the same functions.
// A valid ray's walk over the instances of `kinds` (SCENE_KIND_*), as
// `scene_walk` takes it; a miss for an invalid one.
fn scene_trace_portable(ray:SceneRay,kinds:u32,any_hit:bool,receiver:vec2<u32>,sides:u32,open_end:bool)->RawSceneHit {
 let miss=RawSceneHit(vec4(0u),vec4(0.));
 if !scene_ray_valid(ray) {
  return miss;
 }
 return scene_walk(ray,kinds,any_hit,receiver,sides,open_end,miss);
}

// The nearest hit accepting `sides` (SCENE_SIDES_*).
fn scene_trace_nearest(ray:SceneRay,sides:u32)->RawSceneHit {
 return scene_trace_portable(ray,SCENE_KINDS_ALL,false,vec2(0u),sides,false);
}

// Closed [t_min,t_max] visibility interval, blocked by the triangles whose
// `sides` (SCENE_SIDES_*) it accepts. Direction may be non-unit. Callers
// choose geometric ray-origin offsets and emitter endpoints; this adds no bias.
fn scene_segment_visible(origin:vec3<f32>,direction:vec3<f32>,t_min:f32,t_max:f32,sides:u32)->bool {
 let ray=SceneRay(vec4(origin,t_min),vec4(direction,t_max));
 return scene_trace_portable(ray,SCENE_KINDS_ALL,true,vec2(0u),sides,false).intersection.x==0u;
}

// The nearest hit of either kind, leaving `receiver`, as raster sides it.
fn scene_trace_nearest_except_receiver(ray:SceneRay,receiver:vec2<u32>)->RawSceneHit {
 return scene_trace_portable(ray,SCENE_KINDS_ALL,false,receiver,SCENE_SIDES_AS_RASTER,false);
}

// The nearest moving hit, leaving `receiver`, as raster sides it.
fn scene_trace_moving_except_receiver(ray:SceneRay,receiver:vec2<u32>)->RawSceneHit {
 return scene_trace_portable(ray,SCENE_KIND_MOVING,false,receiver,SCENE_SIDES_AS_RASTER,false);
}

// Visibility to a moving hit needs only a static any-hit in [t_min,t_hit).
// Like Wicked's TraceRay_Any (raytracingHF.hlsli, revision
// 2ff1d9e7b36091d6edf9f823af77e6bc9af20e3b, MIT), stop on the first blocker.
fn scene_static_segment_visible_except_receiver(ray:SceneRay,receiver:vec2<u32>)->bool {
 return scene_trace_portable(ray,SCENE_KIND_STATIC,true,receiver,SCENE_SIDES_AS_RASTER,true).intersection.x==0u;
}
