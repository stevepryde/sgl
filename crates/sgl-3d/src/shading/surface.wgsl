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
// Whether the scene holds an iridescent film (LitConstants::films): where
// it does not, surface_f0s compiles its evaluation out.
override films_enabled:bool=true;
struct Surface {
 position:vec3<f32>,
 // Unit direction toward the viewer: the camera for a fragment, back along
 // the ray for a hit.
 view:vec3<f32>,
 // The mapped base normal, which the base lobes follow; the geometry
 // normal: the interpolated vertex normal toward the side shaded, along
 // which a shadow lookup offsets the receiver, never a mapped normal
 // (normal or bump map, the coat's normal map, decals, scrolling layers),
 // so none of them moves a shadow map's lookup; and the coat normal, which
 // the coat follows: its clearcoat normal map's on the base map's frame,
 // else the geometry normal (KHR_materials_clearcoat).
 // Filament ef1a133 offsets its spot and cascade shadows along this normal,
 // flipped to the side shaded (getWorldGeometricNormalVector(),
 // shading_geometricNormal in shaders/src/surface_shading_parameters.fs and
 // surface_getters.fs); Bevy 9d12036 its point, spot and directional
 // shadows along the geometric normal too, `in.world_normal`
 // (crates/bevy_pbr/src/render/pbr_functions.wesl, apply_pbr_lighting),
 // which it flips only without tangents or a normal map.
 normal:vec3<f32>,
 geometry_normal:vec3<f32>,
 coat_normal:vec3<f32>,
 base:vec4<f32>,
 metallic:f32,
 // The dielectric reflectance at normal incidence
 // (material_dielectric_f0), which metallic mixes toward the base
 // (surface_f0), and at grazing incidence, the specular strength, which it
 // mixes toward 1 (surface_f90).
 dielectric_f0:vec3<f32>,
 specular:f32,
 // Perceptual roughness, already filtered or clamped by the builder.
 roughness:f32,
 coat:f32,
 coat_roughness:f32,
 // Its KHR_materials_iridescence film at the view (surface_film): none
 // until its builder evaluates one.
 film:SurfaceFilm,
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
 // Whether shade_lit occludes the surface's ambient light and environment
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
// A shaded surface's outgoing radiance; the ambient light within it before
// any occlusion, from the environment's diffuse light and the hemisphere
// fill, or a volume's irradiance in their place: its diffuse share
// (`ambient`) and the specular multiple scattering it carries (`multi`);
// and the irradiance volume's sky visibility a(n) at it, 1 where the volume
// does not light it. Source completion occludes the main view's ambient
// light by its visibility, the diffuse share linearly and the multiple
// scattering by its specular occlusion (occlusion_ambient), and its sky
// specular by a(n) too (shading/gbuffer.wgsl, occlusion.wgsl); every other
// view's radiance holds its ambient light occluded so by its material's
// occlusion alone (ShadeContext.material_occlusion).
struct Shaded {
 color:vec3<f32>,
 ambient:vec3<f32>,
 multi:vec3<f32>,
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
 return Shaded(s.base.rgb+s.emission,vec3(0.),vec3(0.),1.);
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
// A surface's KHR_materials_iridescence film at its view, which its
// builder evaluates once (surface_film): the film's Fresnel at the view's
// N.V (iridescence_fresnel) over the dielectric F0 (its specular strength
// included, as Filament ef1a133's iridescentF0 takes pixel.f0) and over the
// base, evaluated apart, as the Khronos glTF Sample Renderer (0686eb2
// source/Renderer/shaders/pbr.frag 158–159) and three.js r185 (2431a09
// PhysicalLightingModel.js 505–526) evaluate them, each refit to the F0
// whose Schlick curve toward its own F90, the specular strength and 1,
// passes through it there (iridescence_refit); and the share of diffuse
// light the film over the dielectric leaves, KHR's rgb_mix: 1 less its
// Fresnel's strongest channel (pbr.frag 385). Its strength is 0 where there
// is none, the zero a Surface starts with.
struct SurfaceFilm {
 strength:f32,
 dielectric:vec3<f32>,
 metal:vec3<f32>,
 diffuse:f32,
}
// The film of strength `strength`, IOR `ior` and `thickness` nanometres
// over `surface`, whose normal, view, base, metallic, dielectric F0 and
// specular strength are set. A film of no thickness is none, as the
// Sample Renderer takes it (pbr.frag 161–163). Each film is evaluated only
// where metallic leaves its F0 a share: a full metal reflects nothing of
// the dielectric's, nor diffuses, and a dielectric nothing of the metal's.
fn surface_film(surface:Surface,strength:f32,ior:f32,thickness:f32)->SurfaceFilm {
 var film=SurfaceFilm(0.,surface.dielectric_f0,surface.base.rgb,1.);
 if !films_enabled || strength<=0. || thickness<=0. {
  return film;
 }
 film.strength=strength;
 let nv=specular_nv(surface.normal,surface.view);
 if surface.metallic<1. {
  let dielectric=iridescence_fresnel(1.,ior,surface.dielectric_f0,thickness,nv);
  film.dielectric=iridescence_refit(dielectric,nv,surface.specular);
  film.diffuse=1.-max(dielectric.r,max(dielectric.g,dielectric.b));
 }
 if surface.metallic>0. {
  let metal=iridescence_fresnel(1.,ior,surface.base.rgb,thickness,nv);
  film.metal=iridescence_refit(metal,nv,1.);
 }
 return film;
}
// A surface's reflectance at normal incidence, its dielectric's and its
// metal's, which metallic mixes (surface_f0), each mixed toward its film's
// by the film's strength (Surface.film), as Filament's iridescentF0 mixes
// it; and the film's strength and the diffuse share it leaves.
struct SurfaceF0 {
 dielectric:vec3<f32>,
 metal:vec3<f32>,
 film:f32,
 film_diffuse:f32,
}
fn surface_f0s(surface:Surface)->SurfaceF0 {
 let film=surface.film;
 if film.strength<=0. {
  return SurfaceF0(surface.dielectric_f0,surface.base.rgb,0.,1.);
 }
 return SurfaceF0(mix(surface.dielectric_f0,film.dielectric,film.strength),mix(surface.base.rgb,film.metal,film.strength),film.strength,film.diffuse);
}
// A surface's specular reflectance at normal incidence: its dielectric F0
// mixed toward its base by metallic, as three.js 0.185.1's
// specularColorBlended and KHR_materials_specular's F0, each under its
// film (surface_f0s). The G-buffer records it (view/geometry.wgsl).
fn surface_f0(surface:Surface)->vec3<f32> {
 let f=surface_f0s(surface);
 return mix(f.dielectric,f.metal,surface.metallic);
}
// A surface's specular reflectance at grazing incidence (F90): its specular
// strength mixed toward 1 by metallic, as KHR_materials_specular defines it
// (dielectric_f90 = specular), Filament ef1a133's specular-factor path
// computes it (shaders/src/surface_shading_lit.fs, pixel.f90) and three.js
// 0.185.1 does (MeshPhysicalNodeMaterial.setupSpecular, specularF90). The
// G-buffer records it (view/geometry.wgsl).
fn surface_f90(surface:Surface)->f32 {
 return mix(surface.specular,1.,surface.metallic);
}
// What shade_lit derives once per surface for its direct lights, as
// Filament's PixelParams: the diffuse colour, the specular reflectance at
// normal incidence, its dielectric's and metal's (surface_f0s) and mixed
// (surface_f0), and at grazing incidence (surface_f90), the DFG
// lookup at the view, the gain that restores the base lobe's multiply
// scattered energy (pbr_multiscatter_gain, from that lookup), and the coat's
// Fresnel toward the view, weighted by the coat (pbr_coat_fresnel), which
// also attenuates shade_lit's ambient, environment, baked and emitted light.
struct SurfaceReflectance {
 diffuse:vec3<f32>,
 f0s:SurfaceF0,
 f0:vec3<f32>,
 f90:f32,
 view_dfg:vec2<f32>,
 multiscatter:vec3<f32>,
 coat_fresnel:f32,
}
// `view_dfg` is the caller's surface_dfg lookup at the surface's N.V, which
// its environment terms also use.
fn surface_reflectance(surface:Surface,view_dfg:vec2<f32>)->SurfaceReflectance {
 let f0s=surface_f0s(surface);
 let f0=mix(f0s.dielectric,f0s.metal,surface.metallic);
 let diffuse=surface.base.rgb*(1.-surface.metallic);
 let coat_fresnel=pbr_coat_fresnel(surface.coat_normal,surface.view,surface.coat);
 return SurfaceReflectance(diffuse,f0s,f0,surface_f90(surface),view_dfg,pbr_multiscatter_gain(f0,view_dfg),coat_fresnel);
}
// The weights every source of indirect irradiance takes and the
// environment's multiple scattering (pbr_ibl_weights), at the surface's
// dielectric and metal F0 under its film (surface_f0s), as three.js r185
// (2431a09 PhysicalLightingModel.js 769–784) takes the film's: its diffuse
// keeps what the dielectric does not scatter, channel by channel.
fn surface_ibl_weights(surface:Surface,reflectance:SurfaceReflectance)->PbrIblWeights {
 return pbr_ibl_weights(surface.base.rgb,surface.metallic,reflectance.f0s.dielectric,reflectance.f0s.metal,reflectance.f90,reflectance.view_dfg);
}
// The light one sample brings to a surface, as Filament's
// surfaceShading(PixelParams, Light); a rectangle's integrated over its face
// (surface_rect_light).
fn surface_direct_light(surface:Surface,reflectance:SurfaceReflectance,light:LightSample)->vec3<f32> {
 if light.rect!=NO_RECT_LIGHT {
  return surface_rect_light(surface,reflectance,lights[light.rect],light.direction,light.specular)*light.radiance*light.visibility;
 }
 return surface_direct_brdf(surface,reflectance,light.direction,light.size,light.specular)*light.radiance*light.visibility;
}
// The share of its diffuse light a dielectric keeps under a light toward
// `direction`: glTF 2.0's dielectric BRDF mixes the Lambertian base under
// the specular layer by the layer's Fresnel at V.H (Appendix B,
// fresnel_mix), as the Khronos glTF Sample Renderer shades each light
// (0686eb2 source/Renderer/shaders/pbr.frag 314, 381), at the dielectric's
// F0 and its F90, the specular strength (KHR_materials_specular); under a
// film, toward the film's share (SurfaceF0.film_diffuse) by its strength,
// as the Sample Renderer mixes rgb_mix in (pbr.frag 385). Filament, Bevy
// and three.js leave diffuse light whole (D-32).
fn surface_diffuse_coupling(surface:Surface,reflectance:SurfaceReflectance,direction:vec3<f32>)->vec3<f32> {
 let h=normalize(surface.view+direction);
 let layer=vec3(1.)-pbr_fresnel_schlick(clamp(dot(surface.view,h),0.,1.),surface.dielectric_f0,surface.specular);
 return mix(layer,vec3(reflectance.f0s.film_diffuse),reflectance.f0s.film);
}
// surface_direct_light's BRDF times the cosine, for a light toward
// `light_direction` of size `size` (LightSample.size): the Lambertian
// diffuse coupled to the specular (surface_diffuse_coupling), the base
// specular lobe (pbr_anisotropic_specular) with its multiple scattering
// (SurfaceReflectance.multiscatter), and the clearcoat lobe, layered over
// the base as KHR_materials_clearcoat and three.js's finish layer it. A
// sized light's specular lobes take its representative point
// (pbr_sized_light) and the cosine there; its diffuse light takes its
// centre. `specular` scales the specular lobes, base and coat, as Godot's
// light_compute applies light_specular; a diffuse-only light (0) evaluates
// neither.
fn surface_direct_brdf(surface:Surface,reflectance:SurfaceReflectance,light_direction:vec3<f32>,size:f32,specular:f32)->vec3<f32> {
 let cosine=clamp(dot(surface.normal,light_direction),0.,1.);
 var base=reflectance.diffuse/3.14159265359*surface_diffuse_coupling(surface,reflectance,light_direction)*cosine;
 if specular>0. {
  var reflected=reflect(-surface.view,surface.normal);
  if surface.anisotropy.w>0. {
   reflected=pbr_anisotropy_reflection(surface.normal,surface.view,surface.anisotropy,surface.roughness);
  }
  let sized=pbr_sized_light(light_direction,size,reflected,surface.view,surface.roughness);
  let lobe=pbr_anisotropic_specular(surface.normal,surface.view,sized.direction,surface.roughness,reflectance.f0,reflectance.f90,surface.anisotropy);
  let lobe_cosine=clamp(dot(surface.normal,sized.direction),0.,1.);
  base+=lobe*reflectance.multiscatter*sized.intensity*lobe_cosine*specular;
 }
 if surface.coat<=0. {
  return base;
 }
 var coat=vec3(0.);
 if specular>0. {
  let coat_normal=surface.coat_normal;
  let sized=pbr_sized_light(light_direction,size,reflect(-surface.view,coat_normal),surface.view,surface.coat_roughness);
  let coat_specular=pbr_ggx_specular(coat_normal,surface.view,sized.direction,surface.coat_roughness,vec3(.04),1.);
  let coat_cosine=clamp(dot(coat_normal,sized.direction),0.,1.);
  coat=surface.coat*coat_specular*sized.intensity*coat_cosine*specular;
 }
 return base*(1.-reflectance.coat_fresnel)+coat;
}
// What rectangle `rect`'s face brings to a surface per unit of its luminance:
// Bevy 9d12036's rect_light (crates/bevy_pbr/src/render/pbr_lighting.wesl,
// MIT OR Apache-2.0, src/LICENSE-bevy.txt), its lobes layered as
// surface_direct_brdf layers a punctual light's. Lambertian diffuse is the
// face's form factor, coupled to the specular toward the face's centre,
// `center_direction` (surface_diffuse_coupling); the base GGX lobe and the
// coat's integrate the fit at their roughness and N.V (rect_light.wgsl),
// weighted by its magnitude and Fresnel, the base with its multiple
// scattering as a punctual light's, where Bevy's takes none. `specular`
// scales the base and coat lobes, and a diffuse-only light (0) evaluates
// neither; the coat's Fresnel toward the view takes from the base.
fn surface_rect_light(surface:Surface,reflectance:SurfaceReflectance,rect:Light,center_direction:vec3<f32>,specular:f32)->vec3<f32> {
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
 let diffuse=reflectance.diffuse*surface_diffuse_coupling(surface,reflectance,center_direction);
 for (var lobe=0;lobe<lobes;lobe++) {
  let coat_lobe=lobe==2;
  let n=select(surface.normal,surface.coat_normal,coat_lobe);
  var inverse=mat3x3(vec3(1.,0.,0.),vec3(0.,1.,0.),vec3(0.,0.,1.));
  var weight=diffuse;
  if lobe>0 {
   let fit=rect_light_fit(select(surface.roughness,surface.coat_roughness,coat_lobe),max(dot(n,surface.view),0.));
   inverse=fit.inverse;
   let scattering=select(reflectance.multiscatter,vec3(1.),coat_lobe);
   weight=rect_light_specular_weight(fit,select(reflectance.f0,vec3(.04),coat_lobe),select(reflectance.f90,1.,coat_lobe))*scattering*specular;
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
 return LightSample(l,radiance,shadow,1.,NO_RECT_LIGHT,frame.directional_lights[index].disc_radius);
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
 let coat_n=s.coat_normal;
 let rough=s.roughness;
 let coat=s.coat;
 let coat_rough=s.coat_roughness;
 let emission=s.emission;
 let v=s.view;
 let dfg=surface_dfg(specular_nv(n,v),rough);
 let reflectance=surface_reflectance(s,dfg);
 let f0=reflectance.f0;
 let probe_hit=context.receiver==SHADOW_RECEIVER_PROBE_HIT;
 // What the material's occlusion lets reach the surface, where this view
 // occludes by it (ShadeContext.material_occlusion).
 let visibility=select(1.,s.occlusion,context.material_occlusion);
 // Every source of indirect irradiance lights the surface by one rule
 // (D-32): its irradiance / PI times the environment's diffuse weight and
 // multiple scattering (pbr_ibl_weights), three.js 0.185.1's
 // PhysicalLightingModel.indirect for its environment. The retained cosine
 // convolution, the hemisphere fill over PI, the volumes and the baked
 // charts and cubes all hold irradiance / PI, already a lighting integral,
 // so neither a second PI nor a brightness fudge belongs here.
 let ibl=surface_ibl_weights(s,reflectance);
 // A probe hit takes diffuse light alone: no multiscattered specular.
 let multi=select(ibl.multi,vec3(0.),probe_hit);
 let response=ibl.diffuse+multi;
 let indirect=surface_indirect_diffuse(s,n,probe_hit);
 let fallback=indirect.ambient*(1.-reflectance.coat_fresnel);
 let environment=diffuse_environment(n)*s.environment_scale*fallback;
 let sky=frame.hemisphere_sky_color;
 let hemisphere_intensity=frame.hemisphere_intensity;
 let ground=frame.hemisphere_ground_color;
 let hemisphere=pbr_hemisphere(n,sky,ground,hemisphere_intensity)/3.14159265359*fallback;
 var color=response*(environment+hemisphere);
 // The ambient light that ambient occlusion occludes: the environment's and
 // hemisphere fill's, or the volumes' irradiance in their place, its
 // diffuse share and its multiple scattering apart (occlusion_ambient).
 var ambient=ibl.diffuse*(environment+hemisphere);
 var ambient_multi=multi*(environment+hemisphere);
 // A probe hit takes the dynamic GI volume's last frame damped, as Wicked's
 // bounce is. Neither volume's irradiance takes environment_scale.
 let dynamic_gi=indirect.dynamic_gi;
 let bounce=select(1.,DYNAMIC_GI_BOUNCE,probe_hit);
 let irradiance=(indirect.field+dynamic_gi.rgb*dynamic_gi.a*bounce)*(1.-reflectance.coat_fresnel);
 color+=response*irradiance;
 ambient+=ibl.diffuse*irradiance;
 ambient_multi+=multi*irradiance;
 // Baked diffuse lies beneath the coat as live light does: Three.js 0.185.1's
 // node path adds a light map to the irradiance its finish dims
 // (NodeMaterial.setupLightMap, PhysicalLightingModel.finish), Godot b130438
 // adds lightmaps to the ambient light its clearcoat then attenuates
 // (scene_forward_clustered.glsl), and Filament ef1a133 dims all indirect
 // diffuse (surface_light_indirect.fs evaluateClearCoatIBL). Bevy 9d12036
 // adds its lightmap undimmed (pbr_functions.wesl). The material's
 // occlusion occludes it in every view, the camera's included, as Three.js
 // 0.185.1 occludes a light map by its AO map (setupLightMap's irradiance,
 // PhysicalLightingModel.ambientOcclusion) and Godot b130438 its lightmaps
 // by its AO (scene_forward_clustered.glsl ambient_light *= ao); the
 // frame's ambient occlusion does not, for a bake holds its own (Bevy 9d12036
 // likewise adds its lightmap unoccluded, pbr_functions.wesl): its diffuse
 // share linearly, its multiple scattering by its specular occlusion
 // (occlusion_multiscatter), as every view's ambient light.
 let baked_multi=multi*occlusion_multiscatter(specular_nv(n,v),rough,s.occlusion,f0);
 color+=(ibl.diffuse*s.occlusion+baked_multi)*indirect.baked*(1.-reflectance.coat_fresnel);
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
  let lobes=specular_lobes(n,coat_n,v,f0,reflectance.f90,rough,dfg,coat,coat_rough,s.anisotropy,lookup_tables,environment_sampler);
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
  let multi_occlusion=occlusion_multiscatter(specular_nv(n,v),rough,visibility,f0);
  color=occlusion_ambient(color,ambient,ambient_multi,visibility,multi_occlusion);
 }
 return Shaded(color,ambient,ambient_multi,indirect.sky_visibility);
}
