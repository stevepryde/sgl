// Nonindexed vertex pulling preserves the authored indexed triangle stream.
// The flat key is written by the same raster invocation as source radiance.
// A deforming instance's position, normal and tangent are its deformed ones
// (its object record's deformed_positions), and its position in the last
// submitted frame is the one motion is measured from: pipelines that draw deforming
// instances (view::pipelines::Variant) read them, a pipeline constant, so
// those of rigid ones do not branch on it.
override deformed_vertices:bool=false;
struct PulledSceneVertex {
 position:vec3<f32>, previous_position:vec3<f32>, normal:vec3<f32>, uv:vec2<f32>, color:vec4<f32>, source_id:vec2<u32>, lightmap_uv:vec2<f32>, lightmap_bounds:vec4<f32>, tangent:vec4<f32>,
}
// Vertex `index` of the drawn mesh whose record is at `mesh`, as the instance
// whose object record is at index `object` shows it. Its source identity is
// that index plus one.
fn scene_source_vertex(object:u32,mesh:u32,index:u32)->PulledSceneVertex {
 let index_word=scene_source[mesh+SCENE_MESH_INDICES];
 let vertex_index=scene_source[index_word+index];
 let vertex=scene_vertex_word(mesh,vertex_index);
 let position=scene_vertex_position(vertex);
 var pulled=PulledSceneVertex(position,position,vec3(0.),scene_vertex_uv(vertex,scene_mesh_uv_rect(mesh)),scene_vertex_color(vertex),
  vec2(object+1u,index_word+(index/3u)*3u),scene_vertex_lightmap_uv(vertex),scene_vertex_lightmap_bounds(mesh,vertex),vec4(0.));
 // A deforming instance's frame is its deformed one, so its rest frame is
 // decoded only for rigid ones.
 if !deformed_vertices {
  let frame=scene_vertex_frame(vertex);
  pulled.normal=frame.normal;
  pulled.tangent=frame.tangent;
 } else {
  pulled.position=scene_deformed_position(objects[object].deformed_positions,mesh,vertex_index);
  pulled.previous_position=scene_deformed_position(objects[object].previous_positions,mesh,vertex_index);
  let frame=scene_deformed_frame(objects[object].deformed_normals,mesh,vertex_index);
  pulled.normal=frame.normal;
  pulled.tangent=frame.tangent;
 }
 return pulled;
}
