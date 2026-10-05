// Application-owned finite-interval geometry queries through the scene's
// two-level ray source: the instance BVH of each kind, static and moving,
// over the instances' posed model bounds, whose leaves name instance entries,
// then each instance's model BVH with the ray in that model's space, as DXR
// and Vulkan traverse a TLAS and its BLASes (Wald et al. 2003). PBRT4
// EqualCounts BVHs; Moller-Trumbore triangle solve. `scene::rays::bvh` builds
// every tree, and `scene::rays::instances` the instance BVHs.
// A BVH node's words in the source (`scene::rays::bvh::Node`): its bounds, the
// word after its subtree, and a leaf's primitive count and first primitive. An
// interior node's first child follows it.
const SCENE_BVH_NODE_WORDS:u32=12u;
const SCENE_BVH_NODE_MIN:u32=0u;
const SCENE_BVH_NODE_ESCAPE:u32=3u;
const SCENE_BVH_NODE_MAX:u32=4u;
const SCENE_BVH_NODE_COUNT:u32=7u;
const SCENE_BVH_NODE_FIRST:u32=8u;
// A leaf primitive's: a triangle of one of the model's meshes.
const SCENE_BVH_PRIMITIVE_WORDS:u32=2u;
const SCENE_BVH_PRIMITIVE_MESH:u32=0u;
const SCENE_BVH_PRIMITIVE_TRIANGLE:u32=1u;
// An instance BVH leaf's: the index of the instance entry it names.
const SCENE_BVH_INSTANCE_WORDS:u32=1u;
// The most records a leaf names, in either kind of BVH
// (`scene::rays::bvh::LEAF_PRIMITIVES`).
const SCENE_BVH_LEAF_PRIMITIVES:u32=4u;
// The most BVH nodes one ray visits across its instance walk and the model
// walks it starts (AR-12), as AMD's FidelityFX SSSR caps a ray's hierarchy
// lookups at `max_traversal_intersections` (FidelityFX SDK
// samples/hybridreflections/shaders/ffx_sssr_intersect.h, revision
// c6efa6bf7f2027b3ec94f28578bb5965eabb9e55). On the CPU, the most a ray
// visited in this walk was 2,081 over a 2-million-triangle terrain at grazing
// angles, 1,173 inside half a million foliage triangles and 19,238 across a
// forest of 40,000 instances of a 5,000-triangle tree at 0 to 3 degrees
// (#160); the cap is over three times the forest's. A ray that reaches it
// reports a miss.
const SCENE_BVH_MOST_VISITS:u32=65536u;
// A walk of a BVH ends whatever the source holds, so corrupt words (a stale
// range, a rebase error) cost a wrong answer, never an unbounded loop that
// hangs the GPU: a ray visits at most SCENE_BVH_MOST_VISITS nodes; every node
// a walk visits lies whole within the source, its end clamped there; each
// step moves forward, as a node's subtree ends after the node, so a walk
// stops at an escape that does not, and its ray reports a miss as at the
// cap; and a leaf names at most a leaf's records, all within the source.
// Counts one more node visit of a ray's `visits`, false once the ray has
// made SCENE_BVH_MOST_VISITS, after which it stays exhausted.
fn scene_bvh_visit(visits:ptr<function,u32>)->bool {
 if *visits>=SCENE_BVH_MOST_VISITS {
  *visits=SCENE_BVH_MOST_VISITS+1u;
  return false;
 }
 *visits+=1u;
 return true;
}
// Whether a ray's walks stopped at SCENE_BVH_MOST_VISITS or at a link that
// does not lead forward: either way the ray reports a miss.
fn scene_bvh_exhausted(visits:u32)->bool {
 return visits>SCENE_BVH_MOST_VISITS;
}
// The word a walk of the BVH at `root` ends before.
fn scene_bvh_end(root:u32)->u32 {
 let length=arrayLength(&scene_source);
 if length<SCENE_BVH_NODE_WORDS || root>length-SCENE_BVH_NODE_WORDS {
  return root;
 }
 return min(scene_source[root+SCENE_BVH_NODE_ESCAPE],length-SCENE_BVH_NODE_WORDS+1u);
}
// Whether `escape` lies past node `node`, as every valid node's does.
fn scene_bvh_forward(node:u32,escape:u32)->bool {
 return escape>=node+SCENE_BVH_NODE_WORDS;
}
// Leaf `node`'s first record and how many it names of `words` each.
fn scene_bvh_leaf(node:u32,words:u32)->vec2<u32> {
 let first=scene_source[node+SCENE_BVH_NODE_FIRST];
 let length=arrayLength(&scene_source);
 let room=select(0u,(length-first)/words,first<length);
 return vec2(first,min(min(scene_source[node+SCENE_BVH_NODE_COUNT],SCENE_BVH_LEAF_PRIMITIVES),room));
}
// Which sides of a triangle a ray accepts, beside the receiver it leaves:
// a camera-origin ray rejects a single-sided material's back faces, as
// raster culls them; a dynamic GI probe ray and its visibility ray accept
// both sides of every triangle, as Wicked Engine's DDGI trace culls none.
const SCENE_SIDES_AS_RASTER:u32=0u;
const SCENE_SIDES_BOTH:u32=1u;
fn scene_finite(v:f32)->bool {
 return abs(v)<=3.402823466e+38;
}
fn scene_finite3(v:vec3<f32>)->bool {
 return all(abs(v)<=vec3(3.402823466e+38));
}

