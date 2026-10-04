// One evaluated surface and the shading every view gives it: rasterized
// fragments (main view and probe captures, surface_raster.wgsl) and
// world-space ray hits (surface_ray.wgsl). The builders evaluate materials;
// direct, baked, environment and emitted light are computed here. Reads the
// lit bindings (bind_lit.wgsl).
// Diagnostic layer switches of surface shading, set per pipeline; always true
// outside diagnostics.
override instance_emission_enabled:bool=true;
override normal_maps_enabled:bool=true;
override bump_maps_enabled:bool=true;
struct Surface {
 position:vec3<f32>,
 // Unit direction toward the viewer: the camera for a fragment, back along
 // the ray for a hit.
 view:vec3<f32>,
 // The mapped base normal, and the geometry normal the coat follows.
 normal:vec3<f32>,
 coat_normal:vec3<f32>,
 base:vec4<f32>,
 metallic:f32,
 // Perceptual roughness, already filtered or clamped by the builder.
 roughness:f32,
 coat:f32,
 coat_roughness:f32,
 anisotropy:vec4<f32>,
 emission:vec3<f32>,
 environment_scale:f32,
 unlit:bool,
 front:bool,
 // A moving instance's: it takes its ambient cube, not baked charts.
 moving:bool,
 // Its material is lightmapped (`Scene::set_lightmap`).
 baked:bool,
 uv:vec2<f32>,
 lightmap_uv:vec2<f32>,
 lightmap_bounds:vec4<f32>,
 baked_irradiance:array<vec4<f32>,6>,
}
// What the view supplies beyond the surface.
struct ShadeContext {
 // The view's pixel: it seeds the directional shadow filter's rotation.
 pixel:vec2<f32>,
 // Whether the frame's camera sees the surface, which selects its
 // directional shadow cascade by view depth; probe captures and ray hits
 // select the first cascade that holds the surface.
 camera:bool,
 // Whether shade_lit adds the surface's environment specular
 // (probe_environment). The main view does not: source completion adds it
 // from the G-buffer. Probe captures and ray hits run no source completion,
 // so they add it from what completion uses.
 environment_specular:bool,
 // The lights and decals that reach the surface: the cluster that holds
 // it (clusters.wgsl), looked up once for its decals and its lights.
 clusters:ClusterRange,
}
// A shaded surface's outgoing radiance, and the ambient diffuse within it
// (environment diffuse and hemisphere fill, not multiscattering) before
// occlusion. Source completion occludes the main view's ambient diffuse by
// its ambient visibility (shading/gbuffer.wgsl); probe captures and ray hits
// keep it whole.
struct Shaded {
 color:vec3<f32>,
 ambient:vec3<f32>,
}
// A surface described by its base color and emission alone.
fn unlit_surface(base:vec4<f32>,emission:vec3<f32>)->Surface {
 var s:Surface;
 s.base=base;
 s.emission=emission;
 s.unlit=true;
 return s;
}
fn shade_unlit(s:Surface)->Shaded {
 return Shaded(s.base.rgb+s.emission,vec3(0.));
}
// The environment specular source completion adds at runtime, for views
// without it (probe captures and ray hits): the installed baked probes, then
// the reflection sky (Frame.reflection_yaw and reflection_intensity) beyond
// them. A second capture pass then sees the first pass's probes, as Unity's
// reflection bounces and Frostbite's iterative probe relighting bake
// interreflection; a ray hit ends its path in them, as Unreal's and HDRP's
// ray-traced reflections take reflection probes at the last bounce.
fn probe_environment(world:vec3<f32>,direction:vec3<f32>,rough:f32)->vec3<f32> {
 let rotation=frame.reflection_yaw;
 let strength=frame.reflection_intensity;
 return collection_environment(world,direction,rough,1.,environment_map,environment_sampler,rotation,strength);
}
// What shade_lit derives once per surface for its direct lights, as
// Filament's PixelParams: the diffuse colour, the specular reflectance at
// normal incidence, the DFG lookup at the view, and the coat's Fresnel toward
// the view, weighted by the coat (pbr_coat_fresnel), which also attenuates
// shade_lit's ambient, environment and emitted light.
struct SurfaceReflectance {
 diffuse:vec3<f32>,
 f0:vec3<f32>,
 view_dfg:vec2<f32>,
 coat_fresnel:f32,
}
// `view_dfg` is the caller's surface_dfg lookup at the surface's N.V, which
// its environment terms also use.
fn surface_reflectance(surface:Surface,view_dfg:vec2<f32>)->SurfaceReflectance {
 let f0=mix(vec3(0.04),surface.base.rgb,surface.metallic);
 let diffuse=surface.base.rgb*(1.-surface.metallic);
 let coat_fresnel=pbr_coat_fresnel(surface.coat_normal,surface.view,surface.coat);
 return SurfaceReflectance(diffuse,f0,view_dfg,coat_fresnel);
}
// The light one sample brings to a surface, as Filament's
// surfaceShading(PixelParams, Light); a rectangle's integrated over its face
// (surface_rect_light).
fn surface_direct_light(surface:Surface,reflectance:SurfaceReflectance,light:LightSample)->vec3<f32> {
 if light.rect!=NO_RECT_LIGHT {
  return surface_rect_light(surface,reflectance,lights[light.rect],light.specular)*light.radiance*light.visibility;
 }
 // Only the specular lobes read the light direction's DFG lookup.
 var light_dfg=vec2(0.);
 if light.specular>0. {
  let light_cosine=max(dot(surface.normal,light.direction),0.);
  light_dfg=surface_dfg(light_cosine,surface.roughness);
 }
 return surface_direct_brdf(surface,reflectance,light.direction,light_dfg,light.specular)*light.radiance*light.visibility;
}
// surface_direct_light's BRDF times the cosine at the light, for the light
// direction's DFG lookup. Three.js 0.185.1 PhysicalLightingModel.direct:
// BRDF_Lambert, BRDF_GGX_Multiscatter over the view and light DFG lookups,
// and the clearcoat lobe, layered over the base as its finish does. The base
// specular lobe is pbr_anisotropic_specular. `specular` scales the specular
// lobes, base and coat, as Godot's light_compute applies light_specular; a
// diffuse-only light (0) evaluates neither.
fn surface_direct_brdf(surface:Surface,reflectance:SurfaceReflectance,light_direction:vec3<f32>,light_dfg:vec2<f32>,specular:f32)->vec3<f32> {
 let cosine=clamp(dot(surface.normal,light_direction),0.,1.);
 var base=reflectance.diffuse/3.14159265359;
 if specular>0. {
  let f0=reflectance.f0;
  let view_dfg=reflectance.view_dfg;
  let view_missing=1.-view_dfg.x-view_dfg.y;
  let light_missing=1.-light_dfg.x-light_dfg.y;
  let average_fresnel=f0+(vec3(1.)-f0)*.047619;
  let scattered=pbr_three_single_scatter(f0,view_dfg)*pbr_three_single_scatter(f0,light_dfg)*average_fresnel;
  let denominator=vec3(1.)-view_missing*light_missing*average_fresnel*average_fresnel+vec3(.000001);
  let multiple_scattering=scattered/denominator*view_missing*light_missing;
  let base_specular=pbr_anisotropic_specular(surface.normal,surface.view,light_direction,surface.roughness,f0,surface.anisotropy);
  base=base+base_specular*specular+multiple_scattering*specular;
 }
 if surface.coat<=0. {
  return base*cosine;
 }
 var coat=vec3(0.);
 if specular>0. {
  let coat_cosine=clamp(dot(surface.coat_normal,light_direction),0.,1.);
  let coat_specular=pbr_three_specular(surface.coat_normal,surface.view,light_direction,surface.coat_roughness,vec3(.04));
  coat=surface.coat*coat_specular*coat_cosine*specular;
 }
 return base*(1.-reflectance.coat_fresnel)*cosine+coat;
}
// What rectangle `rect`'s face brings to a surface per unit of its luminance:
// Bevy 9d12036's rect_light (crates/bevy_pbr/src/render/pbr_lighting.wesl,
// MIT OR Apache-2.0, src/LICENSE-bevy.txt), its lobes layered as
// surface_direct_brdf layers a punctual light's. Lambertian diffuse is the
// face's form factor; the base GGX lobe and the coat's integrate the fit at
// their roughness and N.V (rect_light.wgsl), weighted by its magnitude and
// Fresnel, without the multiscattering a punctual light adds, as Bevy's.
// `specular` scales the base and coat lobes, and a diffuse-only light (0)
// evaluates neither; the coat's Fresnel toward the view takes from the base.
fn surface_rect_light(surface:Surface,reflectance:SurfaceReflectance,rect:Light,specular:f32)->vec3<f32> {
 let center=rect.position-surface.position;
 let half_height=light_rect_half_height(rect);
 // Lobes in turn: diffuse, then with specular the base GGX lobe, then a
 // coat's.
 var lobes=1;
 if specular>0. {
  lobes=select(2,3,surface.coat>0.);
 }
 var base=vec3(0.);
 var coat=vec3(0.);
 for (var lobe=0;lobe<lobes;lobe++) {
  let coat_lobe=lobe==2;
  let n=select(surface.normal,surface.coat_normal,coat_lobe);
  var inverse=mat3x3(vec3(1.,0.,0.),vec3(0.,1.,0.),vec3(0.,0.,1.));
  var weight=reflectance.diffuse;
  if lobe>0 {
   let fit=rect_light_fit(select(surface.roughness,surface.coat_roughness,coat_lobe),max(dot(n,surface.view),0.));
   inverse=fit.inverse;
   weight=rect_light_specular_weight(fit,select(reflectance.f0,vec3(.04),coat_lobe))*specular;
  }
  let value=weight*ltc_integrate_quad(rect_light_frame(n,surface.view,center,rect.half_width,half_height),inverse);
  if coat_lobe {
   coat=surface.coat*value;
  } else {
   base+=value;
  }
 }
 if surface.coat<=0. {
  return base;
 }
 return base*(1.-reflectance.coat_fresnel)+coat;
}
// The shadow receiver a shaded surface is (SHADOW_RECEIVER_*).
fn shade_receiver(context:ShadeContext)->u32 {
 return select(SHADOW_RECEIVER_CAPTURE,SHADOW_RECEIVER_CAMERA,context.camera);
}
// Directional light `index` (Frame.directional_lights) as it reaches a
// surface at `position` with `normal`, shadowed when it has the frame's
// shadow cascades.
fn directional_light_sample(index:u32,position:vec3<f32>,normal:vec3<f32>,context:ShadeContext)->LightSample {
 let l=normalize(frame.directional_lights[index].direction_to_light);
 let radiance=frame.directional_lights[index].color*frame.directional_lights[index].illuminance;
 var shadow=1.;
 if (frame.directional_lights[index].flags&DIRECTIONAL_LIGHT_SHADOW)!=0u {
  shadow=directional_shadow_visibility(index,position,normal,context.pixel,shade_receiver(context));
 }
 return LightSample(l,radiance,shadow,1.,NO_RECT_LIGHT);
}
fn shade_lit(s:Surface,context:ShadeContext)->Shaded {
 let base=s.base;
 let metallic=s.metallic;
 let n=s.normal;
 let coat_n=s.coat_normal;
 let rough=s.roughness;
 let coat=s.coat;
 let coat_rough=s.coat_roughness;
 let emission=s.emission;
 let v=s.view;
 let dfg=surface_dfg(specular_nv(n,v),rough);
 let reflectance=surface_reflectance(s,dfg);
 let f0=reflectance.f0;
 let diffuse=reflectance.diffuse;
 // The retained cosine convolution stores irradiance / PI. It is already
 // a lighting integral, so neither a second PI nor a brightness fudge belongs here.
 let ibl=pbr_ibl_weights(base.rgb,metallic,dfg);
 var color=(ibl.diffuse+ibl.multi)*diffuse_environment(n)*s.environment_scale*(1.-reflectance.coat_fresnel);
 let sky=frame.hemisphere_sky_color;
 let hemisphere_intensity=frame.hemisphere_intensity;
 let ground=frame.hemisphere_ground_color;
 let hemisphere=diffuse/3.14159265359*pbr_hemisphere(n,sky,ground,hemisphere_intensity)*(1.-reflectance.coat_fresnel);
 color+=hemisphere;
 // The ambient diffuse that ambient occlusion weights: the diffuse
 // environment and hemisphere terms; multiscattering stays apart from it.
 let ambient=ibl.diffuse*diffuse_environment(n)*s.environment_scale*(1.-reflectance.coat_fresnel)+hemisphere;
 if frame.directional_lights[0].illuminance>0. {
  color+=surface_direct_light(s,reflectance,directional_light_sample(0u,s.position,n,context));
 }
 if frame.directional_lights[1].illuminance>0. {
  color+=surface_direct_light(s,reflectance,directional_light_sample(1u,s.position,n,context));
 }
 color+=surface_fixed_irradiance(s.baked,s.uv,s.lightmap_uv,s.lightmap_bounds,n,s.front,s.moving,s.baked_irradiance)*diffuse*(vec3(1.)-f0);
 // The scene lights that reach the surface: live ones, then baked ones where
 // no baked map already holds their light.
 let lights=context.clusters;
 var end=lights.first+lights.live;
 if takes_baked_lights(s.baked,s.lightmap_uv,s.moving) {
  end+=lights.baked;
 }
 for (var at=lights.first;at<end;at++) {
  let light=scene_light_sample(cluster_item(at),s.position,n,context.pixel,shade_receiver(context));
  if light.visibility>0. {
   color+=surface_direct_light(s,reflectance,light);
  }
 }
 if context.environment_specular {
  let lobes=specular_lobes(n,coat_n,v,f0,rough,dfg,coat,coat_rough,s.anisotropy,lookup_tables,environment_sampler);
  for (var lobe=SPECULAR_BASE;lobe<=SPECULAR_COAT;lobe++) {
   if lobe==SPECULAR_COAT && coat<=0. {
    continue;
   }
   let environment=probe_environment(s.position,lobes[lobe].direction,lobes[lobe].roughness)*s.environment_scale;
   color+=lobes[lobe].response*environment;
  }
 }
 color+=emission*(1.-reflectance.coat_fresnel);
 return Shaded(color,ambient);
}
