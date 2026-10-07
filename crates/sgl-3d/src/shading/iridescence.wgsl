// KHR_materials_iridescence's thin film over a surface's specular layer:
// Belcour and Barla 2017, "A Practical Extension to Microfacet Theory for
// the Modeling of Varying Iridescence", in the approximate form the
// extension specifies (Schlick in place of the polarised Fresnel equations,
// and the spectral integral evaluated in Fourier space against Gaussians
// fitted to the XYZ colour matching functions), evaluated once at the
// view's N.V and refit into the F0 the lobes read.
//
// Ported from Filament ef1a133 shaders/src/surface_brdf.fs 160–289
// (F_SchlickIridescence, iorToF0, f0ToIor, evaluateIridescenceSensitivity,
// F_Iridescence, F_IridescenceToF0), Apache-2.0, see LICENSE-filament.txt.
// Modified: prefixed snake-case names; f32 throughout, as Filament's highp;
// iridescence_refit inverts the Schlick curve toward the material's F90
// (KHR_materials_specular's, which metallic mixes toward 1) where
// F_IridescenceToF0 takes 1, so the base lobe's Schlick, at its own F90,
// passes through the film's Fresnel at the view; the film's strength mixes
// and the Khronos glTF Sample Renderer's zero-thickness rule are the
// caller's (surface_film, surface_f0s).
fn iridescence_pow5(x:f32)->f32 {
 let x2=x*x;
 return x2*x2*x;
}
// Schlick with a reflectance of one at grazing incidence, which an
// interface between two dielectrics has.
fn iridescence_schlick(f0:f32,cos_theta:f32)->f32 {
 return f0+(1.-f0)*iridescence_pow5(clamp(1.-cos_theta,0.,1.));
}
fn iridescence_schlick3(f0:vec3<f32>,cos_theta:f32)->vec3<f32> {
 return f0+(vec3(1.)-f0)*iridescence_pow5(clamp(1.-cos_theta,0.,1.));
}
// The reflectance at normal incidence of an interface into `transmitted`
// from `incident`, and the IOR a dielectric of reflectance `f0` has from
// air (an approximation for a metal, whose complex index it cannot
// recover).
fn iridescence_ior_to_f0(transmitted:f32,incident:f32)->f32 {
 let t=(transmitted-incident)/(transmitted+incident);
 return t*t;
}
fn iridescence_ior_to_f0_3(transmitted:vec3<f32>,incident:f32)->vec3<f32> {
 let t=(transmitted-vec3(incident))/(transmitted+vec3(incident));
 return t*t;
}
fn iridescence_f0_to_ior(f0:vec3<f32>)->vec3<f32> {
 let r=sqrt(f0);
 return (vec3(1.)+r)/(vec3(1.)-r);
}
// The XYZ sensitivity curves in Fourier space at one optical path
// difference `opd` in nanometres and phase `shift`: four Gaussians, one a
// curve plus the second lobe of x, converted to linear Rec. 709.
fn iridescence_sensitivity(opd:f32,shift:vec3<f32>)->vec3<f32> {
 // The path difference arrives in nanometres and the fits are in inverse
 // metres.
 let phase=2.*3.14159265359*opd*1e-9;
 let value=vec3(5.4856e-13,4.4201e-13,5.2481e-13);
 let position=vec3(1.6810e+06,1.7953e+06,2.2084e+06);
 let variance=vec3(4.3278e+09,9.3046e+09,6.6121e+09);
 var xyz=value*sqrt(2.*3.14159265359*variance)*cos(position*phase+shift)*exp(-variance*phase*phase);
 xyz.x+=9.7470e-14*sqrt(2.*3.14159265359*4.5282e+09)*cos(2.2399e+06*phase+shift.x)*exp(-4.5282e+09*phase*phase);
 xyz/=1.0685e-7;
 let xyz_to_rec709=mat3x3<f32>(
  3.2404542,-0.9692660,0.0556434,
  -1.5371385,1.8760108,-0.2040259,
  -0.4985314,0.0415560,1.0572252);
 return xyz_to_rec709*xyz;
}
// The Fresnel of a base of reflectance `base_f0` under a film of IOR
// `film_ior` and thickness `thickness` in nanometres, seen from a medium of
// IOR `outside_ior` at cosine `cos_theta1`. The film absorbs nothing, so
// what leaves is the geometric series of what bounces between its two
// interfaces, expanded to the second order as the extension does.
fn iridescence_fresnel(outside_ior:f32,film_ior:f32,base_f0:vec3<f32>,thickness:f32,cos_theta1:f32)->vec3<f32> {
 // A film thin enough not to be there behaves as though it is not.
 let ior=mix(outside_ior,film_ior,smoothstep(0.,.03,thickness));
 // Snell's law through the film; past the critical angle nothing enters.
 let cos_theta2_sq=1.-(outside_ior/ior)*(outside_ior/ior)*(1.-cos_theta1*cos_theta1);
 if cos_theta2_sq<0. {
  return vec3(1.);
 }
 let cos_theta2=sqrt(cos_theta2_sq);
 let r12=iridescence_schlick(iridescence_ior_to_f0(ior,outside_ior),cos_theta1);
 let t121=1.-r12;
 // The clamp short of one keeps a white conductor's F0 from taking
 // iridescence_f0_to_ior to infinity.
 let base_ior=iridescence_f0_to_ior(clamp(base_f0,vec3(0.),vec3(.9999)));
 let r23=iridescence_schlick3(iridescence_ior_to_f0_3(base_ior,ior),cos_theta2);
 let opd=2.*ior*thickness*cos_theta2;
 // The phase each reflection picks up: nothing entering a denser medium,
 // half a turn entering a thinner one.
 let phi21=3.14159265359-select(0.,3.14159265359,ior<outside_ior);
 let phi=vec3(phi21)+select(vec3(0.),vec3(3.14159265359),base_ior<vec3(ior));
 let r123=clamp(r12*r23,vec3(1e-5),vec3(.9999));
 let amplitude=sqrt(r123);
 let rs=t121*t121*r23/(vec3(1.)-r123);
 var reflected=vec3(r12)+rs;
 var cm=rs-vec3(t121);
 for (var m=1;m<=2;m++) {
  cm*=amplitude;
  reflected+=cm*2.*iridescence_sensitivity(f32(m)*opd,f32(m)*phi);
 }
 return max(reflected,vec3(0.));
}
// The F0 whose Schlick curve toward `f90` passes through `fresnel` at
// cosine `cos_theta`: F = f0 + (f90 - f0) w inverted, w the Schlick weight
// at that angle, clamped short of one where no F0 reproduces anything but
// f90.
fn iridescence_refit(fresnel:vec3<f32>,cos_theta:f32,f90:f32)->vec3<f32> {
 let weight=min(iridescence_pow5(clamp(1.-cos_theta,0.,1.)),.9999);
 return clamp((fresnel-vec3(f90*weight))/(1.-weight),vec3(0.),vec3(1.));
}
