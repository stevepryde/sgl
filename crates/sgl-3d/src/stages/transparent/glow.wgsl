// GlowVertex::kind (shading/vertex.rs): a tapered profile and a line; any
// other kind is uniform.
const GLOW_TAPERED:u32=1u;
const GLOW_LINE:u32=2u;
// A glow vertex as the scene's glow buffer holds it (GlowVertex).
struct GlowVertex {
 @location(0) position:vec3<f32>,
 @location(1) color:vec4<f32>,
 @location(2) kind:u32,
 @location(3) soft_distance:f32,
 @location(4) uv:vec2<f32>,
 @location(5) taper:f32,
 @location(6) ripple_frequency:vec2<f32>,
 @location(7) ripple_amplitude:f32,
 @location(8) other:vec3<f32>,
 @location(9) offset:f32,
}
struct Glow {
 @builtin(position) clip:vec4<f32>,
 @location(0) uv:vec2<f32>,
 @location(1) color:vec4<f32>,
 @location(2) @interpolate(flat) kind:u32,
 @location(3) view_depth:f32,
 @location(4) @interpolate(flat) soft_distance:f32,
 @location(5) @interpolate(flat) taper:f32,
 @location(6) @interpolate(flat) ripple_frequency:vec2<f32>,
 @location(7) @interpolate(flat) ripple_amplitude:f32,
}
@vertex fn glow_vs(vertex:GlowVertex)->Glow {
 let position=vertex.position;
 let other=vertex.other;
 var o:Glow;
 o.clip=view.view_projection*vec4(position,1.);
 o.uv=vertex.uv;
 o.color=vertex.color;
 o.kind=vertex.kind;
 o.soft_distance=vertex.soft_distance;
 o.taper=vertex.taper;
 o.ripple_frequency=vertex.ripple_frequency;
 o.ripple_amplitude=vertex.ripple_amplitude;
 o.view_depth=-(view.view*vec4(position,1.)).z;
 if vertex.kind==GLOW_LINE {
  var end=view.view_projection*vec4(other,1.);
  // Expand the visible segment. Dividing an endpoint behind the camera by a
  // clamped W changes its projected direction and skews the one-pixel strip.
  // Near plane is clip.z = clip.w; clip before the perspective division.
  let start_near=o.clip.w-o.clip.z;
  let end_near=end.w-end.z;
  if start_near<0. && end_near<0. {
   o.clip=vec4(0.,0.,-1.,1.);
   return o;
  }
  if start_near<0. {
   let t=start_near/(start_near-end_near);
   o.clip=mix(o.clip,end,t);
   o.clip.z=o.clip.w;
   let end_depth=-(view.view*vec4(other,1.)).z;
   o.view_depth=mix(o.view_depth,end_depth,t);
  } else if end_near<0. {
   let t=end_near/(end_near-start_near);
   end=mix(end,o.clip,t);
   end.z=end.w;
  }
  let size=max(view.viewport,vec2(1.));
  let delta=(end.xy/end.w-o.clip.xy/o.clip.w)*size;
  let along=delta/max(length(delta),.00001);
  // One drawing-buffer pixel, perpendicular to the projected segment at every depth.
  o.clip=vec4(o.clip.xy+vec2(-along.y,along.x)*vertex.offset*2./size*o.clip.w,o.clip.zw);
 }
 return o;
}
fn glow_color(i:Glow)->vec4<f32> {
 var alpha=i.color.a;
 if i.kind==GLOW_TAPERED {
  let ripple=sin(dot(i.uv,i.ripple_frequency))*i.ripple_amplitude+1.-i.ripple_amplitude;
  alpha*=pow(1.-i.uv.y,i.taper)*ripple;
 }
 return vec4(frame_fog(i.color.rgb,i.clip.xy,i.view_depth),alpha);
}

@group(1) @binding(1) var effect_depth:texture_depth_2d;
fn soft_fade(i:Glow)->f32 {
 if i.soft_distance<=0. {
  return 1.;
 }
 let z=textureLoad(effect_depth,vec2<i32>(i.clip.xy),0);
 // A clear depth texel is sky.
 if z<=0. {
  return 1.;
 }
 // Solve z_ndc=(P22*z_view+P32)/(P23*z_view+P33).
 // This works for conventional perspective and orthographic projections.
 let p=view.projection;
 let scene_depth=(p[3][2]-z*p[3][3])/(p[2][2]-z*p[2][3]);
 return clamp((scene_depth-i.view_depth)/i.soft_distance,0.,1.);
}
fn glow_soft_color(i:Glow)->vec4<f32> {
 let color=glow_color(i);
 return vec4(color.rgb,color.a*soft_fade(i));
}
@fragment fn glow_soft_fs(i:Glow)->@location(0) vec4<f32> {
 return glow_soft_color(i);
}
// FSR2's masks (fsr2.rs): additive glow is a reactive emitter in AMD's FSR
// sample (FidelityFX SDK 1.1.4, MIT, see LICENSE-amd-fidelityfx.txt,
// framework/rendermodules/translucency/shaders/particlerender.hlsl:249-254 and
// samples/fsrapi/fsrapirendermodule.cpp:100-101): reactivity max(r, g, b)
// times alpha, no transparency and composition. Modified: the reactivity is
// clamped to 0.9, as AMD's FSR2 documentation recommends ("Reactive mask"),
// because this radiance is HDR.
struct GlowFsr2Masked {
 @location(0) color:vec4<f32>,
 @location(1) reactive:f32,
 @location(2) composition:f32,
}
@fragment fn glow_soft_fsr2_masked_fs(i:Glow)->GlowFsr2Masked {
 let color=glow_soft_color(i);
 return GlowFsr2Masked(color,min(max(color.r,max(color.g,color.b))*color.a,.9),0.);
}
