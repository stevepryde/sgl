// The view depth of a device depth under any projection, which the game's
// material shaders (shader_scene_depth.wgsl) and HDR effects
// (hdr_effect_pass.wgsl) read the scene depth through, and its value at
// the sky: one owner for both contracts.
// What a scene depth function returns at the sky.
const SCENE_DEPTH_FAR:f32=1e30;
// The linear view depth in metres of device depth `z` under `projection`:
// whatever projection the camera has, its depth rows give the view-space z
// whose clip z over clip w is `z`. SCENE_DEPTH_FAR at the sky, where
// nothing was drawn: the depth targets' clear value, 0 under reversed-Z
// whatever the projection (an orthographic or finite-far one's far plane
// is no surface), and where no finite depth gives `z`.
fn projected_view_depth(projection:mat4x4<f32>,z:f32)->f32 {
 let p=projection;
 let denominator=z*p[2][3]-p[2][2];
 if z<=0. || denominator==0. {
  return SCENE_DEPTH_FAR;
 }
 return clamp(-(p[3][2]-z*p[3][3])/denominator,0.,SCENE_DEPTH_FAR);
}
