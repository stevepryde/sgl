// The deform stage: morphs and skins one mesh of one deforming instance per
// dispatch, one vertex per invocation, from its model's vertex records into
// the instance's deformed positions, normals and tangents in the scene
// source, which every geometry pass then reads.
//
// Ported from Bevy 9d12036, MIT OR Apache-2.0 (src/LICENSE-bevy.txt):
// morph_vertex (crates/bevy_pbr/src/render/mesh.wesl, with morph.wesl's
// targets), and skin_model, inverse_transpose_3x3m and skin_normals
// (crates/bevy_pbr/src/render/skinning.wesl) with mesh_tangent_local_to_world
// (mesh_functions.wesl). Changed: run once per frame in compute rather than
// in every pass's vertex shader, as Wicked Engine 4323a33's skinningCS.hlsl
// (MIT, src/LICENSE-wicked.txt) runs them; the result stays in the model's
// space, which the instance's pose then places; a tangent keeps its
// handedness, which the pose's mirroring flips later.
@group(0) @binding(0) var<storage,read_write> source:array<u32>;
@group(0) @binding(1) var<uniform> dispatch:DeformDispatch;

fn source_f32(at:u32)->f32 {
 return bitcast<f32>(source[at]);
}
fn source_v3(at:u32)->vec3<f32> {
 return vec3(source_f32(at),source_f32(at+1u),source_f32(at+2u));
}
fn source_v4(at:u32)->vec4<f32> {
 return vec4(source_v3(at),source_f32(at+3u));
}
fn store_v3(at:u32,value:vec3<f32>) {
 source[at]=bitcast<u32>(value.x);
 source[at+1u]=bitcast<u32>(value.y);
 source[at+2u]=bitcast<u32>(value.z);
}
fn store_v4(at:u32,value:vec4<f32>) {
 store_v3(at,value.xyz);
 source[at+3u]=bitcast<u32>(value.w);
}
fn joint_matrix(joint:u32)->mat4x4<f32> {
 let at=dispatch.joints+joint*JOINT_WORDS;
 return mat4x4(source_v4(at),source_v4(at+4u),source_v4(at+8u),source_v4(at+12u));
}
// Bevy's skin_model: the weighted sum of the vertex's joint matrices.
fn skin_model(vertex_index:u32)->mat4x4<f32> {
 let influence=dispatch.influences+vertex_index*INFLUENCE_WORDS;
 let joints=vec4(source[influence],source[influence+1u],source[influence+2u],source[influence+3u]);
 let weights=source_v4(influence+INFLUENCE_WEIGHTS);
 return weights.x*joint_matrix(joints.x)
  +weights.y*joint_matrix(joints.y)
  +weights.z*joint_matrix(joints.z)
  +weights.w*joint_matrix(joints.w);
}
fn inverse_transpose_3x3m(in:mat3x3<f32>)->mat3x3<f32> {
 let x=cross(in[1],in[2]);
 let y=cross(in[2],in[0]);
 let z=cross(in[0],in[1]);
 let det=dot(in[2],z);
 return mat3x3<f32>(x/det,y/det,z/det);
}
fn skin_normals(model:mat4x4<f32>,normal:vec3<f32>)->vec3<f32> {
 return normalize(inverse_transpose_3x3m(mat3x3<f32>(model[0].xyz,model[1].xyz,model[2].xyz))*normal);
}
@compute @workgroup_size(64) fn deform(@builtin(global_invocation_id) id:vec3<u32>) {
 let vertex_index=id.x;
 if vertex_index>=dispatch.vertex_count {
  return;
 }
 let vertex=dispatch.vertices+vertex_index*SCENE_VERTEX_WORDS;
 var position=source_v3(vertex+SCENE_VERTEX_POSITION);
 var normal=source_v3(vertex+SCENE_VERTEX_NORMAL);
 var tangent=source_v4(vertex+SCENE_VERTEX_TANGENT);
 // Bevy's morph_vertex: each target's displacement at its weight.
 let deltas=dispatch.morph_targets+dispatch.morph_target_count;
 // The targets' weight offsets lie within the source, so the loop ends
 // whatever the count says.
 let length=arrayLength(&source);
 let targets=min(dispatch.morph_target_count,select(0u,length-dispatch.morph_targets,dispatch.morph_targets<length));
 for (var morph=0u; morph<targets; morph++) {
  let weight=source_f32(dispatch.weights+source[dispatch.morph_targets+morph]);
  if weight==0. {
   continue;
  }
  let delta=deltas+(morph*dispatch.vertex_count+vertex_index)*MORPH_DELTA_WORDS;
  position+=weight*source_v3(delta);
  normal+=weight*source_v3(delta+MORPH_DELTA_NORMAL);
  tangent+=vec4(weight*source_v3(delta+MORPH_DELTA_TANGENT),0.);
 }
 if dispatch.influences!=0u {
  let model=skin_model(vertex_index);
  position=(model*vec4(position,1.)).xyz;
  normal=skin_normals(model,normal);
  if any(tangent!=vec4(0.)) {
   tangent=vec4(normalize(mat3x3<f32>(model[0].xyz,model[1].xyz,model[2].xyz)*tangent.xyz),tangent.w);
  }
 }
 store_v3(dispatch.positions+vertex_index*DEFORMED_POSITION_WORDS,position);
 let tangent_frame=dispatch.normals+vertex_index*DEFORMED_NORMAL_WORDS;
 store_v3(tangent_frame,normal);
 store_v4(tangent_frame+DEFORMED_TANGENT,tangent);
}
