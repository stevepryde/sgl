// The hardware path's shared module (the architecture's Hardware ray
// tracing), the hardware implementation of the scene ray function set,
// which every form's query module composes as the root of a tracing
// pipeline: the scene's TLAS, which only the passes that trace bind, in
// their group 3 at this one entry (`shading::bind::tlas_entry`); the
// re-trace past a hit the shared predicate rejects; the composition with
// the portable walk over the instances the TLAS does not hold; and the one
// per-ray budget. The query itself is the form's (`scene_hardware_query`).
// Each instance's custom index is its entry's index, and its mask its kind
// (`scene::rays::acceleration`, `MASK_STATIC` and `MASK_MOVING`), as Wicked
// Engine 2ff1d9e (`raytracingHF.hlsli`; `rtreflectionCS.hlsl` 74–147) and
// Bevy Solari b56fc29 (`raytracing_scene_bindings.wgsl` 96–104, 159–162)
// trace their scenes through one query and resolve a hit by instance and
// primitive.
enable wgpu_ray_query;
@group(3) @binding(16) var scene_tlas:acceleration_structure;
// The most hardware steps one ray takes (AR-12): the queries it starts,
// one for its first look and one for each re-trace past a hit the
// predicate rejects. A ray re-traces past every rejected triangle in front
// of the one it stops at: a back face of a single-sided closed mesh the
// ray starts inside, a blended mesh of a mixed model, a hidden group's
// triangle, its receiver's own triangle, the interval's open end and,
// under the baseline form, a cut-out texel of a masked deforming instance
// (masked models that do not deform are on the portable walk, with its own
// cap); a visibility ray whose first, any-hit look is rejected takes one
// more. Counted on the GPU by an instrumented build (#23): every one of
// the dynamic GI example's 6.79 million probe and visibility rays took one
// query; of 6.9 million world-space reflection rays over a glossy floor
// among moving boxes the most took three; and among skinned characters
// each crowned with a swaying bundle of 24 masked, double-sided hair cards
// three quarters cut out, under a dynamic GI volume with world-space
// reflections, the most of 7.8 million rays took 29, each cut-out card it
// crossed a re-trace. The cap is about nine times that, for denser hair,
// and still bounds every ray: a ray that reaches it reports a miss, or a
// visibility ray unoccluded, as the portable walk does at
// SCENE_BVH_MOST_VISITS.
const SCENE_MOST_HARDWARE_STEPS:u32=256u;
// Counts one more hardware step of a ray's `steps`, false once the ray has
// taken SCENE_MOST_HARDWARE_STEPS, after which it stays exhausted.
fn scene_hardware_step(steps:ptr<function,u32>)->bool {
 if *steps>=SCENE_MOST_HARDWARE_STEPS {
  *steps=SCENE_MOST_HARDWARE_STEPS+1u;
  return false;
 }
 *steps+=1u;
 return true;
}
fn scene_hardware_exhausted(steps:u32)->bool {
 return steps>SCENE_MOST_HARDWARE_STEPS;
}

// The bits of the least normal f32, 2^-126.
const SCENE_LEAST_NORMAL_BITS:u32=0x00800000u;
// The interval start that steps past a hit at nonnegative distance `t`:
// the next f32 above it, since an absolute step rounds back to the same
// distance far from the origin and repeats the hit; at least the least
// normal f32, since a GPU may flush a subnormal start to zero. Above
// f32::MAX it is infinite, past every interval's end.
fn scene_hardware_after(t:f32)->f32 {
 let bits=select(bitcast<u32>(t),0u,t<=0.);
 return bitcast<f32>(max(bits+1u,SCENE_LEAST_NORMAL_BITS));
}

// Whether the shared predicate accepts the committed hit `hit` of a ray
// leaving `receiver`, accepting `sides`, over its whole interval, whose end
// `open_end` excludes. The side comes from the triangle's winding in its
// model's space, from the positions the instance shows (deformed where it
// deforms): the hardware's own front-face flag follows each backend's
// convention and is never read.
fn scene_hardware_accepts(ray:SceneRay,hit:RawSceneHit,receiver:vec2<u32>,sides:u32,open_end:bool)->bool {
 let index=hit.intersection.y;
 let mesh_id=hit.intersection.z;
 let primitive_id=hit.intersection.w;
 let instance=scene_instances[index];
 let mesh=instance.mesh_word+mesh_id*SCENE_MESH_WORDS;
 if !scene_accepts_triangle(index,mesh,primitive_id,receiver) {
  return false;
 }
 let triangle=scene_triangle(mesh,primitive_id);
 let a=scene_shown_position(index,mesh,triangle.x);
 let e1=scene_shown_position(index,mesh,triangle.y)-a;
 let e2=scene_shown_position(index,mesh,triangle.z)-a;
 let direction=(instance.inverse_world*vec4(ray.direction.xyz,0.)).xyz;
 let candidate=SceneCandidate(index,mesh_id,primitive_id,hit.coords.x,hit.coords.yz,scene_winding(e1,e2,direction));
 return scene_accepts_solved(ray,candidate,mesh,ray.direction.w,sides,open_end);
}

