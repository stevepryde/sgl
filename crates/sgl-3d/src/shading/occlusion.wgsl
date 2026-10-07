// Ambient occlusion of a receiver's ambient light: the visibility it takes,
// and how that occludes its ambient diffuse, the specular multiple
// scattering its indirect light carries, and each specular lobe's
// environment. Direct light and emission are not occluded; glTF's occlusion
// is of indirect light. Baked light takes the material's occlusion alone, in
// lit shading (surface.wgsl). Screen-space and world-space hits are visible
// surfaces and stay unoccluded, as in Filament. Source completion occludes
// the camera's opaque surfaces (stages/reflections/source.wgsl); lit shading
// (surface.wgsl) every other view's by its material's occlusion alone.

// A receiver's visibility: the lesser of its material's occlusion
// (material_occlusion) and the frame's ambient occlusion, as Filament
// ef1a133 takes min(material.ambientOcclusion, ssao) for its diffuse and
// specular ambient occlusion (shaders/src/surface_light_indirect.fs
// evaluateIBL), Bevy 9d12036 for its diffuse
// (crates/bevy_pbr/src/deferred/deferred_lighting.wesl) and Godot b130438
// for its ambient light (scene_forward_clustered.glsl).
fn occlusion_visibility(material:f32,ambient:f32)->f32 {
 return min(material,ambient);
}
// `color` without the share of its ambient light that occlusion hides,
// never negative: of its ambient diffuse `diffuse`, what `visibility` hides,
// and of the specular multiple scattering `multi` its indirect light
// carries, what `multi_occlusion` (occlusion_multiscatter) hides. Lit colour
// holds each as one additive term, so occluding them after shading equals
// occluding them while shading, as Bevy's deferred lighting applies SSAO in
// a screen pass over its G-buffer (9d12036
// crates/bevy_pbr/src/deferred/deferred_lighting.wesl:65-77).
fn occlusion_ambient(color:vec3<f32>,diffuse:vec3<f32>,multi:vec3<f32>,visibility:f32,multi_occlusion:vec3<f32>)->vec3<f32> {
 return max(vec3(0.),color-(1.-visibility)*diffuse-(vec3(1.)-multi_occlusion)*multi);
}
// Specular occlusion of a lobe's environment specular by the receiver's
// `visibility` of it, as Filament's desktop default evaluates it (ef1a133
// shaders/src/surface_ambient_occlusion.fs SpecularAO_Lagarde and
// gtaoMultiBounce, applied as surface_light_indirect.fs evaluateIBL and
// evaluateClearCoatIBL do; Apache-2.0, see LICENSE-filament.txt. Modified:
// translated to WGSL). Lagarde and de Rousiers 2014, "Moving Frostbite to
// PBR", with GTAO's multi-bounce on the base lobe's F0 (Jimenez et al.
// 2016), for a lobe of perceptual `roughness` seen at cosine `nv` to its
// normal.
fn specular_occlusion(nv:f32,roughness:f32,coat:bool,visibility:f32,f0:vec3<f32>)->vec3<f32> {
 let alpha=roughness*roughness;
 let ao=clamp(pow(nv+visibility,exp2(-16.*alpha-1.))-1.+visibility,0.,1.);
 if coat {
  return vec3(ao);
 }
 let a=2.0404*f0-vec3(.3324);
 let b=-4.7951*f0+vec3(.6417);
 let c=2.7552*f0+vec3(.6903);
 return max(vec3(ao),((ao*a+b)*ao+c)*ao);
}
// A lobe's environment specular, its probes' share `probes` and the sky's
// `sky`, occluded (specular_occlusion): the probes' by the receiver's
// `visibility`, the sky's by that times the irradiance volume's sky
// visibility a(n), `sky_visibility` (irradiance_volume.wgsl), as
// Frostbite's sky visibility and Unreal's baked sky occlusion occlude their
// sky light and not reflection captures. Full visibility occludes nothing:
// the multi-bounce fit would raise a white metal's by 1e-4.
fn occlusion_environment(lobe:SpecularLobe,coat:bool,probes:vec3<f32>,sky:vec3<f32>,visibility:f32,sky_visibility:f32,f0:vec3<f32>)->vec3<f32> {
 var occlusion=vec3(1.);
 if visibility<1. {
  occlusion=specular_occlusion(lobe.nv,lobe.roughness,coat,visibility,f0);
 }
 var sky_occlusion=occlusion;
 if sky_visibility<1. {
  sky_occlusion=specular_occlusion(lobe.nv,lobe.roughness,coat,visibility*sky_visibility,f0);
 }
 return probes*occlusion+sky*sky_occlusion;
}
// The share of the specular multiple scattering indirect light carries that
// a receiver's `visibility` lets through: its base lobe's specular occlusion
// (specular_occlusion, at cosine `nv` and perceptual `roughness`, on F0
// `f0`), as Filament ef1a133 occludes its energy-compensated specular by
// its specular occlusion and its multi-bounce (surface_light_indirect.fs
// evaluateIBL 758-759, 782-785, 812). Full visibility occludes nothing, as
// occlusion_environment.
fn occlusion_multiscatter(nv:f32,roughness:f32,visibility:f32,f0:vec3<f32>)->vec3<f32> {
 if visibility>=1. {
  return vec3(1.);
 }
 return specular_occlusion(nv,roughness,false,visibility,f0);
}
