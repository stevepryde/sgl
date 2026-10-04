@group(3) @binding(0) var<storage,read> query_rays:array<SceneRay>;
@group(3) @binding(1) var<storage,read_write> query_hits:array<RawSceneHit>;
@group(3) @binding(2) var<uniform> query_limits:vec4<u32>;
@compute @workgroup_size(64) fn scene_intersect(@builtin(global_invocation_id) gid:vec3<u32>) {
 let pixel=gid.x+gid.y*query_limits.w;
 if pixel>=query_limits.x {return;}
 query_hits[pixel]=scene_trace_nearest(query_rays[pixel]);
}
