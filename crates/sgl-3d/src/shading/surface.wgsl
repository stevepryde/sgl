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
 // The mapped base normal, and the geometry normal: the interpolated vertex
 // normal toward the side shaded, which the coat follows and along which a
 // shadow lookup offsets the receiver, never the mapped normal (normal or
 // bump map, decals, scrolling layers), so none of them moves a shadow.
 // Filament ef1a133 offsets its spot and cascade shadows along this normal,
 // flipped to the side shaded (getWorldGeometricNormalVector(),
 // shading_geometricNormal in shaders/src/surface_shading_parameters.fs and
 // surface_getters.fs); Bevy 9d12036 its point, spot and directional
 // shadows along the geometric normal too, `in.world_normal`
 // (crates/bevy_pbr/src/render/pbr_functions.wesl, apply_pbr_lighting),
 // which it flips only without tangents or a normal map.
 normal:vec3<f32>,
 geometry_normal:vec3<f32>,
 base:vec4<f32>,
 metallic:f32,
 // The dielectric reflectance at normal incidence
 // (material_dielectric_f0), which metallic mixes toward the base
 // (surface_f0).
 dielectric_f0:vec3<f32>,
 // Perceptual roughness, already filtered or clamped by the builder.
 roughness:f32,
 coat:f32,
 coat_roughness:f32,
 anisotropy:vec4<f32>,
 emission:vec3<f32>,
 environment_scale:f32,
 // The share of ambient light its material lets reach it
 // (material_occlusion; occlusion.wgsl).
 occlusion:f32,
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
 // The shadow receiver the surface is (SHADOW_RECEIVER_*): the frame's
 // camera sees it, which selects its directional shadow cascade by view
 // depth; a probe capture or world-space ray hit, which selects the first
 // cascade that holds it; or a dynamic GI probe ray's hit, which shade_lit
 // shades for its diffuse light alone and lights with no light of its
 // own: its caller adds the one light it draws (surface_ray.wgsl).
 receiver:u32,
 // Whether shade_lit adds the surface's environment specular
 // (probe_environment). The main view does not: source completion adds it
 // from the G-buffer. Probe captures and ray hits run no source completion,
 // so they add it from what completion uses.
 environment_specular:bool,
 // Whether shade_lit occludes the surface's ambient diffuse and environment
 // specular by its material's occlusion (Surface.occlusion): every view but
 // the camera's opaque surfaces, whose source completion occludes them by
 // the lesser of it and the frame's ambient occlusion (occlusion.wgsl).
 material_occlusion:bool,
 // The lights and decals that reach the surface: the cluster that holds
 // it (clusters.wgsl), looked up once for its decals and its lights.
 clusters:ClusterRange,
 // What the frame's screen-space method returned for the surface's traced
 // lobe, which shade_lit composes in place of that lobe's environment
 // specular: a blended receiver's where it is the surface at its pixel,
 // else untraced_reflection().
 traced:TracedReflection,
}
// A shaded surface's outgoing radiance, the ambient diffuse within it
// (environment diffuse and hemisphere fill, or a volume's irradiance in
// their place, not multiscattering) before any occlusion, and the
// irradiance volume's sky visibility a(n) at it, 1 where the volume does not
// light it. Source completion occludes the main view's ambient diffuse by
// its visibility and its sky specular by a(n) too (shading/gbuffer.wgsl,
// occlusion.wgsl); every other view's radiance holds its ambient diffuse
// occluded by its material's occlusion alone (ShadeContext.material_occlusion).
struct Shaded {
 color:vec3<f32>,
 ambient:vec3<f32>,
 sky_visibility:f32,
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
 return Shaded(s.base.rgb+s.emission,vec3(0.),1.);
}
// The environment specular source completion adds at runtime, for views
// without it (probe captures and ray hits): the installed baked probes, then
// the reflection sky (Frame.reflection_yaw and reflection_intensity) beyond
// them. A second capture pass then sees the first pass's probes, as Unity's
// reflection bounces and Frostbite's iterative probe relighting bake
// interreflection; a ray hit ends its path in them, as Unreal's and HDRP's
// ray-traced reflections take reflection probes at the last bounce. The
// probes' and the sky's shares come apart (EnvironmentSpecular), for the
// irradiance volume's sky visibility occludes the sky's alone.
fn probe_environment(world:vec3<f32>,direction:vec3<f32>,rough:f32)->EnvironmentSpecular {
 let rotation=frame.reflection_yaw;
 let strength=frame.reflection_intensity;
 return collection_environment(world,direction,rough,1.,environment_map,environment_sampler,rotation,strength);
}
// A surface's specular reflectance at normal incidence: its dielectric F0
// mixed toward its base by metallic, as three.js 0.185.1's
// specularColorBlended and KHR_materials_specular's F0. The G-buffer records
// it (view/geometry.wgsl).
fn surface_f0(surface:Surface)->vec3<f32> {
 return mix(surface.dielectric_f0,surface.base.rgb,surface.metallic);
}
// What shade_lit derives once per surface for its direct lights, as
// Filament's PixelParams: the diffuse colour, the specular reflectance at
// normal and grazing incidence (surface_f0, and pbr_f90 of it), the DFG
// lookup at the view, and the coat's Fresnel toward the view, weighted by
// the coat (pbr_coat_fresnel), which also attenuates shade_lit's ambient,
// environment, baked and emitted light.
struct SurfaceReflectance {
 diffuse:vec3<f32>,
 f0:vec3<f32>,
 f90:f32,
 view_dfg:vec2<f32>,
 coat_fresnel:f32,
}
// `view_dfg` is the caller's surface_dfg lookup at the surface's N.V, which
// its environment terms also use.
fn surface_reflectance(surface:Surface,view_dfg:vec2<f32>)->SurfaceReflectance {
 let f0=surface_f0(surface);
 let diffuse=surface.base.rgb*(1.-surface.metallic);
 let coat_fresnel=pbr_coat_fresnel(surface.geometry_normal,surface.view,surface.coat);
 return SurfaceReflectance(diffuse,f0,pbr_f90(f0),view_dfg,coat_fresnel);
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
  let f90=reflectance.f90;
  let view_dfg=reflectance.view_dfg;
  let view_missing=1.-view_dfg.x-view_dfg.y;
  let light_missing=1.-light_dfg.x-light_dfg.y;
  let average_fresnel=f0+(vec3(1.)-f0)*.047619;
  let scattered=pbr_three_single_scatter(f0,f90,view_dfg)*pbr_three_single_scatter(f0,f90,light_dfg)*average_fresnel;
  let denominator=vec3(1.)-view_missing*light_missing*average_fresnel*average_fresnel+vec3(.000001);
  let multiple_scattering=scattered/denominator*view_missing*light_missing;
  let base_specular=pbr_anisotropic_specular(surface.normal,surface.view,light_direction,surface.roughness,f0,f90,surface.anisotropy);
  base=base+base_specular*specular+multiple_scattering*specular;
 }
 if surface.coat<=0. {
  return base*cosine;
 }
 var coat=vec3(0.);
 if specular>0. {
  let coat_cosine=clamp(dot(surface.geometry_normal,light_direction),0.,1.);
  let coat_specular=pbr_three_specular(surface.geometry_normal,surface.view,light_direction,surface.coat_roughness,vec3(.04),1.);
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
  let n=select(surface.normal,surface.geometry_normal,coat_lobe);
  var inverse=mat3x3(vec3(1.,0.,0.),vec3(0.,1.,0.),vec3(0.,0.,1.));
  var weight=reflectance.diffuse;
  if lobe>0 {
   let fit=rect_light_fit(select(surface.roughness,surface.coat_roughness,coat_lobe),max(dot(n,surface.view),0.));
   inverse=fit.inverse;
   weight=rect_light_specular_weight(fit,select(reflectance.f0,vec3(.04),coat_lobe),select(reflectance.f90,1.,coat_lobe))*specular;
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
// Directional light `index` (Frame.directional_lights) as it reaches a
// surface at `position` with `geometry_normal` (Surface), shadowed when it
// has the frame's shadow cascades.
fn directional_light_sample(index:u32,position:vec3<f32>,geometry_normal:vec3<f32>,context:ShadeContext)->LightSample {
 let l=normalize(frame.directional_lights[index].direction_to_light);
 let radiance=frame.directional_lights[index].color*frame.directional_lights[index].illuminance;
 let shadow=directional_light_shadow(index,position,geometry_normal,context.pixel,context.receiver);
 return LightSample(l,radiance,shadow,1.,NO_RECT_LIGHT);
}
// A receiver's indirect diffuse light along `normal`, by the one
// determination: its lightmap or irradiance atlas chart
// (baked_diffuse_source), else the irradiance volume where it lights the
// frame and reaches the receiver (irradiance_volume_light), else the dynamic
// GI volume where it lights the frame, reaches the receiver and has a
// blended, active probe about it (dynamic_gi_irradiance; a moving receiver
// also weighs dormant probes, those with no surface in their cell; for a
// probe ray's hit, wherever it reaches the hit, its light zero where no
// probe about the hit weighs), else a moving instance's ambient cube, else
// the frame's ambient alone. Each volume takes its share
// and leaves the rest to what follows it, so a receiver hands over at its
// border without a seam. A chart takes all of it, as no volume lights a
// charted receiver.
struct IndirectDiffuse {
 // The chart's irradiance / PI, or the cube's times the share the volumes
 // leave it.
 baked:vec3<f32>,
 // The irradiance volume's own light rgb(n) times its share.
 field:vec3<f32>,
 // The dynamic GI volume's irradiance / PI in rgb, and in a its share: of
 // what the irradiance volume leaves.
 dynamic_gi:vec4<f32>,
 // The share of the frame's ambient the receiver takes: a(n) of the
 // irradiance volume's share, and whatever neither volume takes.
 ambient:f32,
 // The irradiance volume's sky visibility a(n), 1 beyond its share.
 sky_visibility:f32,
}
fn surface_indirect_diffuse(s:Surface,normal:vec3<f32>,probe_hit:bool)->IndirectDiffuse {
 let source=baked_diffuse_source(s.baked,s.lightmap_uv,s.moving);
 let baked=surface_fixed_irradiance(s.baked,s.uv,s.lightmap_uv,s.lightmap_bounds,normal,s.front,s.moving,s.baked_irradiance);
 var indirect=IndirectDiffuse(baked,vec3(0.),vec4(0.),1.,1.);
 if source==BAKED_LIGHTMAP || source==BAKED_ATLAS {
  return indirect;
 }
 // The irradiance volume steps half a cell along the geometry normal.
 let field=irradiance_volume_light(s.position,s.geometry_normal,normal);
 let rest=1.-field.share;
 var dynamic_gi=vec4(0.);
 if rest>0. {
  dynamic_gi=dynamic_gi_irradiance(s.position,normal,s.view,probe_hit,s.moving);
  dynamic_gi.a*=rest;
 }
 let fallback=rest-dynamic_gi.a;
 indirect.baked=baked*fallback;
 indirect.field=field.light*field.share;
 indirect.dynamic_gi=dynamic_gi;
 indirect.ambient=field.sky_visibility*field.share+fallback;
 indirect.sky_visibility=mix(1.,field.sky_visibility,field.share);
 return indirect;
}
// The share of the dynamic GI volume's last frame a probe hit reflects
// again, so the bounces it carries converge: Wicked's energy_conservation
// (95e357f ddgi_raytraceCS.hlsl 270–275, MIT, src/LICENSE-wicked.txt). The
// volume holds irradiance / PI, the radiance a hit reflects per unit of its
// diffuse colour, so nothing else scales it; df44c3d's further division by
// PI (492–498) dims every bounce by a further PI and is not taken. A hit
// takes the irradiance volume, the game's own field, whole.
const DYNAMIC_GI_BOUNCE:f32=.95;
fn shade_lit(s:Surface,context:ShadeContext)->Shaded {
 let base=s.base;
 let metallic=s.metallic;
 let n=s.normal;
 let coat_n=s.geometry_normal;
 let rough=s.roughness;
 let coat=s.coat;
 let coat_rough=s.coat_roughness;
 let emission=s.emission;
 let v=s.view;
 let dfg=surface_dfg(specular_nv(n,v),rough);
 let reflectance=surface_reflectance(s,dfg);
 let f0=reflectance.f0;
 let diffuse=reflectance.diffuse;
 let probe_hit=context.receiver==SHADOW_RECEIVER_PROBE_HIT;
 // What the material's occlusion lets reach the surface, where this view
 // occludes by it (ShadeContext.material_occlusion).
 let visibility=select(1.,s.occlusion,context.material_occlusion);
 // The retained cosine convolution stores irradiance / PI. It is already
 // a lighting integral, so neither a second PI nor a brightness fudge belongs here.
 let ibl=pbr_ibl_weights(base.rgb,metallic,s.dielectric_f0,reflectance.f90,dfg);
 // A probe hit takes diffuse light alone: no multiscattered specular.
 let multi=select(ibl.multi,vec3(0.),probe_hit);
 let indirect=surface_indirect_diffuse(s,n,probe_hit);
 let fallback=indirect.ambient*(1.-reflectance.coat_fresnel);
 let environment=diffuse_environment(n)*s.environment_scale*fallback;
 var color=(ibl.diffuse+multi)*environment;
 let sky=frame.hemisphere_sky_color;
 let hemisphere_intensity=frame.hemisphere_intensity;
 let ground=frame.hemisphere_ground_color;
 let hemisphere=diffuse/3.14159265359*pbr_hemisphere(n,sky,ground,hemisphere_intensity)*fallback;
 color+=hemisphere;
 // The ambient diffuse that ambient occlusion weights: the diffuse
 // environment and hemisphere terms, or the volumes' irradiance in their
 // place; multiscattering stays apart from it.
 var ambient=ibl.diffuse*environment+hemisphere;
 // A probe hit takes the dynamic GI volume's last frame damped, as Wicked's
 // bounce is. Neither volume's irradiance takes environment_scale.
 let dynamic_gi=indirect.dynamic_gi;
 let bounce=select(1.,DYNAMIC_GI_BOUNCE,probe_hit);
 let irradiance=(indirect.field+dynamic_gi.rgb*dynamic_gi.a*bounce)*(1.-reflectance.coat_fresnel);
 color+=(ibl.diffuse+multi)*irradiance;
 ambient+=ibl.diffuse*irradiance;
 // Baked diffuse lies beneath the coat as live light does: Three.js 0.185.1's
 // node path adds a light map to the irradiance its finish dims
 // (NodeMaterial.setupLightMap, PhysicalLightingModel.finish), Godot b130438
 // adds lightmaps to the ambient light its clearcoat then attenuates
 // (scene_forward_clustered.glsl), and Filament ef1a133 dims all indirect
 // diffuse (surface_light_indirect.fs evaluateClearCoatIBL). Bevy 9d12036
 // adds its lightmap undimmed (pbr_functions.wesl).
 color+=indirect.baked*diffuse*(vec3(1.)-f0)*(1.-reflectance.coat_fresnel);
 if !probe_hit {
  if frame.directional_lights[0].illuminance>0. {
   color+=surface_direct_light(s,reflectance,directional_light_sample(0u,s.position,s.geometry_normal,context));
  }
  if frame.directional_lights[1].illuminance>0. {
   color+=surface_direct_light(s,reflectance,directional_light_sample(1u,s.position,s.geometry_normal,context));
  }
  // The scene lights that reach the surface: live ones, then baked ones
  // where no baked map already holds their light.
  let lights=context.clusters;
  var end=lights.first+lights.live;
  if takes_baked_lights(s.baked,s.lightmap_uv,s.moving) {
   end+=lights.baked;
  }
  for (var at=lights.first;at<end;at++) {
   let light=scene_light_sample(cluster_item(at),s.position,n,s.geometry_normal,context.pixel,context.receiver);
   if light.visibility>0. {
    color+=surface_direct_light(s,reflectance,light);
   }
  }
 }
 if context.environment_specular {
  let lobes=specular_lobes(n,coat_n,v,f0,rough,dfg,coat,coat_rough,s.anisotropy,lookup_tables,environment_sampler);
  let traced=context.traced;
  for (var lobe=SPECULAR_BASE;lobe<=SPECULAR_COAT;lobe++) {
   if lobe==SPECULAR_COAT && coat<=0. {
    continue;
   }
   // The material's occlusion occludes the probes' and the sky's shares,
   // the irradiance volume's sky visibility the sky's alone
   // (occlusion_environment).
   let resolved=probe_environment(s.position,lobes[lobe].direction,lobes[lobe].roughness);
   let occluded=occlusion_environment(lobes[lobe],lobe==SPECULAR_COAT,resolved.probes,resolved.sky,visibility,indirect.sky_visibility,f0);
   let environment=occluded*s.environment_scale;
   if lobe==specular_traced_lobe(coat) && specular_traces(lobes[lobe].roughness,traced.cutoff*traced.cutoff) {
    let fade=specular_trace_fade(lobes[lobe].roughness,traced.cutoff,traced.fade);
    color+=specular_traced(lobes[lobe],traced.reflected,fade,environment);
   } else {
    color+=lobes[lobe].response*environment;
   }
  }
 }
 // The coat lies over emission too. KHR_materials_clearcoat, SGL3D's
 // material definition, layers the coat over the base "including emission"
 // and darkens emission by its Fresnel (Khronos glTF 8e69120,
 // KHR_materials_clearcoat/README.md, Clearcoat and Implementation:
 // Emission), as Bevy 9d12036 (pbr_functions.wesl) and Three.js 0.185.1's
 // WebGL meshphysical shader do. Three.js's node path
 // (NodeMaterial.setupLighting), Filament ef1a133 (surface_shading_lit.fs
 // evaluateMaterial) and Godot b130438 (scene_forward_clustered.glsl) add
 // emission after the coat instead.
 color+=emission*(1.-reflectance.coat_fresnel);
 if visibility<1. {
  color=occlusion_diffuse(color,ambient,visibility);
 }
 return Shaded(color,ambient,indirect.sky_visibility);
}