// Slab division can overflow for a finite, nearly parallel direction. Saturate
// that endpoint before min/max, preserving overlap with finite nonnegative t.
fn scene_slab_endpoint(difference:f32,direction:f32)->f32 {
 let t=difference/direction;
 if scene_finite(t) {
  return t;
 }
 return select(-3.402823466e+38,3.402823466e+38,(difference<0.)==(direction<0.));
}

fn scene_portable_bounds(node:u32,origin:vec3<f32>,direction:vec3<f32>,minimum:f32,maximum:f32)->bool {
 let lo=scene_v3(node+SCENE_BVH_NODE_MIN);
 let hi=scene_v3(node+SCENE_BVH_NODE_MAX);
 var near=minimum;
 var far=maximum;
 for(var axis=0u;axis<3u;axis++) {
  if direction[axis]==0. {
   if origin[axis]<lo[axis] || origin[axis]>hi[axis] {
    return false;
   }
  } else {
   let a=scene_slab_endpoint(lo[axis]-origin[axis],direction[axis]);
   let b=scene_slab_endpoint(hi[axis]-origin[axis],direction[axis]);
   let low=min(a,b);
   let high=max(a,b);
   near=max(near,low);
   far=min(far,high);
   if near>far {
    return false;
   }
  }
 }
 return true;
}

// One primitive acceptance predicate for every traversal and, with the
// hardware path, its candidate loop: the receiver's own triangle, visibility
// group, alpha mode, interval and its open end, side under the ray's `sides`
// (SCENE_SIDES_*) and cut-out texels. Keep geometric arithmetic, intervals
// and materials identical.
fn scene_intersect_primitive(ray:SceneRay,index:u32,mesh_id:u32,primitive_id:u32,origin:vec3<f32>,direction:vec3<f32>,maximum:f32,receiver:vec2<u32>,sides:u32,open_end:bool)->RawSceneHit {
 let miss=RawSceneHit(vec4(0u),vec4(0.));
 let instance=scene_instances[index];
 let mesh=instance.mesh_word+mesh_id*SCENE_MESH_WORDS;
 // A surface-origin straight ray cannot re-intersect its own triangle.
 // The native caller supplies authoritative raster identity, not a distance
 // epsilon. Every distinct triangle and instance keeps its usual interval.
 if receiver.x!=0u && receiver.x==index+1u && receiver.y==scene_source[mesh+SCENE_MESH_INDICES]+primitive_id*3u {
  return miss;
 }
 let material=scene_material(scene_source[mesh+SCENE_MESH_MATERIAL_WORD]);
 let group=material.values.visibility_group;
 if (group & scene_source[SCENE_HEADER_VISIBILITY_MASK]) != group {
  return miss;
 }
 // Rays pass through blended surfaces, which write no depth and cast no
 // shadow.
 if (material.values.flags&MATERIAL_ALPHA_BLEND)!=0u {
  return miss;
 }
 let vertices=scene_vertex_words(mesh,primitive_id);
 let a=scene_v3(vertices.x+SCENE_VERTEX_POSITION);
 let e1=scene_v3(vertices.y+SCENE_VERTEX_POSITION)-a;
 let e2=scene_v3(vertices.z+SCENE_VERTEX_POSITION)-a;
 let p=cross(direction,e2);
 let determinant=dot(e1,p);
 // No grazing-angle epsilon: a small nonzero determinant is a valid solve.
 if !scene_finite(determinant) || determinant==0. {
  return miss;
 }
 let offset=origin-a;
 let q=cross(offset,e1);
 let u=dot(offset,p)/determinant;
 let v=dot(direction,q)/determinant;
 let t=dot(e2,q)/determinant;
 if !scene_finite3(vec3(t,u,v)) {
  return miss;
 }
 if u<0. || v<0. || u+v>1. || t<ray.origin.w || t>maximum {
  return miss;
 }
 // An open interval excludes its end: visibility to a hit at that distance.
 if open_end && t>=ray.direction.w {
  return miss;
 }
 // Object-space winding preserves authored sides even under mirrored poses;
 // a rejected candidate never narrows the interval.
 let front=determinant>0.;
 if !front && sides==SCENE_SIDES_AS_RASTER && (material.values.flags&MATERIAL_DOUBLE_SIDED)==0u {
  return miss;
 }
 // A masked material's cut-out texels are no surface to any traversal,
 // nearest, any-hit or visibility, as raster discards them; the any-hit
 // test samples the base alpha as a hit's shading does (scene_base_color).
 if (material.values.flags&MATERIAL_ALPHA_MASK)!=0u {
  let b=vec3(1.-u-v,u,v);
  let uv=scene_interpolated_uv(vertices,b);
  let color=scene_interpolated_color(vertices,b);
  if material_cut_out(material.values,scene_base_color(material,uv,color).a) {
   return miss;
  }
 }
 return RawSceneHit(vec4(1u,index,mesh_id,primitive_id),vec4(t,u,v,0.));
}

