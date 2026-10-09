// The inputs of a program that composes no game shader
// (shader_default.wgsl): a zero parameter block and zero instance data, which
// no binding holds, so its pipelines read neither parameter buffer nor the
// instances' shader data. A program composes it or shader_inputs_bound.wgsl,
// exactly one.
// The parameters an evaluation at the last submitted frame (`previous`) or
// this frame takes.
fn material_shader_params(previous:bool)->ShaderParams {
 return ShaderParams(vec4(0.));
}
// The shader data of the instance whose object record is at index `object`,
// at the last submitted frame (`previous`) or this frame.
fn material_instance(object:u32,previous:bool)->vec4<f32> {
 return vec4(0.);
}
