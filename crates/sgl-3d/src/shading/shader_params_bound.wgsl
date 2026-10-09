// Group 2's parameter blocks of a material with a game's shader, which a
// program that composes the game's module composes beside it: this frame's
// and the last submitted frame's (Scene::set_shader_parameters), visible to
// the vertex and fragment stages. Rust layout: shading::bind::group2.
@group(2) @binding(20) var<uniform> shader_params:ShaderParams;
@group(2) @binding(21) var<uniform> previous_shader_params:ShaderParams;
// The parameters an evaluation at the last submitted frame (`previous`) or
// this frame takes.
fn material_shader_params(previous:bool)->ShaderParams {
 if previous {
  return previous_shader_params;
 }
 return shader_params;
}