// The nearest hit of valid `ray` in the TLAS's instances of kinds `mask`
// (SCENE_KIND_*, their instance masks) that the predicate accepts, or with `any_hit` any one,
// leaving `receiver`, accepting `sides`, its interval's end excluded when
// `open_end`; a miss once `steps` is exhausted. A rejected hit re-traces:
// the ray keeps its origin and direction and its interval starts past the
// rejected distance (scene_hardware_after), so the start rises strictly
// and the ray ends once it passes the interval's end. A triangle at
// exactly a rejected one's distance is skipped, since one opaque query
// cannot list ties. A visibility ray (`any_hit`) asks for any hit first:
// an accepted one occludes whatever its order and a miss is unoccluded; a
// rejected one carries no order, so the ray then asks nearest queries from
// its interval's start, stepping past each rejected nearest hit, so that
// no nearer occluder is skipped.
fn scene_hardware_trace(ray:SceneRay,mask:u32,any_hit:bool,receiver:vec2<u32>,sides:u32,open_end:bool,steps:ptr<function,u32>)->RawSceneHit {
 var accepted=RawSceneHit(vec4(0u),vec4(0.));
 var t_min=ray.origin.w;
 var first_hit=any_hit;
 loop {
  if t_min>ray.direction.w || !scene_hardware_step(steps) {
   break;
  }
  let hit=scene_hardware_query(ray.origin.xyz,ray.direction.xyz,t_min,ray.direction.w,mask,first_hit);
  if hit.intersection.x==0u {
   break;
  }
  if scene_hardware_accepts(ray,hit,receiver,sides,open_end) {
   accepted=hit;
   break;
  }
  if first_hit {
   first_hit=false;
  } else {
   t_min=scene_hardware_after(hit.coords.x);
  }
 }
 return accepted;
}

// The nearer of `ray`'s hit in the TLAS and the portable walk's over the
// instance BVHs of the same kinds (`kinds`), which on a hardware-traced
// frame bound the capture-visible instances the TLAS does not hold: those
// whose model has a masked mesh (predicate instances), whose model's BLAS
// is pending, or that the device's limits left out. A visibility ray is
// occluded by either. The walk keeps its own visit budget beside the
// hardware's steps; either exhausted, the ray reports a miss.
fn scene_trace_hardware(ray:SceneRay,kinds:u32,any_hit:bool,receiver:vec2<u32>,sides:u32,open_end:bool)->RawSceneHit {
 let miss=RawSceneHit(vec4(0u),vec4(0.));
 if !scene_ray_valid(ray) {
  return miss;
 }
 var steps=0u;
 let hit=scene_hardware_trace(ray,kinds,any_hit,receiver,sides,open_end,&steps);
 if scene_hardware_exhausted(steps) {
  return miss;
 }
 if any_hit && hit.intersection.x!=0u {
  return hit;
 }
 return scene_walk(ray,kinds,any_hit,receiver,sides,open_end,hit);
}

// The scene ray function set (the architecture's Ray source), as
// scene_rays_portable.wgsl defines it for the portable path.
// The nearest hit accepting `sides` (SCENE_SIDES_*).
fn scene_trace_nearest(ray:SceneRay,sides:u32)->RawSceneHit {
 return scene_trace_hardware(ray,SCENE_KINDS_ALL,false,vec2(0u),sides,false);
}

// Closed [t_min,t_max] visibility interval, blocked by the triangles whose
// `sides` (SCENE_SIDES_*) it accepts. Direction may be non-unit. Callers
// choose geometric ray-origin offsets and emitter endpoints; this adds no bias.
fn scene_segment_visible(origin:vec3<f32>,direction:vec3<f32>,t_min:f32,t_max:f32,sides:u32)->bool {
 let ray=SceneRay(vec4(origin,t_min),vec4(direction,t_max));
 return scene_trace_hardware(ray,SCENE_KINDS_ALL,true,vec2(0u),sides,false).intersection.x==0u;
}

// The nearest hit of either kind, leaving `receiver`, as raster sides it.
fn scene_trace_nearest_except_receiver(ray:SceneRay,receiver:vec2<u32>)->RawSceneHit {
 return scene_trace_hardware(ray,SCENE_KINDS_ALL,false,receiver,SCENE_SIDES_AS_RASTER,false);
}

// The nearest moving hit, leaving `receiver`, as raster sides it.
fn scene_trace_moving_except_receiver(ray:SceneRay,receiver:vec2<u32>)->RawSceneHit {
 return scene_trace_hardware(ray,SCENE_KIND_MOVING,false,receiver,SCENE_SIDES_AS_RASTER,false);
}

// Visibility to a moving hit needs only a static any-hit in [t_min,t_hit).
fn scene_static_segment_visible_except_receiver(ray:SceneRay,receiver:vec2<u32>)->bool {
 return scene_trace_hardware(ray,SCENE_KIND_STATIC,true,receiver,SCENE_SIDES_AS_RASTER,true).intersection.x==0u;
}
