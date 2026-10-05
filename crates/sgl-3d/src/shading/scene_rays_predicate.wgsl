// The one acceptance predicate of every scene ray (the architecture's Ray
// source), whoever found the candidate: the portable walk's triangle solve
// or the hardware's query. It decides a found candidate from the ray in its
// model's space: the receiver's own triangle, visibility group, alpha mode,
// the interval and its open end, side under the ray's side policy from the
// triangle's object-space winding, and cut-out texels. With it, what makes
// a ray valid and the side policies, which every trace module composes.
// Which sides of a triangle a ray accepts, beside the receiver it leaves:
// a camera-origin ray rejects a single-sided material's back faces, as
// raster culls them; a dynamic GI probe ray and its visibility ray accept
// both sides of every triangle, as Wicked Engine's DDGI trace culls none;
// a ray-traced shadow ray rejects a single-sided material's front faces,
// the shadow maps' rule in a ray's terms (a map draws a single-sided
// caster's front faces from the light, so a ray from the receiver meets
// that caster's back), as Wicked Engine 2ff1d9e's RT shadows cull them
// (screenspaceshadowCS.hlsl 227, RAY_FLAG_CULL_FRONT_FACING_TRIANGLES; MIT,
// src/LICENSE-wicked.txt). A ray leaving a lit face whose origin lies a
// rounding behind it meets that face from behind, which this policy
// accepts: the ray's start past the surface (Wicked's TMin) keeps it from
// its own receiver, as in Wicked.
// A double-sided material occludes from either side under every policy.
const SCENE_SIDES_AS_RASTER:u32=0u;
const SCENE_SIDES_BOTH:u32=1u;
const SCENE_SIDES_SHADOW:u32=2u;
fn scene_finite(v:f32)->bool {
 return abs(v)<=3.402823466e+38;
}
fn scene_finite3(v:vec3<f32>)->bool {
 return all(abs(v)<=vec3(3.402823466e+38));
}

// A finite ray with a nonempty direction over a nonnegative interval.
fn scene_ray_valid(ray:SceneRay)->bool {
 return scene_finite3(ray.origin.xyz) && scene_finite3(ray.direction.xyz) &&
    scene_finite(ray.origin.w) && scene_finite(ray.direction.w) &&
    ray.origin.w>=0. && ray.direction.w>=ray.origin.w && any(ray.direction.xyz!=vec3(0.));
}

// The winding of the triangle with edges `e1` and `e2` seen along
// `direction`, all in its model's space: positive where the ray meets its
// front, the counter-clockwise side raster draws as front. It is
// Moller-Trumbore's determinant, which the portable walk's solve divides
// by.
fn scene_winding(e1:vec3<f32>,e2:vec3<f32>,direction:vec3<f32>)->f32 {
 return dot(e1,cross(direction,e2));
}

// A triangle a ray found: instance `index`'s triangle `primitive_id` of its
// mesh `mesh_id`, at distance `t` and barycentrics (those of its second and
// third vertices), met on the side its `winding` gives (scene_winding).
struct SceneCandidate {
 index:u32,
 mesh_id:u32,
 primitive_id:u32,
 t:f32,
 barycentrics:vec2<f32>,
 winding:f32,
}

// The predicate's checks that need no triangle solve: whether a ray leaving
// `receiver` (its instance's index plus one and its triangle's first index
// word, zero for none) may stop at triangle `primitive_id` of the mesh whose
// record is at `mesh`, of instance `index`: not the receiver's own triangle
// (a surface-origin straight ray cannot re-intersect it; the caller
// supplies the raster identity, not a distance epsilon), in a visible
// group, and not blended (rays pass through blended surfaces, which write
// no depth and cast no shadow).
fn scene_accepts_triangle(index:u32,mesh:u32,primitive_id:u32,receiver:vec2<u32>)->bool {
 if receiver.x!=0u && receiver.x==index+1u && receiver.y==scene_source[mesh+SCENE_MESH_INDICES]+primitive_id*3u {
  return false;
 }
 let material=scene_source[mesh+SCENE_MESH_MATERIAL_WORD];
 let group=scene_source[material+SCENE_MATERIAL_VISIBILITY_GROUP];
 if (group&scene_source[SCENE_HEADER_VISIBILITY_MASK])!=group {
  return false;
 }
 return (scene_source[material+SCENE_MATERIAL_FLAGS]&MATERIAL_ALPHA_BLEND)==0u;
}

// The rest of the predicate, for `candidate` on the mesh whose record is at
// `mesh`, which scene_accepts_triangle accepted: within the ray's interval
// from its start up to `maximum` (the nearest hit so far, else its end),
// short of the end where the interval is open (`open_end`: visibility to a
// hit at that distance), on a side `sides` (SCENE_SIDES_*) accepts, and not
// a masked material's cut-out texel. A rejected candidate never narrows the
// interval.
fn scene_accepts_solved(ray:SceneRay,candidate:SceneCandidate,mesh:u32,maximum:f32,sides:u32,open_end:bool)->bool {
 if candidate.t<ray.origin.w || candidate.t>maximum {
  return false;
 }
 if open_end && candidate.t>=ray.direction.w {
  return false;
 }
 let material_word=scene_source[mesh+SCENE_MESH_MATERIAL_WORD];
 let flags=scene_source[material_word+SCENE_MATERIAL_FLAGS];
 // Object-space winding preserves authored sides even under mirrored poses.
 // A triangle met edge-on (zero winding) has neither side.
 if (flags&MATERIAL_DOUBLE_SIDED)==0u {
  if candidate.winding<=0. && sides==SCENE_SIDES_AS_RASTER {
   return false;
  }
  if candidate.winding>=0. && sides==SCENE_SIDES_SHADOW {
   return false;
  }
 }
 // A masked material's cut-out texels are no surface to any traversal,
 // nearest, any-hit or visibility, as raster discards them; the test
 // samples the base alpha as a hit's shading does (scene_base_color), at
 // the packed UV and colour, which deformation leaves.
 if (flags&MATERIAL_ALPHA_MASK)!=0u {
  let material=scene_material(material_word);
  let b=vec3(1.-candidate.barycentrics.x-candidate.barycentrics.y,candidate.barycentrics);
  let vertices=scene_vertex_words(mesh,candidate.primitive_id);
  let uv=scene_interpolated_uv(scene_mesh_uv_rect(mesh),vertices,b);
  let color=scene_interpolated_color(vertices,b);
  if material_cut_out(material.values,scene_base_color(material,uv,color).a) {
   return false;
  }
 }
 return true;
}
