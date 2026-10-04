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
 let vertex=scene_source[mesh+SCENE_MESH_VERTICES]+vertex_index*SCENE_VERTEX_WORDS;
 let position=scene_v3(vertex+SCENE_VERTEX_POSITION);
 var pulled=PulledSceneVertex(position,position,scene_v3(vertex+SCENE_VERTEX_NORMAL),scene_v2(vertex+SCENE_VERTEX_UV),scene_v4(vertex+SCENE_VERTEX_COLOR),
  vec2(object+1u,index_word+(index/3u)*3u),scene_v2(vertex+SCENE_VERTEX_LIGHTMAP_UV),scene_v4(vertex+SCENE_VERTEX_LIGHTMAP_BOUNDS),scene_v4(vertex+SCENE_VERTEX_TANGENT));
 if deformed_vertices {
  let deformed=scene_source[mesh+SCENE_MESH_FIRST_VERTEX]+vertex_index;
  pulled.position=scene_v3(objects[object].deformed_positions+deformed*DEFORMED_POSITION_WORDS);
  pulled.previous_position=scene_v3(objects[object].previous_positions+deformed*DEFORMED_POSITION_WORDS);
  let tangent_frame=objects[object].deformed_normals+deformed*DEFORMED_NORMAL_WORDS;
  pulled.normal=scene_v3(tangent_frame);
  pulled.tangent=scene_v4(tangent_frame+DEFORMED_TANGENT);
 }
 return pulled;
}
