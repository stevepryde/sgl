// A rectangle light's integral over its face: linearly transformed cosines
// (Heitz, Dupuy, Hill and Neubelt 2016, "Real-Time Polygonal-Light Shading
// with Linearly Transformed Cosines"). Reads `lookup_tables` (its fit's
// layers, lookup_tables.wgsl) and `environment_sampler`.
//
// Ported from Bevy 9d12036's crates/bevy_pbr/src/render/pbr_lighting.wesl
// (ltc_integrate_edge, ltc_integrate_quad and rect_light's table lookup and
// Fresnel weight), MIT OR Apache-2.0 (src/LICENSE-bevy.txt), with the fit
// of selfshadow/ltc_code (src/LICENSE-ltc-code.txt). Changes: the horizon
// clips the polygon's vector form factor as it clips a sphere's, from the
// fit's table, as ltc_code's own LTC_Evaluate does and Bevy's TODO
// proposes, in place of clipping the polygon, which costs more; the
// rectangle is transformed as its centre and half extents; and the tangent
// toward the view falls back to any tangent when the view is along the
// normal, where Bevy's normalizes a zero vector.
// Texel centres of the 64² table's first and last texels, from [0, 1].
const LTC_LUT_SCALE:f32=63./64.;
const LTC_LUT_BIAS:f32=.5/64.;
// The table's inverse matrix and its magnitude and Fresnel weights at a
// perceptual roughness and N.V.
struct RectLightFit {
 inverse:mat3x3<f32>,
 weights:vec2<f32>,
}
fn rect_light_fit(rough:f32,nv:f32)->RectLightFit {
 let uv=vec2(rough,sqrt(1.-nv))*LTC_LUT_SCALE+LTC_LUT_BIAS;
 let t1=textureSampleLevel(lookup_tables,environment_sampler,uv,LOOKUP_LTC_MATRIX_LAYER,0.);
 let t2=textureSampleLevel(lookup_tables,environment_sampler,uv,LOOKUP_LTC_WEIGHTS_LAYER,0.);
 let inverse=mat3x3(vec3(t1.x,0.,t1.y),vec3(0.,1.,0.),vec3(t1.z,0.,t1.w));
 return RectLightFit(inverse,t2.xy);
}
// The fit's specular reflectance for reflectance `f0` at normal and `f90`
// at grazing incidence: its magnitude and Fresnel weights, Schlick's
// f0 + (f90 - f0) (1 - v.h)^5 integrated (Bevy's and ltc_code's take F90 1).
fn rect_light_specular_weight(fit:RectLightFit,f0:vec3<f32>,f90:f32)->vec3<f32> {
 return f0*fit.weights.x+(vec3(f90)-f0)*fit.weights.y;
}
// One edge's vector form factor on the unit sphere: Eq. 11 with the
// polynomial fit of Hill and Heitz's 2016 talk, with its 1 / 2π.
fn ltc_integrate_edge_vec(v1:vec3<f32>,v2:vec3<f32>)->vec3<f32> {
 let x=dot(v1,v2);
 let y=abs(x);
 let a=.8543985+(.4965155+.0145206*y)*y;
 let b=3.417594+(4.1616724+y)*y;
 let v=a/b;
 let theta_sintheta=select(.5*inverseSqrt(max(1.-x*x,1e-7))-v,v,x>0.);
 return cross(v1,v2)*theta_sintheta;
}
// A rectangle relative to a receiver, in the receiver's frame for its unit
// normal and view: the tangent toward the view, the bitangent and the normal
// (ltc_integrate_quad's basis).
struct RectLightFrame {
 center:vec3<f32>,
 half_width:vec3<f32>,
 half_height:vec3<f32>,
}
fn rect_light_frame(n:vec3<f32>,v:vec3<f32>,center:vec3<f32>,half_width:vec3<f32>,half_height:vec3<f32>)->RectLightFrame {
 var t1=v-n*dot(v,n);
 if dot(t1,t1)<1e-10 {
  // Any tangent: the fit is symmetric about the normal at normal incidence.
  t1=select(vec3(1.,0.,0.),vec3(0.,1.,0.),abs(n.x)>.9)-n*select(n.x,n.y,abs(n.x)>.9);
 }
 t1=normalize(t1);
 let basis=transpose(mat3x3(t1,-cross(n,t1),n));
 return RectLightFrame(basis*center,basis*half_width,basis*half_height);
}
// The cosine-weighted integral over the rectangle in `frame` of the
// distribution `inverse` transforms the clamped cosine into: the form
// factor for the identity. Its corners wind as Bevy's do about its normal.
// The horizon clips it as it clips a sphere of the same vector form factor,
// from the fit's table (ltc_code's LTC_Evaluate; Hill and Heitz 2016,
// slides 87-102).
fn ltc_integrate_quad(frame:RectLightFrame,inverse:mat3x3<f32>)->f32 {
 let c=inverse*frame.center;
 let w=inverse*frame.half_width;
 let h=inverse*frame.half_height;
 let l0=normalize(c+w-h);
 let l1=normalize(c-w-h);
 let l2=normalize(c-w+h);
 let l3=normalize(c+w+h);
 let form=ltc_integrate_edge_vec(l0,l1)+ltc_integrate_edge_vec(l1,l2)+ltc_integrate_edge_vec(l2,l3)+ltc_integrate_edge_vec(l3,l0);
 let size=length(form);
 if size<=0. {
  return 0.;
 }
 let uv=vec2(form.z/size*.5+.5,size)*LTC_LUT_SCALE+LTC_LUT_BIAS;
 return size*textureSampleLevel(lookup_tables,environment_sampler,uv,LOOKUP_LTC_WEIGHTS_LAYER,0.).w;
}
