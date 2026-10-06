// A world-space ray hit's Surface (surface.wgsl). The hit supplies its actual
// position, authored material and triangle UV differential frame; textures are
// pulled from the scene's ray buffers at LOD 0 (a normal map's scrolling
// layers where the frame's time puts them, as raster's), and the decals its
// list holds (decals.wgsl) sample the atlas's level 0. The outgoing
// direction is toward the receiver, never toward the primary camera. Reads
// the lit bindings and the scene's ray buffers.
fn ray_tangent_frame(hit:SceneHit)->mat3x3<f32> {
 let side=select(-1.,1.,hit.front_face);
 let frame=pbr_tangent_frame(hit.normal*side,hit.authored_tangent);
 return mat3x3(frame[0]*side,frame[1]*side,frame[2]*side);
}
fn ray_normal(hit:SceneHit,material:SceneMaterial)->vec3<f32> {
 // scene_trace flips the interpolated normal of accepted two-sided backs.
 // dp/du and dp/dv come from actual triangle positions and UVs. Their dual
 // cotangent frame is the same surface gradient construction as surface_normal.
 var n=hit.normal;
 let du=hit.tangent;
 let dv=hit.bitangent;
 if material.values.anisotropy_strength<=0. && (dot(du,du)==0. || dot(dv,dv)==0.) {
  return n;
 }
 if normal_maps_enabled && (material.values.flags&MATERIAL_NORMAL_MAP)!=0u {
  // The dual UV frame must retain the signed UV Jacobian. Its orientation
  // is measured against the authored geometric normal, not the back-flipped
  // lighting normal. This is the glTF tangent.w / mirrored-UV handedness;
  // n already reverses all three TBN columns for accepted back faces.
  let orientation=sign(dot(cross(du,dv),hit.geometric_normal));
  let tangent=cross(dv,n)*orientation;
  let bitangent=cross(n,du)*orientation;
  let scale=inverseSqrt(max(max(dot(tangent,tangent),dot(bitangent,bitangent)),0.0000001));
  let normal_map=material.textures[SCENE_TEXTURE_NORMAL];
  var mapped:vec3<f32>;
  if (material.values.flags&MATERIAL_NORMAL_LAYERS)!=0u {
   let phase=frame.animation_phase;
   let first=scene_sample_texture(normal_map,material_normal_layer_uv(material.values.normal_layers[0],hit.uv,phase),material.wrap,false);
   let second=scene_sample_texture(normal_map,material_normal_layer_uv(material.values.normal_layers[1],hit.uv,phase),material.wrap,false);
   mapped=material_layered_normal(material.values,first,second);
  } else {
   mapped=material_mapped_normal(material.values,scene_sample_texture(normal_map,hit.uv,material.wrap,false));
  }
  if material.values.anisotropy_strength>0. {
   n=normalize(ray_tangent_frame(hit)*mapped);
  } else {
   n=normalize(mat3x3(tangent*scale,bitangent*scale,n)*mapped);
  }
 }
 // Preserve MaterialNode.NORMAL's authored-map precedence even when the
 // diagnostic disables normal-map evaluation; the bump map remains unselected.
 if (material.values.flags&MATERIAL_NORMAL_MAP)==0u && bump_maps_enabled && (material.values.flags&MATERIAL_BUMP_MAP)!=0u {
  // One authored texel on each UV axis gives the bump surface gradient;
  // this is not a fabricated screen-space dpdx/dpdy in a compute invocation.
  let step=1./vec2<f32>(scene_texture_size(material.textures[SCENE_TEXTURE_BUMP]));
  let u=vec2(step.x,0.);
  let v=vec2(0.,step.y);
  let dhdu=(scene_sample_texture(material.textures[SCENE_TEXTURE_BUMP],hit.uv+u,material.wrap,false).r-scene_sample_texture(material.textures[SCENE_TEXTURE_BUMP],hit.uv-u,material.wrap,false).r)/(2.*step.x)*material.values.bump_scale;
  let dhdv=(scene_sample_texture(material.textures[SCENE_TEXTURE_BUMP],hit.uv+v,material.wrap,false).r-scene_sample_texture(material.textures[SCENE_TEXTURE_BUMP],hit.uv-v,material.wrap,false).r)/(2.*step.y)*material.values.bump_scale;
  // Match Three's faceDirection correction while retaining the physical UV
  // height-gradient contract and current central-difference samples.
  let a=cross(dv,n);
  let b=cross(n,du);
  let determinant=dot(du,a)*select(-1.,1.,hit.front_face);
  n=normalize(abs(determinant)*n-sign(determinant)*(dhdu*a+dhdv*b));
 }
 return n;
}
fn ray_base_color(hit:SceneHit,material:SceneMaterial)->vec4<f32> {
 return scene_base_color(material,hit.uv,hit.color);
}
fn ray_emission(hit:SceneHit,material:SceneMaterial)->vec3<f32> {
 var emission=material.values.emission*scene_sample_texture(material.textures[SCENE_TEXTURE_EMISSION],hit.uv,material.wrap,true).rgb;
 // A hit's flags are its object record's, so a hit is moving exactly where
 // raster's object.flags say so.
 if !instance_emission_enabled && (hit.instance_flags&OBJECT_STATIC)==0u {
  emission=vec3(0.);
 }
 return emission;
}
// Its decals are those of `clusters`, the ray hits' lists.
fn ray_surface(hit:SceneHit,material:SceneMaterial,base:vec4<f32>,emission:vec3<f32>,outgoing:vec3<f32>,clusters:ClusterRange)->Surface {
 let mr=scene_sample_texture(material.textures[SCENE_TEXTURE_METALLIC_ROUGHNESS],hit.uv,material.wrap,false);
 let unlit=(material.values.flags&MATERIAL_UNLIT)!=0u;
 var decaled=DecalSurface(base.rgb,ray_normal(hit,material),material.values.roughness*mr.g,material.values.metallic*mr.b);
 if !unlit {
  decaled=decal_surface(decaled,clusters,hit.position,hit.normal,vec3(0.),vec3(0.));
 }
 let n=decaled.normal;
 var anisotropy=vec4(0.);
 if material.values.anisotropy_strength>0. {
  anisotropy=pbr_resolve_anisotropy(n,ray_tangent_frame(hit),material.values.anisotropy_strength,material.values.anisotropy_rotation,(material.values.flags&MATERIAL_ANISOTROPY_MAP)!=0u,scene_sample_texture(material.textures[SCENE_TEXTURE_ANISOTROPY],hit.uv,material.wrap,false).rgb);
 }
 var s:Surface;
 s.position=hit.position;
 s.view=outgoing;
 s.normal=n;
 s.geometry_normal=hit.normal;
 s.base=vec4(decaled.base,base.a);
 s.metallic=decaled.metallic;
 s.roughness=clamp(decaled.roughness,.0525,1.);
 s.coat=material.values.coat;
 s.coat_roughness=clamp(material.values.coat_roughness,.0525,1.);
 s.anisotropy=anisotropy;
 s.emission=emission;
 s.environment_scale=material.values.environment_scale;
 s.unlit=unlit;
 s.front=hit.front_face;
 s.moving=(hit.instance_flags&OBJECT_STATIC)==0u;
 s.baked=material.baked!=0u;
 s.uv=hit.uv;
 s.lightmap_uv=hit.lightmap_uv;
 s.lightmap_bounds=hit.lightmap_bounds;
 s.baked_irradiance=objects[hit.instance_id].baked_irradiance;
 return s;
}
// Where a probe hit's visibility ray starts past the hit, and how far
// short of the light it ends (light_visibility_ray), in metres: Wicked's
// DDGI shadow ray's TMin (ddgi_raytraceCS.hlsl).
const PROBE_HIT_T_MIN:f32=.001;
// A dynamic GI probe ray's hit's direct light (SHADOW_RECEIVER_PROBE_HIT):
// one light drawn uniformly by `random.x` from the frame's directional
// lights and the lights of `list` the surface takes, times their count; its
// diffuse light alone, unoccluded by a map, and, for a light that casts a
// shadow, its visibility one any-hit ray toward it through the one
// acceptance predicate over both kinds of instance and both sides of every
// triangle, at its shadow opacity, the ray not cast at or below the cutoff.
// A light that casts no shadow lights the hit unoccluded, as it lights every
// other receiver, as Godot's VoxelGI traces a light only where it has one
// (b13043816a0f234985030ec035363a005bc86c32,
// servers/rendering/renderer_rd/shaders/environment/voxel_gi.glsl 297,
// `has_shadow` from gi.cpp 3036; MIT, src/LICENSE-godot.txt). The ray ends
// short of where `random.yz` draws it on the light, as ray-traced shadows'
// rays do (light_surface.wgsl, light_visibility_ray): a point of a point or
// spot light's sphere or of a rectangle's face, or a direction within a
// directional light's disc.
//
// Ports Wicked Engine df44c3db4c4927492bc9c791eac715d98d7ed091's light
// sampling at a hit (WickedEngine/shaders/ddgi_raytraceCS.hlsl 329–490: one
// light drawn uniformly, its diffuse light times NdotL / PI times the light
// count, and a TraceRay_Any shadow ray from 0.001 to the light, to infinity
// for a directional light, culling no side), MIT (src/LICENSE-wicked.txt).
// Changed: each light reaches the hit as every receiver's does
// (scene_light_sample, a rectangle integrated over its face), its shadow
// opacity applies, a light without a shadow casts no ray, and the ray's
// end is drawn on the light as SGL3D's ray-traced shadows draw it and
// ends its TMin short of it, so the light's own fixture never occludes it
// (#228). Not
// taken: NVIDIA RTXGI's shading of every light at a hit (practice only),
// which removes the noise of the light's choice. The allocation measures a
// texel's inconsistency against its own deviation, so less noise leaves it
// as it was: in a static room with two shadowed lights a probe still traced
// about 63 rays a frame at High after 250 frames with one light per hit and
// 76 with every light, each hit casting a shadow ray per light.
fn probe_hit_light(s:Surface,list:ClusterRange,random:vec3<f32>)->vec3<f32> {
 var directional=array<u32,2>(0u,0u);
 var directional_count=0u;
 for (var index=0u;index<2u;index++) {
  if frame.directional_lights[index].illuminance>0. {
   directional[directional_count]=index;
   directional_count++;
  }
 }
 var scene_count=list.live;
 if takes_baked_lights(s.baked,s.lightmap_uv,s.moving) {
  scene_count+=list.baked;
 }
 let light_count=directional_count+scene_count;
 if light_count==0u {
  return vec3(0.);
 }
 let pick=min(u32(floor(random.x*f32(light_count))),light_count-1u);
 var sample:LightSample;
 var direction=vec3(0.);
 // The visibility ray's direction: toward where it ends on the light.
 var ray=vec3(0.);
 // A light at infinity: Wicked's FLT_MAX.
 var distance=3.402823466e+38;
 // The shadow opacity of a light that casts a shadow, else none.
 var opacity=0.;
 if pick<directional_count {
  let light=frame.directional_lights[directional[pick]];
  direction=normalize(light.direction_to_light);
  if dot(s.normal,direction)<=0. {
   return vec3(0.);
  }
  sample=LightSample(direction,light.color*light.illuminance,1.,0.,NO_RECT_LIGHT);
  // The light with the frame's cascades is the one directional light
  // that casts a shadow. Its visibility ray leaves within its disc.
  opacity=select(0.,light.shadow_opacity,(light.flags&DIRECTIONAL_LIGHT_SHADOW)!=0u);
  ray=directional_ray_direction(direction,light.disc_radius,random.yz);
 } else {
  let index=cluster_item(list.first+pick-directional_count);
  sample=scene_light_sample(index,s.position,s.normal,s.geometry_normal,vec2(0.),SHADOW_RECEIVER_PROBE_HIT);
  if sample.visibility<=0. {
   return vec3(0.);
  }
  let light=lights[index];
  let visibility_ray=light_visibility_ray(light,s.position,random.yz,PROBE_HIT_T_MIN);
  ray=visibility_ray.xyz;
  distance=visibility_ray.w;
  opacity=select(0.,light.shadow_opacity,(light.flags&LIGHT_CASTS_SHADOW)!=0u);
 }
 sample.specular=0.;
 if opacity>SHADOW_OPACITY_CUTOFF {
  let visible=scene_segment_visible(s.position,ray,PROBE_HIT_T_MIN,distance,SCENE_SIDES_BOTH);
  sample.visibility=shadow_opacity_visibility(select(0.,1.,visible),opacity);
 }
 // The surface's reflectance as shade_lit derives it, its DFG lookup at the
 // view included, so the one light is shaded as every receiver's lights are.
 let reflectance=surface_reflectance(s,surface_dfg(specular_nv(s.normal,s.view),s.roughness));
 return surface_direct_light(s,reflectance,sample)*f32(light_count);
}
// Radiance leaving a ray hit toward `outgoing` as `receiver`, a world-space
// reflection ray's hit (SHADOW_RECEIVER_CAPTURE) or a dynamic GI probe
// ray's (SHADOW_RECEIVER_PROBE_HIT), its ambient diffuse unoccluded. An
// offscreen hit has no camera pixel or view depth: a reflection ray's hit
// takes the directional shadow's first cascade that holds it, with the
// fixed filter, and its environment specular from the installed probes and
// the reflection sky beyond them, as a probe capture's surfaces take it. A
// probe ray's hit takes its diffuse light alone and the one light `random`
// draws (probe_hit_light); other hits ignore `random`.
//
// A probe ray's hit on a material that does not emit into global
// illumination (MATERIAL_EMITS_INTO_GI clear, from
// SurfaceMaterial::emits_into_gi), a fixture a scene light stands for,
// takes none of the light the material gives off itself: its emission, and
// an unlit material's whole colour, all of which is its own light. That light
// reaches the probes once, through the scene light and the hits it lights.
// The hit still ends the ray, so the surface occludes, and a lit one still
// reflects the light that reaches it, as Unity's emission GI flag None
// keeps a glowing material's light out of its GI while the object stays a
// GI contributor (MaterialGlobalIlluminationFlags.None, practice only).
// Not taken: Godot's GeometryInstance3D.gi_mode, Unity's Contribute GI and
// Unreal's Affect Dynamic Indirect Lighting, which take the whole object out
// of GI, occluder and bounce too. A reflection ray's hit keeps it: a
// reflection shows the fixture as it glows.
fn shade_ray_hit(hit:SceneHit,outgoing:vec3<f32>,receiver:u32,random:vec3<f32>)->vec3<f32> {
 let material=scene_material(hit.material_word);
 let probe_hit=receiver==SHADOW_RECEIVER_PROBE_HIT;
 let own_light=!probe_hit || (material.values.flags&MATERIAL_EMITS_INTO_GI)!=0u;
 let unlit=(material.values.flags&MATERIAL_UNLIT)!=0u;
 if unlit && !own_light {
  return vec3(0.);
 }
 let base=ray_base_color(hit,material);
 var emission=vec3(0.);
 if own_light {
  emission=ray_emission(hit,material);
 }
 if unlit {
  return shade_unlit(unlit_surface(base,emission)).color;
 }
 let context=ShadeContext(vec2(0.),receiver,!probe_hit,cluster_range(hit.position,vec2(0.)),untraced_reflection());
 let s=ray_surface(hit,material,base,emission,outgoing,context.clusters);
 var color=shade_lit(s,context).color;
 if probe_hit {
  color+=probe_hit_light(s,context.clusters,random);
 }
 return max(vec3(0.),color);
}
