// A world-space ray hit's Surface (surface.wgsl). The hit supplies its actual
// position, authored material and triangle UV differential frame; textures are
// pulled from the scene's ray buffers at LOD 0, and the decals its list holds
// (decals.wgsl) sample the atlas's level 0. The outgoing direction is toward
// the receiver, never toward the primary camera. Reads the lit bindings and
// the scene's ray buffers.
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
  var mapped=scene_sample_texture(material.textures[SCENE_TEXTURE_NORMAL],hit.uv,material.wrap,false).xyz*2.-vec3(1.);
  mapped=vec3(mapped.xy*material.values.normal_scale,mapped.z);
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
 s.coat_normal=hit.normal;
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
// A dynamic GI probe ray's hit's direct light (SHADOW_RECEIVER_PROBE_HIT):
// one light drawn uniformly by `random.x` from the frame's directional
// lights and the lights of `list` the surface takes, times their count; its
// diffuse light alone, unoccluded by a map, and its visibility one any-hit
// ray toward it through the one acceptance predicate over both kinds of
// instance, at its shadow opacity, the ray not cast at or below the
// cutoff. A rectangle's ray ends at the point of its face `random.yz` draws.
//
// Ports Wicked Engine df44c3db4c4927492bc9c791eac715d98d7ed091's light
// sampling at a hit (WickedEngine/shaders/ddgi_raytraceCS.hlsl 329–490: one
// light drawn uniformly, its diffuse light times NdotL / PI times the light
// count, and a TraceRay_Any shadow ray from 0.001 to the light, to infinity
// for a directional light), MIT (src/LICENSE-wicked.txt). Changed: each
// light reaches the hit as every receiver's does (scene_light_sample, a
// rectangle integrated over its face), its shadow opacity applies, and
// SGL3D's lights are punctual, so Wicked's radius jitter has no
// counterpart.
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
 // A light at infinity: Wicked's FLT_MAX.
 var distance=3.402823466e+38;
 var opacity=0.;
 if pick<directional_count {
  let light=frame.directional_lights[directional[pick]];
  direction=normalize(light.direction_to_light);
  if dot(s.normal,direction)<=0. {
   return vec3(0.);
  }
  sample=LightSample(direction,light.color*light.illuminance,1.,0.,NO_RECT_LIGHT);
  opacity=light.shadow_opacity;
 } else {
  let index=cluster_item(list.first+pick-directional_count);
  sample=scene_light_sample(index,s.position,s.normal,vec2(0.),SHADOW_RECEIVER_PROBE_HIT);
  if sample.visibility<=0. {
   return vec3(0.);
  }
  let light=lights[index];
  var end=light.position;
  if sample.rect!=NO_RECT_LIGHT {
   end+=light.half_width*(random.y*2.-1.)+light_rect_half_height(light)*(random.z*2.-1.);
  }
  let to_light=end-s.position;
  distance=length(to_light);
  direction=to_light/max(distance,1e-20);
  opacity=light.shadow_opacity;
 }
 sample.specular=0.;
 if opacity>SHADOW_OPACITY_CUTOFF {
  let visible=scene_segment_visible(s.position,direction,.001,distance);
  sample.visibility=shadow_opacity_visibility(select(0.,1.,visible),opacity);
 }
 let reflectance=surface_reflectance(s,vec2(0.));
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
fn shade_ray_hit(hit:SceneHit,outgoing:vec3<f32>,receiver:u32,random:vec3<f32>)->vec3<f32> {
 let material=scene_material(hit.material_word);
 let base=ray_base_color(hit,material);
 let emission=ray_emission(hit,material);
 if (material.values.flags&MATERIAL_UNLIT)!=0u {
  return shade_unlit(unlit_surface(base,emission)).color;
 }
 let probe_hit=receiver==SHADOW_RECEIVER_PROBE_HIT;
 let context=ShadeContext(vec2(0.),receiver,!probe_hit,cluster_range(hit.position,vec2(0.)),untraced_reflection());
 let s=ray_surface(hit,material,base,emission,outgoing,context.clusters);
 var color=shade_lit(s,context).color;
 if probe_hit {
  color+=probe_hit_light(s,context.clusters,random);
 }
 return max(vec3(0.),color);
}
