// The volumes example's tinted glass (examples/volumes.rs): a game's shader
// (Scene::add_shader) that absorbs the light crossing a closed glass volume
// over the length its view ray travels inside it, measured by SGL3D's
// volume layers (scene_volume_path). SGL3D ships no glass: these equations
// are the example's.
//
// The material is double-sided, so both the faces where the view ray
// enters (ctx.front) and those where it leaves are drawn, and each one
// transmits the opaque frame behind it. The light is absorbed once, at the
// face the eye sees first:
// - an entry face absorbs over the path to the volume's exit (VOLUME_EXIT),
//   or to the opaque surface where that lies inside the volume
//   (VOLUME_OPAQUE);
// - an exit face seen from inside (VOLUME_EYE: the camera is in the glass)
//   absorbs over the path from the eye;
// - an exit face behind an entry (VOLUME_ENTRY) covers nothing: the entry
//   in front carries the whole path, and the exit, drawn after it, would
//   show the background unabsorbed over it;
// - without a measured path (VOLUME_NONE: the setting off or the Basic
//   tier; VOLUME_HIDDEN: an entry face behind two exits, or an exit face
//   behind another volume's exit, which hides where its segment starts)
//   either face keeps its coverage and takes the authored thickness, as a
//   material without a shader does.
// Coverage is never faded by the path's length: a finite volume's entry and
// exit meet at its silhouette, where the path falls to 0.
//
// A blob's vertices wobble with the time: each moves along its normal by a
// travelling wave over its rest position, its normal tilting with the
// wave's slope. Boxes set no wobble and keep their rest geometry.

struct ShaderParams {
 // The Beer-Lambert coefficient per metre on each channel, and in w the
 // thickness in the mesh's units taken where no path is measured.
 absorption:vec4<f32>,
 // The wobble's amplitude in the mesh's units (0 for none), its spatial
 // frequency in radians per mesh unit, and its whole cycles per hour.
 wobble:vec4<f32>,
}

const GLASS_TAU:f32=6.28318530718;

// The wobble's offset along the rest normal at rest position `p`, and its
// gradient there.
struct GlassWobble {
 offset:f32,
 gradient:vec3<f32>,
}
fn glass_wobble(p:vec3<f32>,phase:f32,params:ShaderParams)->GlassWobble {
 let a=params.wobble.x;
 let k=params.wobble.y;
 let theta=GLASS_TAU*params.wobble.z*phase;
 let u=k*p.y+theta;
 let v=k*p.x+0.5*theta;
 let gradient=vec3(-a*k*sin(u)*sin(v),a*k*cos(u)*cos(v),0.);
 return GlassWobble(a*sin(u)*cos(v),gradient);
}

fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 var out=v;
 if params.wobble.x==0. {
  return out;
 }
 let n=v.normal;
 let wobble=glass_wobble(v.position,ctx.phase,params);
 out.position=v.position+n*wobble.offset;
 // The displaced surface's normal: the rest normal tilted against the
 // wave's slope along the surface, scaled to the displaced radius.
 let radius=max(length(v.position),1e-4);
 let slope=wobble.gradient-n*dot(n,wobble.gradient);
 out.normal=normalize(n-slope*radius/max(radius+wobble.offset,1e-4));
 return out;
}

fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 var out=s;
 out.attenuation=params.absorption.rgb;
 out.thickness=params.absorption.w;
 let path=scene_volume_path(ctx);
 // The path is in metres, the thickness in the mesh's units: exact under
 // the uniform scales the example poses with.
 let scale=(ctx.model_scale.x+ctx.model_scale.y+ctx.model_scale.z)/3.;
 let measured=path.length/scale;
 if ctx.front {
  if path.bound==VOLUME_EXIT||path.bound==VOLUME_OPAQUE {
   out.thickness=measured;
  }
 } else if path.bound==VOLUME_EYE {
  out.thickness=measured;
 } else if path.bound==VOLUME_ENTRY {
  out.thickness=0.;
  out.base_color=vec4(s.base_color.rgb,0.);
 }
 return out;
}
