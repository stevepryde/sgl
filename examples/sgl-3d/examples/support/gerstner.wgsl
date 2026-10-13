// The water example's waves (examples/water.rs): a game's shader
// (Scene::add_shader), which SGL3D composes into its programs. SGL3D ships
// no water: these equations are the example's.
//
// Six Gerstner waves (Tessendorf, "Simulating Ocean Water", SIGGRAPH 2001
// course notes, section 3.1; Finch, "Effective Water Simulation from
// Physical Models", GPU Gems, 2004, equations 9 and 12). Each wave's vector
// is a whole multiple of 2π / L on each axis (the example lays them so), so
// the sum repeats every L metres along x and z, and a chunk's instance data
// holds its origin modulo L: a vertex's anchor, its chunk-local position
// plus that, is continuous across chunks, needs no render-frame position
// and stays where it is when the render origin moves. Each wave moves a
// whole number of its wavelengths per hour, at the frame's phase within the
// hour (VertexContext.phase), so its motion is exact however long a session
// runs.

struct ShaderParams {
 // Each wave's unit direction along x and z, its wavelength in metres and
 // the whole wavelengths it travels per hour.
 waves:array<vec4<f32>,6>,
 // The steepness Q (horizontal pinch, 0..1), the amplitude per metre of
 // wavelength, and the column in metres beyond which the water is deep.
 shape:vec4<f32>,
 // The deep water's tint, and the column in metres over which the shallow
 // tint gives way to it.
 deep:vec4<f32>,
 // The shallow water's tint, and the column in metres over which the water
 // fades into the shore.
 shallow:vec4<f32>,
 // The water's Beer-Lambert coefficient per metre on each channel.
 absorption:vec4<f32>,
}

const WATER_WAVES:u32=6u;
const WATER_TAU:f32=6.28318530718;

// The displacement of the rest surface at `anchor` (x and z) at `phase`, and
// the displaced surface's unit normal there.
struct WaterSurface {
 offset:vec3<f32>,
 normal:vec3<f32>,
}
fn water_surface(anchor:vec2<f32>,phase:f32,params:ShaderParams)->WaterSurface {
 var offset=vec3(0.);
 var slope=vec2(0.);
 var lift=1.;
 for (var i=0u;i<WATER_WAVES;i++) {
  let wave=params.waves[i];
  let k=WATER_TAU/wave.z;
  let amplitude=params.shape.y*wave.z;
  let theta=k*dot(wave.xy,anchor)-WATER_TAU*wave.w*phase;
  let s=sin(theta);
  let c=cos(theta);
  offset+=vec3(params.shape.x*amplitude*wave.x*c,amplitude*s,params.shape.x*amplitude*wave.y*c);
  slope+=wave.xy*(k*amplitude*c);
  lift-=params.shape.x*k*amplitude*s;
 }
 return WaterSurface(offset,normalize(vec3(-slope.x,max(lift,0.25),-slope.y)));
}

// Each vertex's data (x) is how much of the waves it takes: 1 on open
// water, less toward the shore (the example's meshing policy).
fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 var out=v;
 let state=v.shader_data.x;
 let water=water_surface(v.position.xz+ctx.instance.xy,ctx.phase,params);
 out.position+=water.offset*state;
 out.normal=normalize(mix(v.normal,water.normal,state));
 out.custom=vec4(state,0.,0.,0.);
 return out;
}

// The light behind the water crosses its column: the opaque surface behind
// it along the view where the frame gives the scene depth, else the deep
// column. The column tints it from shallow to deep, fades the water into
// the shore where it is short, and is its volume's thickness, in the mesh's
// units; calmer water (less of the waves) is smoother.
fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 var out=s;
 let deep=params.shape.z;
 var column=deep;
 if scene_depth_available() {
  column=min(scene_depth_behind(ctx),deep);
 }
 let scale=(ctx.model_scale.x+ctx.model_scale.y+ctx.model_scale.z)/3.;
 out.thickness=column/scale;
 out.attenuation=params.absorption.rgb;
 let tint=mix(params.deep.rgb,params.shallow.rgb,exp(-column/max(params.deep.w,1e-3)));
 out.base_color=vec4(s.base_color.rgb*tint,s.base_color.a*smoothstep(0.,params.shallow.w,column));
 out.roughness=mix(s.roughness*0.5,s.roughness,ctx.custom.x);
 return out;
}
