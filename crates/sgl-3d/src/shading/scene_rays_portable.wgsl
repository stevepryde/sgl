// Application-owned finite-interval geometry queries. PBRT4 EqualCounts BVH;
// Moller-Trumbore triangle solve. `scene::rays::bvh` builds the tree.
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

// One primitive acceptance predicate for every traversal. Keep geometric
// arithmetic, intervals and materials identical.
fn scene_intersect_primitive(ray:SceneRay,slot:u32,mesh_id:u32,primitive_id:u32,origin:vec3<f32>,direction:vec3<f32>,maximum:f32)->RawSceneHit {
 let miss=RawSceneHit(vec4(0u),vec4(0.));
 let instance=scene_instances[slot];
 let mesh=instance.mesh_word+mesh_id*SCENE_MESH_WORDS;
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
 // Object-space winding preserves authored sides even under mirrored poses;
 // a rejected candidate never narrows the interval.
 let front=determinant>0.;
 if !front && (material.values.flags&MATERIAL_DOUBLE_SIDED)==0u {
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
 return RawSceneHit(vec4(1u,slot,mesh_id,primitive_id),vec4(t,u,v,0.));
}

// instance_kind: 0 = all, 1 = static, 2 = moving. Filtering happens before BVH traversal.
fn scene_trace_portable_receiver(ray:SceneRay,any_hit:bool,receiver:vec2<u32>,instance_kind:u32,exclusive_end:bool)->RawSceneHit {
 var closest=RawSceneHit(vec4(0u),vec4(0.));
 if !scene_finite3(ray.origin.xyz) || !scene_finite3(ray.direction.xyz) ||
    !scene_finite(ray.origin.w) || !scene_finite(ray.direction.w) ||
    ray.origin.w<0. || ray.direction.w<ray.origin.w || all(ray.direction.xyz==vec3(0.)) {
     return closest;
    }
 var maximum=ray.direction.w;
 for(var slot=0u;slot<scene_source[SCENE_HEADER_INSTANCE_COUNT];slot++) {
  let instance=scene_instances[slot];
  let root=instance.bvh_root;
  if root==0u {
   continue;
  }
  let moving=(instance.flags&OBJECT_STATIC)==0u;
  if instance_kind==1u && moving {
   continue;
  }
  if instance_kind==2u && !moving {
   continue;
  }
  // normal_matrix = transpose(inverse(world)). Do not normalize the transformed
  // direction: retaining its magnitude preserves WORLD t under arbitrary scale.
  let inverse_world=transpose(instance.normal_matrix);
  let origin=(inverse_world*vec4(ray.origin.xyz,1.)).xyz;
  let direction=(inverse_world*vec4(ray.direction.xyz,0.)).xyz;
  if !scene_finite3(origin) || !scene_finite3(direction) {
   continue;
  }
  var node=root;
  let end=scene_source[root+SCENE_BVH_NODE_ESCAPE];
  while node<end {
   let escape=scene_source[node+SCENE_BVH_NODE_ESCAPE];
   if !scene_portable_bounds(node,origin,direction,ray.origin.w,maximum) {
    node=escape;
    continue;
   }
   let count=scene_source[node+SCENE_BVH_NODE_COUNT];
   if count==0u {
    node+=SCENE_BVH_NODE_WORDS;
    continue;
   }
   let first=scene_source[node+SCENE_BVH_NODE_FIRST];
   for(var primitive=0u;primitive<count;primitive++) {
    let leaf=first+primitive*SCENE_BVH_PRIMITIVE_WORDS;
    let mesh_id=scene_source[leaf+SCENE_BVH_PRIMITIVE_MESH];
    let primitive_id=scene_source[leaf+SCENE_BVH_PRIMITIVE_TRIANGLE];
    // A surface-origin straight ray cannot re-intersect its own triangle.
    // The native caller supplies authoritative raster identity, not a distance
    // epsilon. Every distinct triangle and instance keeps its usual interval.
    if receiver.x!=0u && receiver.x==instance.id+1u {
     let mesh=instance.mesh_word+mesh_id*SCENE_MESH_WORDS;
     if receiver.y==scene_source[mesh+SCENE_MESH_INDICES]+primitive_id*3u {
      continue;
     }
    }
    let hit=scene_intersect_primitive(ray,slot,mesh_id,primitive_id,origin,direction,maximum);
    if hit.intersection.x==0u {
     continue;
    }
    if exclusive_end && hit.coords.x>=ray.direction.w {
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
 }
 return closest;
}

fn scene_trace_portable(ray:SceneRay,any_hit:bool)->RawSceneHit {
 return scene_trace_portable_receiver(ray,any_hit,vec2(0u),0u,false);
}

fn scene_trace_moving_except_receiver(ray:SceneRay,receiver:vec2<u32>)->RawSceneHit {
 return scene_trace_portable_receiver(ray,false,receiver,2u,false);
}

// Visibility to a moving hit needs only a static any-hit in [t_min,t_hit).
// Like Wicked's TraceRay_Any (raytracingHF.hlsli, revision
// 2ff1d9e7b36091d6edf9f823af77e6bc9af20e3b, MIT), stop on the first blocker.
fn scene_static_segment_visible_except_receiver(ray:SceneRay,receiver:vec2<u32>)->bool {
 return scene_trace_portable_receiver(ray,true,receiver,1u,true).intersection.x==0u;
}

fn scene_trace_nearest(ray:SceneRay)->RawSceneHit {
 return scene_trace_portable(ray,false);
}

// Closed [t_min,t_max] visibility interval. Direction may be non-unit. Callers
// choose geometric ray-origin offsets and emitter endpoints; this adds no bias.
fn scene_segment_visible(origin:vec3<f32>,direction:vec3<f32>,t_min:f32,t_max:f32)->bool {
 return scene_trace_portable(SceneRay(vec4(origin,t_min),vec4(direction,t_max)),true).intersection.x==0u;
}