// Instance `index`'s nearest accepted hit closer than `maximum`, or with
// `any_hit` its first: its model's BVH walked with the ray in model space.
// A leaf names only instances whose model has triangles.
fn scene_trace_model(index:u32,ray:SceneRay,any_hit:bool,receiver:vec2<u32>,sides:u32,open_end:bool,maximum_before:f32,visits:ptr<function,u32>)->RawSceneHit {
 var closest=RawSceneHit(vec4(0u),vec4(0.));
 let instance=scene_instances[index];
 // Do not normalize the transformed direction: retaining its magnitude
 // preserves WORLD t under arbitrary scale.
 let origin=(instance.inverse_world*vec4(ray.origin.xyz,1.)).xyz;
 let direction=(instance.inverse_world*vec4(ray.direction.xyz,0.)).xyz;
 if !scene_finite3(origin) || !scene_finite3(direction) {
  return closest;
 }
 var maximum=maximum_before;
 let root=instance.bvh_root;
 var node=root;
 let end=scene_bvh_end(root);
 while node<end {
  if !scene_bvh_visit(visits) {
   break;
  }
  let escape=scene_source[node+SCENE_BVH_NODE_ESCAPE];
  if !scene_bvh_forward(node,escape) {
   *visits=SCENE_BVH_MOST_VISITS+1u;
   break;
  }
  if !scene_portable_bounds(node,origin,direction,ray.origin.w,maximum) {
   node=escape;
   continue;
  }
  if scene_source[node+SCENE_BVH_NODE_COUNT]==0u {
   node+=SCENE_BVH_NODE_WORDS;
   continue;
  }
  let leaf_records=scene_bvh_leaf(node,SCENE_BVH_PRIMITIVE_WORDS);
  for(var primitive=0u;primitive<leaf_records.y;primitive++) {
   let leaf=leaf_records.x+primitive*SCENE_BVH_PRIMITIVE_WORDS;
   let mesh_id=scene_source[leaf+SCENE_BVH_PRIMITIVE_MESH];
   let primitive_id=scene_source[leaf+SCENE_BVH_PRIMITIVE_TRIANGLE];
   let hit=scene_intersect_primitive(ray,index,mesh_id,primitive_id,origin,direction,maximum,receiver,sides,open_end);
   if hit.intersection.x==0u {
    continue;
   }
   maximum=hit.coords.x;
   closest=hit;
   if any_hit {
    return closest;
   }
  }
  node=escape;
 }
 return closest;
}

// The instance BVH at `root` (a header root, zero when it bounds nothing)
// walked with the world-space ray after `nearest`: the nearest accepted hit
// closer than it, or with `any_hit` the first; else `nearest`.
fn scene_trace_instances(root:u32,ray:SceneRay,any_hit:bool,receiver:vec2<u32>,sides:u32,open_end:bool,nearest:RawSceneHit,visits:ptr<function,u32>)->RawSceneHit {
 var closest=nearest;
 if root==0u {
  return closest;
 }
 var maximum=select(ray.direction.w,nearest.coords.x,nearest.intersection.x!=0u);
 var node=root;
 let end=scene_bvh_end(root);
 while node<end {
  if !scene_bvh_visit(visits) {
   break;
  }
  let escape=scene_source[node+SCENE_BVH_NODE_ESCAPE];
  if !scene_bvh_forward(node,escape) {
   *visits=SCENE_BVH_MOST_VISITS+1u;
   break;
  }
  if !scene_portable_bounds(node,ray.origin.xyz,ray.direction.xyz,ray.origin.w,maximum) {
   node=escape;
   continue;
  }
  if scene_source[node+SCENE_BVH_NODE_COUNT]==0u {
   node+=SCENE_BVH_NODE_WORDS;
   continue;
  }
  let leaf_records=scene_bvh_leaf(node,SCENE_BVH_INSTANCE_WORDS);
  for(var leaf=0u;leaf<leaf_records.y;leaf++) {
   let index=scene_source[leaf_records.x+leaf*SCENE_BVH_INSTANCE_WORDS];
   let hit=scene_trace_model(index,ray,any_hit,receiver,sides,open_end,maximum,visits);
   if hit.intersection.x==0u {
    continue;
   }
   maximum=hit.coords.x;
   closest=hit;
   if any_hit {
    return closest;
   }
  }
  node=escape;
 }
 return closest;
}

// A finite ray with a nonempty direction over a nonnegative interval.
fn scene_ray_valid(ray:SceneRay)->bool {
 return scene_finite3(ray.origin.xyz) && scene_finite3(ray.direction.xyz) &&
    scene_finite(ray.origin.w) && scene_finite(ray.direction.w) &&
    ray.origin.w>=0. && ray.direction.w>=ray.origin.w && any(ray.direction.xyz!=vec3(0.));
}

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
