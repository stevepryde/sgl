// SGL3D's BRDF, by the references D-32 names (specs/decisions.md): glTF 2.0
// and its KHR extensions for what a material's values mean, Filament's
// shading maths and energy treatment as Bevy's WGSL ports them, three.js
// r185 for what Filament lacks. GGX with height-correlated Smith visibility
// and Schlick's Fresnel; perceptual roughness is squared once to give the
// distribution's alpha. Bevy 9d12036 crates/bevy_pbr/src/render/
// pbr_lighting.wesl, MIT OR Apache-2.0 (LICENSE-bevy.txt); Filament ef1a133
// shaders/src, Apache-2.0 (LICENSE-filament.txt); three.js r185
// src/nodes/functions, MIT (stages/post/smaa/LICENSE-three.txt). Modified:
// translated to WGSL.
// The least perceptual roughness a surface shades at: Filament's desktop
// MIN_PERCEPTUAL_ROUGHNESS (surface_material.fs), at raster fragments and ray
// hits alike.
const PBR_MIN_PERCEPTUAL_ROUGHNESS:f32=.045;
// Geometric specular antialiasing (Kaplanyan 2016; Tokuyoshi and Kaplanyan
// 2019). Ported from Filament ef1a133 shaders/src/surface_shading_lit.fs
// normalFiltering with its material defaults (variance 0.15, threshold 0.2,
// libs/filamat/include/filamat/MaterialBuilder.h) and desktop
// MIN_PERCEPTUAL_ROUGHNESS (surface_material.fs); Apache-2.0, see
// LICENSE-filament.txt. Modified: translated to WGSL, derivatives scale of 1.
// The filtered roughness is what the G-buffer holds, as in Filament, HDRP and
// Unreal.
fn pbr_filtered_roughness(rough:f32,geometry_normal:vec3<f32>)->f32 {
 let perceptual=clamp(rough,PBR_MIN_PERCEPTUAL_ROUGHNESS,1.);
 let du=dpdx(geometry_normal);
 let dv=dpdy(geometry_normal);
 let variance=0.15*(dot(du,du)+dot(dv,dv));
 let roughness=perceptual*perceptual;
 let kernel_roughness=min(2.*variance,0.2);
 let square_roughness=saturate(roughness*roughness+kernel_roughness);
 return sqrt(sqrt(square_roughness));
}
fn pbr_bump_normal(map:texture_2d<f32>,filtering:sampler,world:vec3<f32>,n:vec3<f32>,uv:vec2<f32>,scale:f32,face:f32)->vec3<f32> {
 // Three r185 BumpMapNode: normalized positional derivatives and forward
 // texture samples. GLSL dFdy is -dpdy on the WebGPU coordinate system.
 let sx=normalize(dpdx(world));
 let sy=normalize(-dpdy(world));
 let uv_dx=dpdx(uv);
 let uv_dy=-dpdy(uv);
 let height=textureSample(map,filtering,uv).r;
 let gradient=vec2(textureSample(map,filtering,uv+uv_dx).r-height,
  textureSample(map,filtering,uv+uv_dy).r-height)*scale;
 let a=cross(sy,n);
 let b=cross(n,sx);
 let determinant=dot(sx,a)*face;
 return normalize(abs(determinant)*n-sign(determinant)*(gradient.x*a+gradient.y*b));
}

