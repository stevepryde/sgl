// The shaders example's wind (examples/shaders.rs): a game's shader
// (Scene::add_shader) bending vegetation, which SGL3D does not ship.
//
// Each vertex bends along the wind by its height weight (its shader data's
// y: 0 at the root, 1 at the tip) squared, so the root stays put and the
// tip sways most, as a cantilever does, at a gust whose phase each instance
// offsets by its own data's x, so neighbours do not move as one. The gusts
// repeat a whole number of times an hour (VertexContext.phase).

struct ShaderParams {
 // The wind's unit direction along x and z, the bend at a tip in metres,
 // and the gusts per hour.
 wind:vec4<f32>,
}

const WIND_TAU:f32=6.28318530718;

fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 var out=v;
 let height=v.shader_data.y;
 let gust=0.6+0.4*sin(WIND_TAU*(params.wind.w*ctx.phase+ctx.instance.x));
 let bend=params.wind.z*height*height*gust;
 out.position+=vec3(params.wind.x,0.,params.wind.y)*bend;
 return out;
}

fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 return s;
}
