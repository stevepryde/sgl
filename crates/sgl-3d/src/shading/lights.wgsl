// The scene lights that reach a point, and each one as a LightSample.
// Reads `view`, `lights`, `clusters` and local_shadow.wgsl's bindings; a
// view's clusters list them (clusters.wgsl). How a light reaches a point is
// light_reach.wgsl's.
// Scene light `index` at `receiver` (SHADOW_RECEIVER_*) with its position,
// its shading normal and its geometry normal (Surface), seen at the view's
// `pixel`. It does not reach a receiver light_reach says it does not
// reach, or that its shadow, looked up along the geometry normal, at its
// shadow opacity, fully occludes. A light at or below the shadow opacity
// cutoff looks up no shadow. The camera's surfaces take the shadow of a
// light the ray-traced shadow mask holds from the mask, every other
// receiver from the maps. A point in the fog has no side, so a light
// reaches it from any direction; its normals are zero, so its shadow takes
// no normal offset.
fn scene_light_sample(index:u32,position:vec3<f32>,normal:vec3<f32>,geometry_normal:vec3<f32>,pixel:vec2<f32>,receiver:u32)->LightSample {
 let unreached=LightSample(vec3(0.),vec3(0.),0.,0.,NO_RECT_LIGHT,0.);
 let light=lights[index];
 let reach=light_reach(light,position,normal,receiver==SHADOW_RECEIVER_MEDIUM);
 if reach.attenuation<=0. {
  return unreached;
 }
 var visibility=1.;
 // A probe hit looks up no map: its caller casts a visibility ray. The
 // camera's surfaces take a light the ray-traced shadow mask holds from it
 // (camera_shadow_mask, whose provider the program composes).
 if light.shadow_opacity>SHADOW_OPACITY_CUTOFF && receiver!=SHADOW_RECEIVER_PROBE_HIT {
  var shadow=SHADOW_MASK_NO_SLOT;
  if receiver==SHADOW_RECEIVER_CAMERA {
   shadow=camera_shadow_mask(index,pixel);
  }
  if shadow<0. {
   shadow=local_shadow_visibility(index,light.position,light.range,position,geometry_normal,pixel,receiver);
  }
  visibility=shadow_opacity_visibility(shadow,light.shadow_opacity);
 }
 if visibility<=0. {
  return unreached;
 }
 // A point or spot light's sphere over its distance; a rectangle has none.
 let to_light=light.position-position;
 let size=select(light.radius*inverseSqrt(dot(to_light,to_light)),0.,reach.rect);
 return LightSample(reach.direction,light.color*reach.attenuation,visibility,light.specular,select(NO_RECT_LIGHT,index,reach.rect),size);
}
