// The shader provider of a program that composes no game shader
// (shader_contract.wgsl): its functions return their argument, so a
// material without a shader takes SGL3D's one vertex and surface path
// unchanged. A program composes it with shader_inputs_none.wgsl, or a
// game's module with shader_inputs_bound.wgsl.
struct ShaderParams {
 unused:vec4<f32>,
}
fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 return v;
}
fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 return s;
}
