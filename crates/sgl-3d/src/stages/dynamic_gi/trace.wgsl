// The dynamic GI stage's trace: each allocated ray from its probe through
// the scene's ray source, a hit shaded as the probe-hit receiver kind
// (surface_ray.wgsl's shade_ray_hit), a miss taking the environment and the
// hemisphere fill along the ray. Group 0 is the volume's lit group, group 1
// the scene's; the stage's own are at group 3.
//
// Ports Wicked Engine df44c3db4c4927492bc9c791eac715d98d7ed091,
// WickedEngine/shaders/ddgi_raytraceCS.hlsl (spherical Fibonacci directions
// 22–31 under the frame's random rotation 51–52, the trace 228–251 culling
// no side, the miss to the sky 253–279, the hit 283–326 with its depth
// pushed inwards on a back face, its light 329–490 and bounce 492–500 in
// shade_ray_hit, the store 505–511) and globals.hlsli's RNG (1131–1190), MIT
// (src/LICENSE-wicked.txt). Not taken: the direct light Wicked gathers from
// one static light at the probe itself (54–226): SGL3D's baked lights reach
// every receiver without baked lighting in the forward pass, and a light
// enters the probes through the hits it lights. Changed: the miss takes the
// diffuse environment's radiance along the ray (its yaw and intensity) and
// the hemisphere fill as the radiance whose irradiance it is; a ray that
// meets a single-sided material from behind, the inside of closed geometry
// or the outside of a shell built to be seen from within, which Wicked
// shades as a back face, brings no light and shortens its depth to a fifth,
// as Majercik et al. 2021 (section 4.1) and NVIDIA RTXGI's probe trace (ProbeTraceRGS,
// practice only) treat back-face hits: the probe takes nothing from behind
// the surface, and the receivers beyond it weigh the probe as occluded. One
// difference from RTXGI: its ProbeBlendingCS skips back-face rays in the
// radiance blend and takes |0.2 t| only as their distance, where here they
// count black at their full cosine weight in the irradiance blend, which
// darkens a probe that sees back faces rather than brightening it.
@group(3) @binding(0) var<uniform> volume:DdgiVolume;
@group(3) @binding(1) var ray_list:texture_2d<u32>;
@group(3) @binding(2) var ray_results:texture_storage_2d<rgba32uint,write>;
// Wicked's RNG: xoroshiro64*, seeded through Thomas Wang's hash.
struct DdgiRng {
 s:vec2<u32>,
}
fn ddgi_rng_rotl(x:u32,k:u32)->u32 {
 return (x<<k)|(x>>(32u-k));
}
fn ddgi_rng_next(rng:ptr<function,DdgiRng>)->u32 {
 var s=(*rng).s;
 let result=s.x*0x9e3779bbu;
 s.y^=s.x;
 s.x=ddgi_rng_rotl(s.x,26u)^s.y^(s.y<<9u);
 s.y=ddgi_rng_rotl(s.y,13u);
 (*rng).s=s;
 return result;
}
fn ddgi_rng_hash(seed_in:u32)->u32 {
 var seed=(seed_in^61u)^(seed_in>>16u);
 seed*=9u;
 seed=seed^(seed>>4u);
 seed*=0x27d4eb2du;
 seed=seed^(seed>>15u);
 return seed;
}
fn ddgi_rng_init(id:vec2<u32>,frame_index:u32)->DdgiRng {
 var rng=DdgiRng(vec2(ddgi_rng_hash((id.x<<16u)|id.y),ddgi_rng_hash(frame_index)));
 ddgi_rng_next(&rng);
 return rng;
}
fn ddgi_rng_next_float(rng:ptr<function,DdgiRng>)->f32 {
 return bitcast<f32>(0x3f800000u|(ddgi_rng_next(rng)>>9u))-1.;
}
// A single-sided back-face hit's share of its distance in the depth map:
// RTXGI's 0.2. A double-sided material's back face, which is a surface,
// takes Wicked's 0.9.
const DDGI_BACKFACE_DEPTH:f32=.2;
const DDGI_DOUBLE_SIDED_BACKFACE_DEPTH:f32=.9;
// Ray `i` of `n` spread evenly over the sphere.
fn ddgi_spherical_fibonacci(i:f32,n:f32)->vec3<f32> {
 let golden=sqrt(5.)*.5+.5;
 let phi=6.28318530718*fract(i*(golden-1.));
 let cos_theta=1.-(2.*i+1.)*(1./n);
 let sin_theta=sqrt(clamp(1.-cos_theta*cos_theta,0.,1.));
 return vec3(cos(phi)*sin_theta,sin(phi)*sin_theta,cos_theta);
}
// A traced ray: where its result goes, the result, and with ray
// observation what its walks cost (ddgi_trace_ray).
struct DdgiTraced {
 texel:vec2<u32>,
 ray:DdgiRay,
 fixed:bool,
 // The probe ray's own query, and the visibility query its hit cast.
 nearest:SceneRayWalks,
 visibility:SceneRayWalks,
}
// Ray `id` of the frame's rays, traced.
fn ddgi_trace_ray(id:u32)->DdgiTraced {
 let entry=ddgi_unpack_ray_entry(textureLoad(ray_list,ddgi_ray_texel(id),0).xy);
 let probe_index=entry.probe;
 let ray_index=entry.ray;
 let ray_count=entry.rays;
 let stored=ddgi_probe_coord(probe_index,volume.probes);
 let probe_data=textureLoad(dynamic_gi_probes,ddgi_probe_data_pixel(stored,volume.probes),0);
 let lattice=ddgi_probe_lattice(stored,volume.probes,volume.scroll);
 let probe_pos=ddgi_probe_position(lattice,volume.origin,volume.spacing,probe_data.rgb);
 var rng=ddgi_rng_init(vec2(id,id),volume.frame);
 // Past its rays, its fixed rays, unrotated, which classify it and bring
 // no light: this turn's of its cycle, or on its first turn
 // (DDGI_FIXED_CYCLE) all of them.
 let fixed=ray_index>=ray_count;
 var direction=normalize(volume.rotation*ddgi_spherical_fibonacci(f32(ray_index),f32(ray_count)));
 if fixed {
  let first=entry.cycle>=DDGI_FIXED_CYCLE;
  let ray=select(entry.cycle*DDGI_FIXED_RAYS_PER_FRAME,0u,first)+ray_index-ray_count;
  direction=ddgi_spherical_fibonacci(f32(ray),f32(DDGI_FIXED_RAYS));
 }
 var ray=DdgiRay(direction,-1.,vec3(0.),false);
 // No interval start: the ray leaves a probe, not a surface.
 let raw=scene_trace_nearest(SceneRay(vec4(probe_pos,0.),vec4(direction,3.402823466e+38)),SCENE_SIDES_BOTH);
 let nearest=scene_ray_walks;
 if raw.intersection.x==0u {
  if !fixed {
   ray.radiance=sample_environment(direction,0.)+pbr_hemisphere_radiance(direction,frame.hemisphere_sky_color,frame.hemisphere_ground_color,frame.hemisphere_intensity);
  }
 } else {
  let hit=scene_decode_hit(raw,probe_pos,direction);
  ray.depth=hit.distance;
  let double_sided=(scene_material(hit.material_word).values.flags&MATERIAL_DOUBLE_SIDED)!=0u;
  if !hit.front_face && !double_sided {
   ray.depth*=DDGI_BACKFACE_DEPTH;
   ray.backface=true;
  } else {
   if !hit.front_face {
    // Pushed inwards, which helps keep light inside from leaking out.
    ray.depth*=DDGI_DOUBLE_SIDED_BACKFACE_DEPTH;
   }
   if !fixed {
    let random=vec3(ddgi_rng_next_float(&rng),ddgi_rng_next_float(&rng),ddgi_rng_next_float(&rng));
    ray.radiance=shade_ray_hit(hit,-direction,SHADOW_RECEIVER_PROBE_HIT,random);
   }
  }
 }
 let visibility=SceneRayWalks(scene_ray_walks.queries-nearest.queries,scene_ray_walks.visits-nearest.visits,scene_ray_walks.exhausted-nearest.exhausted);
 return DdgiTraced(ddgi_ray_texel(ddgi_ray_slot(probe_index,ray_index,volume.max_rays)),ray,fixed,nearest,visibility);
}
@compute @workgroup_size(DDGI_TRACE_THREADS)
fn trace(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
 let id=ddgi_group_probe(group)*DDGI_TRACE_THREADS+lane;
 if id>=volume.rays {
  return;
 }
 let traced=ddgi_trace_ray(id);
 textureStore(ray_results,traced.texel,ddgi_pack_ray(traced.ray));
}
// A query's cost in a word of ray_costs (DDGI_COST_*).
fn ddgi_pack_ray_cost(walks:SceneRayWalks,fixed:bool,hit:bool)->u32 {
 var word=min(walks.visits,DDGI_COST_VISITS);
 word|=select(0u,DDGI_COST_QUERIED,walks.queries>0u);
 word|=select(0u,DDGI_COST_EXHAUSTED,walks.exhausted>0u);
 word|=select(0u,DDGI_COST_HIT,hit);
 word|=select(0u,DDGI_COST_FIXED,fixed);
 return word;
}
// The trace observed (feature diagnostics), its pipeline with
// ray_observation_enabled: each ray's texel of the ray list's size in
// ray_costs also takes what its walks cost (ddgi_pack_ray_cost), which
// observe.wgsl sums.
@group(2) @binding(0) var ray_costs:texture_storage_2d<rg32uint,write>;
@compute @workgroup_size(DDGI_TRACE_THREADS)
fn trace_observed(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
 let id=ddgi_group_probe(group)*DDGI_TRACE_THREADS+lane;
 if id>=volume.rays {
  return;
 }
 let traced=ddgi_trace_ray(id);
 textureStore(ray_results,traced.texel,ddgi_pack_ray(traced.ray));
 let hit=traced.ray.depth>=0.;
 textureStore(ray_costs,ddgi_ray_texel(id),vec4(ddgi_pack_ray_cost(traced.nearest,traced.fixed,hit),ddgi_pack_ray_cost(traced.visibility,false,false),0u,0u));
}
