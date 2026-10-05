// The portable walk: finite-interval geometry queries through the scene's
// two-level ray source, the instance BVH of each kind, static and moving,
// over the instances' posed model bounds, whose leaves name instance entries,
// then each instance's model BVH with the ray in that model's space, as DXR
// and Vulkan traverse a TLAS and its BLASes (Wald et al. 2003). Binned SAH
// model BVHs and EqualCounts instance BVHs; Moller-Trumbore triangle solve,
// whose candidates the shared predicate decides (scene_rays_predicate.wgsl).
// `scene::rays::bvh` builds every tree, and `scene::rays::instances` the
// instance BVHs, over the instances the hardware path's TLAS does not hold
// on a frame it traces.
// The portable function set (scene_rays_portable.wgsl) and the hardware
// module (scene_rays_hardware.wgsl) both compose it.
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
// A ray's kinds of instance, as bits: the instance BVHs the walk takes
// and, on the hardware path, the TLAS instance masks its cull mask selects
// (`scene::rays::acceleration`, `MASK_STATIC` and `MASK_MOVING`).
const SCENE_KIND_STATIC:u32=1u;
const SCENE_KIND_MOVING:u32=2u;
const SCENE_KINDS_ALL:u32=SCENE_KIND_STATIC|SCENE_KIND_MOVING;
// The most BVH nodes one ray visits across its instance walk and the model
// walks it starts (AR-12), as AMD's FidelityFX SSSR caps a ray's hierarchy
// lookups at `max_traversal_intersections` (FidelityFX SDK
// samples/hybridreflections/shaders/ffx_sssr_intersect.h, revision
// c6efa6bf7f2027b3ec94f28578bb5965eabb9e55). On the CPU, the most a ray
// visited in this walk was 2,081 over a 2-million-triangle terrain at grazing
// angles, 1,173 inside half a million foliage triangles and 19,238 across a
// forest of 40,000 instances of a 5,000-triangle tree at 0 to 3 degrees
// (#160), with median-split model BVHs. Rebuilt with surface area model BVHs
// (#187), a forest of that size took at most 24,977 where its median-split
// one took 29,027, and a terrain 1,799 where it took 2,153; the cap is over
// twice the forest's. A ray that reaches it reports a miss.
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

// Instance `index`'s triangle `primitive_id` of its mesh `mesh_id` met by the
// ray in its model's space (`origin`, `direction`) closer than `maximum`:
// the Moller-Trumbore solve, whose candidate the shared predicate decides.
// Keep geometric arithmetic, intervals and materials identical.
fn scene_intersect_primitive(ray:SceneRay,index:u32,mesh_id:u32,primitive_id:u32,origin:vec3<f32>,direction:vec3<f32>,maximum:f32,receiver:vec2<u32>,sides:u32,open_end:bool)->RawSceneHit {
 let miss=RawSceneHit(vec4(0u),vec4(0.));
 let mesh=scene_instances[index].mesh_word+mesh_id*SCENE_MESH_WORDS;
 if !scene_accepts_triangle(index,mesh,primitive_id,receiver) {
  return miss;
 }
 let vertices=scene_vertex_words(mesh,primitive_id);
 let a=scene_vertex_position(vertices.x);
 let e1=scene_vertex_position(vertices.y)-a;
 let e2=scene_vertex_position(vertices.z)-a;
 let p=cross(direction,e2);
 let determinant=scene_winding(e1,e2,direction);
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
 if u<0. || v<0. || u+v>1. {
  return miss;
 }
 let candidate=SceneCandidate(index,mesh_id,primitive_id,t,vec2(u,v),determinant);
 if !scene_accepts_solved(ray,candidate,mesh,maximum,sides,open_end) {
  return miss;
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

// The walk of a valid ray over the instance BVHs of `kinds`
// (SCENE_KIND_*), the static one first, then the moving one within its
// nearest hit: the nearest accepted hit nearer than `nearest`, or with
// `any_hit` the first, leaving `receiver`, accepting `sides`, its
// interval's end excluded when `open_end`; else `nearest`. The two walks
// share one visit budget, and a ray that exhausts it reports a miss.
fn scene_walk(ray:SceneRay,kinds:u32,any_hit:bool,receiver:vec2<u32>,sides:u32,open_end:bool,nearest:RawSceneHit)->RawSceneHit {
 let miss=RawSceneHit(vec4(0u),vec4(0.));
 var hit=nearest;
 var visits=0u;
 if (kinds&SCENE_KIND_STATIC)!=0u {
  hit=scene_trace_instances(scene_source[SCENE_HEADER_STATIC_ROOT],ray,any_hit,receiver,sides,open_end,hit,&visits);
  if scene_bvh_exhausted(visits) {
   return miss;
  }
  if any_hit && hit.intersection.x!=0u {
   return hit;
  }
 }
 if (kinds&SCENE_KIND_MOVING)!=0u {
  hit=scene_trace_instances(scene_source[SCENE_HEADER_MOVING_ROOT],ray,any_hit,receiver,sides,open_end,hit,&visits);
  if scene_bvh_exhausted(visits) {
   return miss;
  }
 }
 return hit;
}
