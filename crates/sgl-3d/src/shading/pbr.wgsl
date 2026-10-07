// GGX / height-correlated Smith, with Schlick Fresnel. Perceptual roughness
// is squared exactly once to obtain the microfacet distribution's alpha.
// References and the integration convention are in docs/native-pbr.md.
fn pbr_fresnel(c:f32,f0:vec3<f32>)->vec3<f32> {
 return f0+(vec3(1.)-f0)*pow(clamp(1.-c,0.,1.),5.);
}
// Geometric specular antialiasing (Kaplanyan 2016; Tokuyoshi and Kaplanyan
// 2019). Ported from Filament ef1a133 shaders/src/surface_shading_lit.fs
// normalFiltering with its material defaults (variance 0.15, threshold 0.2,
// libs/filamat/include/filamat/MaterialBuilder.h) and desktop
// MIN_PERCEPTUAL_ROUGHNESS (surface_material.fs); Apache-2.0, see
// LICENSE-filament.txt. Modified: translated to WGSL, derivatives scale of 1.
// The filtered roughness is what the G-buffer holds, as in Filament, HDRP and
// Unreal.
fn pbr_filtered_roughness(rough:f32,geometry_normal:vec3<f32>)->f32 {
 let perceptual=clamp(rough,0.045,1.);
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

// The reflectance at grazing incidence of a surface whose reflectance at
// normal incidence is `f0`: 1 for any F0 of 0.02 or more, which every real
// material has, falling to 0 with F0 below it, so an F0 under 0.02 (a
// specular strength under one half, a near-black metal) also takes the
// grazing reflection away, a specular occlusion baked into F0. Filament
// ef1a133 derives it so for its Fresnel (shaders/src/surface_brdf.fs
// fresnel, Apache-2.0, see LICENSE-filament.txt) and Bevy 9d12036 for its
// Fresnel and environment's specular occlusion
// (crates/bevy_pbr/src/render/pbr_lighting.wesl fresnel,
// light_probe/environment_map.wesl; MIT OR Apache-2.0, see
// LICENSE-bevy.txt). Three.js's PhysicalLightingModel, which SGL3D's BRDF
// follows, takes F90 as an input.
fn pbr_f90(f0:vec3<f32>)->f32 {
 return saturate(dot(f0,vec3(50.*.33)));
}
// Three.js 0.185.1 PhysicalLightingModel / BRDF_GGX_Multiscatter: F_Schlick
// with Epic's exponent, from `f0` toward `f90`.
fn pbr_three_fresnel(c:f32,f0:vec3<f32>,f90:f32)->vec3<f32> {
 let f=exp2((-5.55473*c-6.98316)*c);
 return f0*(1.-f)+vec3(f90*f);
}
// A coat's Fresnel toward the view, weighted by the coat, as Three.js 0.185.1
// PhysicalLightingModel.finish evaluates it: at
// clearcoatNormalView.dot(positionViewDirection).clamp() (to [0, 1]) with
// F0 0.04 and F90 1. Callers dim the light beneath the coat by it.
fn pbr_coat_fresnel(coat_normal:vec3<f32>,view:vec3<f32>,coat:f32)->f32 {
 let coat_view_cosine=clamp(dot(coat_normal,view),0.,1.);
 return coat*pbr_three_fresnel(coat_view_cosine,vec3(.04),1.).x;
}
fn pbr_three_specular(n:vec3<f32>,v:vec3<f32>,l:vec3<f32>,r:f32,f0:vec3<f32>,f90:f32)->vec3<f32> {
 let nv=clamp(dot(n,v),0.,1.);
 let nl=clamp(dot(n,l),0.,1.);
 let h=normalize(v+l);
 let nh=clamp(dot(n,h),0.,1.);
 let vh=clamp(dot(v,h),0.,1.);
 let a2=pow(r,4.);
 let denominator=1.-nh*nh*(1.-a2);
 let distribution=a2/(denominator*denominator*3.14159265359);
 let visibility=0.5/max(nl*sqrt(a2+(1.-a2)*nv*nv)+nv*sqrt(a2+(1.-a2)*nl*nl),0.000001);
 return pbr_three_fresnel(vh,f0,f90)*visibility*distribution;
}
// Three.js 0.185.1's split-sum single scattering (EnvironmentBRDF,
// computeMultiscattering's FssEss): specularColor * fab.x + specularF90 * fab.y.
fn pbr_three_single_scatter(f0:vec3<f32>,f90:f32,dfg:vec2<f32>)->vec3<f32> {
 return f0*dfg.x+vec3(f90*dfg.y);
}
fn pbr_three_multi_scatter(f0:vec3<f32>,f90:f32,dfg:vec2<f32>)->vec3<f32> {
 let single=pbr_three_single_scatter(f0,f90,dfg);
 let missing=1.-dfg.x-dfg.y;
 let average=f0+(vec3(1.)-f0)*0.047619;
 return single*average/(vec3(1.)-missing*average)*missing;
}
struct PbrIblWeights {
 single:vec3<f32>,
 multi:vec3<f32>,
 diffuse:vec3<f32>
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
// scattering at `dielectric_f0` and the metal's at `base`, each toward the
// surface's `f90`, mixed by metallic; the diffuse keeps what the dielectric
// does not scatter.
fn pbr_ibl_weights(base:vec3<f32>,metallic:f32,dielectric_f0:vec3<f32>,f90:f32,dfg:vec2<f32>)->PbrIblWeights {
 let dielectric_single=pbr_three_single_scatter(dielectric_f0,f90,dfg);
 let dielectric_multi=pbr_three_multi_scatter(dielectric_f0,f90,dfg);
 return PbrIblWeights(mix(dielectric_single,pbr_three_single_scatter(base,f90,dfg),metallic),
  mix(dielectric_multi,pbr_three_multi_scatter(base,f90,dfg),metallic),
  base*(1.-metallic)*(vec3(1.)-dielectric_single-dielectric_multi));
}

// KHR directional roughness and GGX distribution; correlated Smith visibility
// retains Three's denominator floor, without the illustrative KHR upper clamp.
// Fresnel, diffuse and isotropic DFG compensation retain the existing model.
fn pbr_anisotropic_specular(n:vec3<f32>,v:vec3<f32>,l:vec3<f32>,rough:f32,f0:vec3<f32>,f90:f32,axis_strength:vec4<f32>)->vec3<f32> {
 if axis_strength.w<=0. {
  return pbr_three_specular(n,v,l,rough,f0,f90);
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
 return pbr_three_fresnel(clamp(dot(v,h),0.,1.),f0,f90)*distribution*(0.5/max(gv+gl,.000001));
}
