@group(3) @binding(0) var<storage,read> query_rays:array<SceneRay>;
@group(3) @binding(1) var<storage,read_write> query_hits:array<RawSceneHit>;
// The rays' count, the side policy they accept (SCENE_SIDES_*), the
// function they take (`scene::rays::query::Function`) and a row's
// invocations; then the receiver the functions that leave one leave.
@group(3) @binding(2) var<uniform> query_limits:array<vec4<u32>,2>;
// A visibility function's result as a hit's first word: 1 where visible.
fn query_visible(visible:bool)->RawSceneHit {
 return RawSceneHit(vec4(select(0u,1u,visible),0u,0u,0u),vec4(0.));
}
@compute @workgroup_size(64) fn scene_intersect(@builtin(global_invocation_id) gid:vec3<u32>) {
 let pixel=gid.x+gid.y*query_limits[0].w;
 if pixel>=query_limits[0].x {return;}
 let ray=query_rays[pixel];
 let sides=query_limits[0].y;
 let receiver=query_limits[1].xy;
 switch query_limits[0].z {
  case 1u: {
   query_hits[pixel]=query_visible(scene_segment_visible(ray.origin.xyz,ray.direction.xyz,ray.origin.w,ray.direction.w,sides));
  }
  case 2u: {
   query_hits[pixel]=scene_trace_moving_except_receiver(ray,receiver);
  }
  case 3u: {
   query_hits[pixel]=query_visible(scene_static_segment_visible_except_receiver(ray,receiver));
  }
  case 4u: {
   let hit=scene_decode_hit(scene_trace_nearest(ray,sides),ray.origin.xyz,ray.direction.xyz);
   query_hits[pixel]=RawSceneHit(vec4(select(0u,1u,hit.hit),bitcast<vec3<u32>>(hit.normal)),vec4(hit.geometric_normal,hit.distance));
  }
  case 5u: {
   query_hits[pixel]=scene_trace_nearest_except_receiver(ray,receiver);
  }
  default: {
   query_hits[pixel]=scene_trace_nearest(ray,sides);
  }
 }
}
