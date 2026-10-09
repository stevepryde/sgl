// The parameters of a program that composes no game shader
// (shader_default.wgsl): a zero block no binding holds, so its pipelines
// read neither parameter buffer. A program composes it or
// shader_params_bound.wgsl, exactly one.
// The parameters an evaluation at the last submitted frame (`previous`) or
// this frame takes.
fn material_shader_params(previous:bool)->ShaderParams {
 return ShaderParams(vec4(0.));
}
