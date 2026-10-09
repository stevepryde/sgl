// The inputs of a program that composes a game's module, beside it. Group
// 2's parameter blocks of a material with a game's shader: this frame's and
// the last submitted frame's (Scene::set_shader_parameters), visible to the
// vertex and fragment stages. Rust layout: shading::bind::group2.
@group(2) @binding(20) var<uniform> shader_params:ShaderParams;
@group(2) @binding(21) var<uniform> previous_shader_params:ShaderParams;
// Group 1's instance shader data (Scene::set_instance_shader_data), beside
// the object records and not in them, so that a program without a game's
// shader reads records of the size it always did: each instance's this
// frame at twice its record's index and the last submitted frame's after
// it. Visible to the vertex stage alone, which passes a fragment its
// instance's (Fragment.instance), so the fragment stage's storage buffers
// stay at S3D-1's floor. Rust layout: shading::bind::scene.
@group(1) @binding(3) var<storage,read> object_shader_data:array<vec4<f32>>;
// The parameters an evaluation at the last submitted frame (`previous`) or
// this frame takes.
fn material_shader_params(previous:bool)->ShaderParams {
 if previous {
  return previous_shader_params;
 }
 return shader_params;
}
// The shader data of the instance whose object record is at index `object`,
// at the last submitted frame (`previous`) or this frame. Vertex stage only.
fn material_instance(object:u32,previous:bool)->vec4<f32> {
 return object_shader_data[2u*object+select(0u,1u,previous)];
}
