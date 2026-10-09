// Nonindexed vertex pulling preserves the authored indexed triangle stream.
// The flat key is written by the same raster invocation as source radiance.
// A deforming instance's position, normal and tangent are its deformed ones
// (its object record's deformed_positions), and its position in the last
// submitted frame is the one motion is measured from: pipelines that draw deforming
// instances (view::pipelines::Variant) read them, a pipeline constant, so
// those of rigid ones do not branch on it.
override deformed_vertices:bool=false;
struct PulledSceneVertex {
 position:vec3<f32>, previous_position:vec3<f32>, normal:vec3<f32>, uv:vec2<f32>, color:vec4<f32>, source_id:vec2<u32>, lightmap_uv:vec2<f32>, lightmap_bounds:vec4<f32>, tangent:vec4<f32>, shader_data:vec4<f32>,
}
// A GPU-built draw's vertex past its instance's triangles: a fixed finite
// point outside the clip volume on every side (x, y, and depth past w),
// where Bevy 9d12036's meshlet raster emits a NaN
// (crates/bevy_pbr/src/meshlet/visibility_buffer_hardware_raster.wesl
// 31-54, 69-83), which WGSL leaves indeterminate and Metal's fast math may
// fold. A triangle past the count has all three vertices there, so it has
// no area either.
const SCENE_DUMMY_CLIP:vec4<f32>=vec4(2.,2.,2.,1.);
// Whether draw vertex `vertex` of `drawn` lies past its triangles.
fn drawn_dummy(drawn:DrawInstance,vertex:u32)->bool {
 return vertex>=drawn.triangles*3u;
}
// The index, relative to its mesh's indices, of draw vertex `vertex` of
// `drawn`: a CPU-built draw draws a mesh's own index range from first
// index zero, a GPU-built one a section's from its first index.
fn drawn_index(drawn:DrawInstance,vertex:u32)->u32 {
 return drawn.first_index+vertex;
}
// The vertex that index `index`, relative to its mesh's indices, of the mesh
// whose record is at `mesh` names.
fn scene_pulled_vertex(mesh:u32,index:u32)->u32 {
 return scene_source[scene_source[mesh+SCENE_MESH_INDICES]+index];
}
// Vertex `vertex_index`'s position of the mesh whose record is at `mesh`,
// as the instance whose object record is at index `object` shows it: its
// deformed position this frame where it deforms.
fn scene_pulled_position(object:u32,mesh:u32,vertex_index:u32)->vec3<f32> {
 if deformed_vertices {
  return scene_deformed_position(objects[object].deformed_positions,mesh,vertex_index);
 }
 return scene_vertex_position(scene_vertex_word(mesh,vertex_index));
}
// Vertex `index` of the drawn mesh whose record is at `mesh`, as the instance
// whose object record is at index `object` shows it. Its source identity is
// that index plus one, and its primitive's first index word.
fn scene_source_vertex(object:u32,mesh:u32,index:u32)->PulledSceneVertex {
 let vertex_index=scene_pulled_vertex(mesh,index);
 let vertex=scene_vertex_word(mesh,vertex_index);
 let position=scene_pulled_position(object,mesh,vertex_index);
 let primitive=scene_source[mesh+SCENE_MESH_INDICES]+(index/3u)*3u;
 var pulled=PulledSceneVertex(position,position,vec3(0.),scene_vertex_uv(vertex,scene_mesh_uv_rect(mesh)),scene_vertex_color(vertex),
  vec2(object+1u,primitive),scene_vertex_lightmap_uv(vertex),scene_vertex_lightmap_bounds(mesh,vertex),vec4(0.),scene_vertex_shader_data(mesh,vertex_index));
 // A deforming instance's frame is its deformed one, so its rest frame is
 // decoded only for rigid ones.
 var frame:PackedFrame;
 if deformed_vertices {
  pulled.previous_position=scene_deformed_position(objects[object].previous_positions,mesh,vertex_index);
  frame=scene_deformed_frame(objects[object].deformed_normals,mesh,vertex_index);
 } else {
  frame=scene_vertex_frame(vertex);
 }
 pulled.normal=frame.normal;
 pulled.tangent=frame.tangent;
 return pulled;
}
