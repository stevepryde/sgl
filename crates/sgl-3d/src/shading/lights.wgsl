// The scene lights that reach a point, and each one as a LightSample.
// Reads `view`, `lights`, `clusters` and local_shadow.wgsl's bindings; a
// view's clusters list them (clusters.wgsl).
//
// The attenuation is Filament ef1a133's
// shaders/src/surface_light_punctual.fs (getSquareFalloffAttenuation,
// getDistanceAttenuation without its camera-distance fade,
// getAngleAttenuation, getLight's one loop over point and spot lights),
// Apache-2.0 (src/LICENSE-filament.txt), as Bevy's point_light and
// spot_light use it. A rectangle's is Bevy's rect_light
// (crates/bevy_pbr/src/render/pbr_lighting.wesl): one-sided, and faded by
// the range window alone, since its integral over the face falls off with
// distance.
// Whether the pipeline shades rectangle lights: off while the scene holds
// none (view/pipelines.rs), so no sample names one and a scene without them
// pays nothing for their shading.
override rect_lights_enabled:bool=true;
// Smooth falloff to zero at the range (Bevy's getRangeFalloff).
fn light_range_window(distance_square:f32,inverse_square_range:f32)->f32 {
 let factor=distance_square*inverse_square_range;
 let smooth_factor=saturate(1.-factor*factor);
 return smooth_factor*smooth_factor;
}
// The range window over the inverse square of the distance, taken as at
// least one centimetre.
fn light_distance_attenuation(distance_square:f32,inverse_square_range:f32)->f32 {
 return light_range_window(distance_square,inverse_square_range)/max(distance_square,.0001);
}
// A spot light's cone at the unit direction from the receiver toward it.
fn light_angle_attenuation(light:Light,to_light:vec3<f32>)->f32 {
 let cosine=dot(light.direction,-to_light);
 let attenuation=saturate(cosine*light.spot_scale+light.spot_offset);
 return attenuation*attenuation;
}
// Scene light `index` at `receiver` (SHADOW_RECEIVER_*) with its position
// and normal, seen at the view's `pixel`. It does not reach a receiver it is
// out of range of, outside the cone of, behind (a rectangle: not in front of
// its face), or that its shadow fully occludes. A point in the fog has no
// side, so a light reaches it from any direction; its normal is zero, so
// its shadow takes no normal offset.
fn scene_light_sample(index:u32,position:vec3<f32>,normal:vec3<f32>,pixel:vec2<f32>,receiver:u32)->LightSample {
 let unreached=LightSample(vec3(0.),vec3(0.),0.,0.,NO_RECT_LIGHT);
 let light=lights[index];
 let to_light=light.position-position;
 let distance_square=dot(to_light,to_light);
 if distance_square<=1e-10 {
  return unreached;
 }
 let direction=to_light*inverseSqrt(distance_square);
 var attenuation=0.;
 let rect=rect_lights_enabled&&light.shape==LIGHT_RECT;
 if rect {
  // A rectangle lights the half-space in front of its face, wherever the
  // receiver's normal points: clipping its face to the receiver's
  // hemisphere takes the rest.
  if dot(light.direction,direction)>=0. {
   return unreached;
  }
  attenuation=light_range_window(distance_square,light.inverse_square_range);
 } else {
  if receiver!=SHADOW_RECEIVER_MEDIUM && dot(normal,direction)<=0. {
   return unreached;
  }
  attenuation=light_distance_attenuation(distance_square,light.inverse_square_range)*light_angle_attenuation(light,direction);
 }
 if attenuation<=0. {
  return unreached;
 }
 let visibility=local_shadow_visibility(index,light.position,light.range,position,normal,pixel,receiver);
 if visibility<=0. {
  return unreached;
 }
 return LightSample(direction,light.color*attenuation,visibility,light.specular,select(NO_RECT_LIGHT,index,rect));
}