// Schlick's Fresnel at cosine `c` from `f0` at normal incidence toward `f90`
// at grazing: Bevy's F_Schlick_vec (306-309), as Filament's F_Schlick
// (surface_brdf.fs) and the Khronos glTF Sample Renderer's F_Schlick
// (0686eb2 source/Renderer/shaders/brdf.glsl 30-36, whose multiplications
// this takes in place of pow) evaluate it; the DFG table integrates it.
fn pbr_fresnel_schlick(c:f32,f0:vec3<f32>,f90:f32)->vec3<f32> {
 let x=clamp(1.-c,0.,1.);
 let x2=x*x;
 return f0+(vec3(f90)-f0)*(x2*x2*x);
}
// A coat's Fresnel toward the view, weighted by the coat: Schlick's
// (pbr_fresnel_schlick) at the coat normal's cosine to the view, with F0
// 0.04 and F90 1, the cosine at which Three.js 0.185.1's
// PhysicalLightingModel.finish takes it. Callers dim the light beneath the
// coat by it.
fn pbr_coat_fresnel(coat_normal:vec3<f32>,view:vec3<f32>,coat:f32)->f32 {
 let coat_view_cosine=clamp(dot(coat_normal,view),0.,1.);
 return coat*pbr_fresnel_schlick(coat_view_cosine,vec3(.04),1.).x;
}
// Bevy's D_GGX (146-152): Walter et al. 2007's GGX distribution at alpha
// `roughness`.
fn D_GGX(roughness:f32,NdotH:f32)->f32 {
 let oneMinusNdotHSquared=1.-NdotH*NdotH;
 let a=NdotH*roughness;
 let k=roughness/(oneMinusNdotHSquared+a*a);
 let d=k*k*(1./3.14159265359);
 return d;
}
// Bevy's V_SmithGGXCorrelated (185-191): height-correlated Smith visibility
// at alpha `roughness`. Changed: the denominator is at least 1e-6, as
// three.js's BRDF_GGX keeps it, so a light and view both at the horizon give
// zero, not infinity.
fn V_SmithGGXCorrelated(roughness:f32,NdotV:f32,NdotL:f32)->f32 {
 let a2=roughness*roughness;
 let lambdaV=NdotL*sqrt((NdotV-a2*NdotV)*NdotV+a2);
 let lambdaL=NdotV*sqrt((NdotL-a2*NdotL)*NdotL+a2);
 let v=0.5/max(lambdaV+lambdaL,.000001);
 return v;
}
// The isotropic GGX lobe of `n`, `v` and `l` at perceptual roughness `r`,
// reflecting `f0` at normal and `f90` at grazing incidence: Bevy's specular
// (405-427) without its multiple scattering, which the caller applies
// (pbr_multiscatter_gain).
fn pbr_ggx_specular(n:vec3<f32>,v:vec3<f32>,l:vec3<f32>,r:f32,f0:vec3<f32>,f90:f32)->vec3<f32> {
 let nv=clamp(dot(n,v),0.,1.);
 let nl=clamp(dot(n,l),0.,1.);
 let h=normalize(v+l);
 let nh=clamp(dot(n,h),0.,1.);
 let vh=clamp(dot(v,h),0.,1.);
 let alpha=r*r;
 return pbr_fresnel_schlick(vh,f0,f90)*D_GGX(alpha,nh)*V_SmithGGXCorrelated(alpha,nv,nl);
}
// The split sum's single scattering from the DFG table's scale and bias
// `dfg`, toward `f90` at grazing: Bevy's EnvBRDFApprox (542-544), three.js's
// EnvironmentBRDF.
fn pbr_split_sum(f0:vec3<f32>,f90:f32,dfg:vec2<f32>)->vec3<f32> {
 return f0*dfg.x+vec3(f90*dfg.y);
}
// The gain that restores the energy a GGX lobe's single scattering loses to
// multiple scattering, from its DFG lookup `dfg` at the view: Fdez-Agüera
// 2019 ("A Multiple-Scattering Microfacet Model for Real-Time Image Based
// Lighting", JCGT 8(1)), F_ss E_ss + F_ms E_ms = F_ss E_ss / (1 - F_avg
// E_ms), as three.js's computeMultiscattering (PhysicalLightingModel.js
// 570-590) and Khronos's getIBLGGXFresnel (ibl.glsl 26-43) apply it to the
// environment. Every lobe that scatters takes it, under direct light too,
// where Filament and Bevy scale the lobe by 1 + F0 (1/E - 1)
// (surface_shading_lit.fs 268; specular_multiscatter, 331-344): the two
// agree on a white metal, and this keeps direct and environment light alike
// on coloured metals (D-32).
// The energy single scattering misses is never below 0: the table's Monte
// Carlo noise puts its sum a hair above 1 on the smoothest lobes, which
// would take light from them.
fn pbr_multiscatter_gain(f0:vec3<f32>,dfg:vec2<f32>)->vec3<f32> {
 let missing=max(1.-dfg.x-dfg.y,0.);
 let average=f0+(vec3(1.)-f0)*0.047619;
 return vec3(1.)/(vec3(1.)-missing*average);
}
// A sized light as a specular lobe of perceptual roughness `rough` sees it,
// seen along `view`, toward unit `direction` from a sphere of radius `size`
// at unit distance (a point or spot light's radius over its distance, a
// directional light's disc radius): Karis's representative point (2013,
// "Real Shading in Unreal Engine 4", 14-16, eq. 11), the point of the
// sphere nearest the lobe's `reflected` ray, as Bevy's
// compute_specular_layer_values_for_point_light finds it (361-401), and his
// normalisation (alpha / alpha')² (eq. 14) of the distribution widened by
// the light's cone (eq. 10). Changed: the cone is taken into half-vector
// space by its Jacobian, dw_h = dw_l / (4 l.h) (Walter et al. 2007, eq. 14),
// a cone of radius size / (2 sqrt(l.h)) where Karis's eq. 10 takes its
// normal-incidence value size / 2; Bevy's specular_fix_remap and
// solid-angle factor are not taken; and the directional light's disc is a
// sphere at unit distance. D-32 (specs/decisions.md) records the
// measurements behind each. A size of 0 is the light's own direction,
// whole.
struct PbrSizedLight {
 direction:vec3<f32>,
 intensity:f32,
}
fn pbr_sized_light(direction:vec3<f32>,size:f32,reflected:vec3<f32>,view:vec3<f32>,rough:f32)->PbrSizedLight {
 if size<=0. {
  return PbrSizedLight(direction,1.);
 }
 // Bevy's LtFdotR, kept positive (bevyengine/bevy#13318), and the vector
 // from the sphere's centre to the nearest point of the ray.
 let LtFdotR=max(.0001,dot(direction,reflected));
 let centerToRay=LtFdotR*reflected-direction;
 let closestPoint=direction+centerToRay*saturate(size*inverseSqrt(max(dot(centerToRay,centerToRay),1e-12)));
 let l=normalize(closestPoint);
 // The light's cone in half-vector space at the representative point, l.h
 // of unit l and view, sqrt((1 + v.l) / 2), with no half vector to
 // normalise. It is 0 only for a light straight behind the view, at
 // grazing, where the Jacobian is unbounded: the floor keeps alpha' finite,
 // and saturate holds it at 1, the widest lobe, there.
 let lh=sqrt(max((1.+dot(view,l))*.5,1e-8));
 let a=rough*rough;
 let a_prime=saturate(a+size/(2.*sqrt(lh)));
 let normalizationFactor=a/a_prime;
 return PbrSizedLight(l,normalizationFactor*normalizationFactor);
}
// The environment's single and multiple scattering, the diffuse weight of
// a Lambertian of the base's colour beneath them, and the share of a
// Lambertian's light the dielectric's scattering keeps, whatever its
// colour (the diffuse weight over the base).
struct PbrIblWeights {
 single:vec3<f32>,
 multi:vec3<f32>,
 diffuse:vec3<f32>,
 kept:vec3<f32>,
}
fn pbr_hemisphere(n:vec3<f32>,upper:vec3<f32>,ground:vec3<f32>,intensity:f32)->vec3<f32> {
 return mix(ground,upper,n.y*.5+.5)*intensity;
}
// The radiance whose irradiance pbr_hemisphere is: the upper colour above
// the horizon and the ground colour below, times the intensity over PI. A
// normal n sees the upper half over a projected solid angle of
// PI (1 + n.y) / 2 and the lower over the rest, so their irradiance is
// pbr_hemisphere's mix.
fn pbr_hemisphere_radiance(direction:vec3<f32>,upper:vec3<f32>,ground:vec3<f32>,intensity:f32)->vec3<f32> {
 return select(ground,upper,direction.y>=0.)*intensity/3.14159265359;
}
// Three.js 0.185.1 PhysicalLightingModel.indirect: the dielectric's
// scattering at `dielectric_f0` and the metal's at `metal_f0` (the base,
// or the film's refit of it: computeMultiscattering's Fr), each toward the
// surface's `f90`, mixed by metallic, its multiple scattering the gain's
// share (pbr_multiscatter_gain); the diffuse keeps what the dielectric does
// not scatter. Every source of irradiance a surface takes is weighted by
// diffuse plus multi alike (D-32).
fn pbr_ibl_weights(base:vec3<f32>,metallic:f32,dielectric_f0:vec3<f32>,metal_f0:vec3<f32>,f90:f32,dfg:vec2<f32>)->PbrIblWeights {
 let dielectric_single=pbr_split_sum(dielectric_f0,f90,dfg);
 let dielectric_multi=dielectric_single*(pbr_multiscatter_gain(dielectric_f0,dfg)-vec3(1.));
 let metal_single=pbr_split_sum(metal_f0,f90,dfg);
 let metal_multi=metal_single*(pbr_multiscatter_gain(metal_f0,dfg)-vec3(1.));
 let kept=(1.-metallic)*(vec3(1.)-dielectric_single-dielectric_multi);
 return PbrIblWeights(mix(dielectric_single,metal_single,metallic),
  mix(dielectric_multi,metal_multi,metallic),
  base*kept,kept);
}

