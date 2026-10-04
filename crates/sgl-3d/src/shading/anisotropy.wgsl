// KHR_materials_anisotropy acfcbe65e40c53d6d3aa55a7299982bf2c01c75d.
// Shared authored frame and named KHR bent-normal IBL approximation.
fn pbr_tangent_frame(n:vec3<f32>,tangent:vec4<f32>)->mat3x3<f32> {
 let t=normalize(tangent.xyz-n*dot(n,tangent.xyz));
 return mat3x3(t,cross(n,t)*tangent.w,n);
}
fn pbr_resolve_anisotropy(n:vec3<f32>,frame:mat3x3<f32>,material_strength:f32,rotation:f32,textured:bool,texel:vec3<f32>)->vec4<f32> {
 if material_strength<=0. {
  return vec4(0.);
 }
 var direction=vec2(1.,0.);
 var strength=material_strength;
 if textured {
  let raw=texel.rg*2.-vec2(1.);
  if dot(raw,raw)>0. {
   direction=normalize(raw);
  }
  strength*=texel.b;
 }
 let c=cos(rotation);
 let s=sin(rotation);
 let rotated=vec2(c*direction.x-s*direction.y,s*direction.x+c*direction.y);
 let axis=frame[0]*rotated.x+frame[1]*rotated.y;
 let projected=axis-n*dot(n,axis);
 // A map normal parallel to the authored axis has no unique projection.
 // Its perpendicular authored column supplies a deterministic tangent limit.
 var t=projected;
 if dot(t,t)<1e-12 {
  t=frame[1]-n*dot(n,frame[1]);
 }
 return vec4(normalize(t),clamp(strength,0.,1.));
}
fn pbr_anisotropy_bent_normal(n:vec3<f32>,v:vec3<f32>,axis_strength:vec4<f32>,rough:f32)->vec3<f32> {
 if axis_strength.w<=0. {
  return n;
 }
 let perpendicular=cross(n,axis_strength.xyz);
 // Stored half-float axes are reprojected against the sampled receiver normal.
 // At a discontinuity an unresolved frame falls back to the original normal.
 if dot(perpendicular,perpendicular)<1e-12 {
  return n;
 }
 let b=normalize(perpendicular);
 let projected=v-b*dot(v,b);
 var bent=n;
 if dot(projected,projected)>1e-12 {
  bent=normalize(projected);
 }
 let a=pow(1.-axis_strength.w*(1.-rough),4.);
 return normalize(mix(bent,n,a));
}
fn pbr_anisotropy_reflection(n:vec3<f32>,v:vec3<f32>,axis_strength:vec4<f32>,rough:f32)->vec3<f32> {
 let bent=pbr_anisotropy_bent_normal(n,v,axis_strength,rough);
 return normalize(mix(reflect(-v,bent),bent,pow(rough,4.)));
}
