// The shaders example's tinted glass (examples/shaders.rs): a game's shader
// (Scene::add_shader) giving a transmissive pane a thickness that varies
// across it, which SGL3D does not ship.
//
// Each vertex's shader data's x is the glass's thickness there in the
// mesh's units, which the vertex function passes to the surface function
// interpolated; the surface function makes it the volume's thickness and
// the parameters' tint its Beer-Lambert attenuation, so the tint deepens
// where the glass is thick. It needs no scene depth, so it renders alike
// on every binding tier, but for where the transmission copy is held.

struct ShaderParams {
 // The Beer-Lambert coefficient per metre on each channel.
 attenuation:vec4<f32>,
}

fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 var out=v;
 out.custom=vec4(v.shader_data.x,0.,0.,0.);
 return out;
}

fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 var out=s;
 out.thickness=ctx.custom.x;
 out.attenuation=params.attenuation.rgb;
 return out;
}