// The base GGX lobe stretched along a KHR_materials_anisotropy axis
// (acfcbe65e40c53d6d3aa55a7299982bf2c01c75d): its directional roughness
// alpha_t = mix(alpha_b, 1, strength²), alpha_b = roughness², and its
// distribution and height-correlated Smith visibility, as Bevy's
// D_GGX_anisotropic and V_GGX_anisotropic (170-176, 194-208) evaluate them,
// without KHR's illustrative clamp of the visibility to 1 so the lobe meets
// the isotropic one as the strength falls to 0; at strength 0 it is that one.
// The multiple scattering is the caller's (pbr_multiscatter_gain).
fn pbr_anisotropic_specular(n:vec3<f32>,v:vec3<f32>,l:vec3<f32>,rough:f32,f0:vec3<f32>,f90:f32,axis_strength:vec4<f32>)->vec3<f32> {
 if axis_strength.w<=0. {
  return pbr_ggx_specular(n,v,l,rough,f0,f90);
 }
 let t=axis_strength.xyz;
 let b=normalize(cross(n,t));
 let h=normalize(v+l);
 let nv=clamp(dot(n,v),0.,1.);
 let nl=clamp(dot(n,l),0.,1.);
 let at=mix(rough*rough,1.,axis_strength.w*axis_strength.w);
 let ab=rough*rough;
 let f=vec3(ab*dot(t,h),at*dot(b,h),at*ab*clamp(dot(n,h),0.,1.));
 let w2=at*ab/dot(f,f);
 let distribution=at*ab*w2*w2/3.14159265359;
 let gv=nl*length(vec3(at*dot(t,v),ab*dot(b,v),nv));
 let gl=nv*length(vec3(at*dot(t,l),ab*dot(b,l),nl));
 return pbr_fresnel_schlick(clamp(dot(v,h),0.,1.),f0,f90)*distribution*(0.5/max(gv+gl,.000001));
}
