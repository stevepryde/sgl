// The view depth of a device depth under any projection a view may have,
// and its value at the sky: the one owner of that mapping, which the game's
// material shaders (shader_scene_depth.wgsl), HDR effects
// (hdr_effect_pass.wgsl) and the glow's soft fade
// (stages/transparent/glow.wgsl) read the scene depth through. Where the
// projection is known to be `perspective`'s, depth.wgsl's shorter form
// applies.
// What a scene depth function returns at the sky.
const SCENE_DEPTH_FAR:f32=1e30;
// The share of a view ray's device depth that its direction gives under a
// `projection` whose depth row has x and y terms, which only a perspective
// projection clipped at an oblique plane has (a planar reflector's
// reflected view, view/reflected_camera.rs): those terms times the
// view-space slopes (x and y over view depth) of the ray through `ndc`, an
// unjittered position in the image (x right, y up, each -1..1). 0 under any
// other projection, which projected_view_depth then does not need.
fn view_oblique_depth(projection:mat4x4<f32>,ndc:vec2<f32>)->f32 {
 let p=projection;
 if p[0][2]==0. && p[1][2]==0. {
  return 0.;
 }
 return p[0][2]*(ndc.x+p[2][0])/p[0][0]+p[1][2]*(ndc.y+p[2][1])/p[1][1];
}
// The linear view depth in metres of device depth `z` under `projection`,
// on the view ray whose direction gives `oblique` of it
// (view_oblique_depth; 0 but under a clipped projection): whatever
// projection the camera has, its depth rows give the view-space z whose
// clip z over clip w is `z`. SCENE_DEPTH_FAR at the sky, where nothing was
// drawn: the depth targets' clear value, 0 under reversed-Z whatever the
// projection (an orthographic or finite-far one's far plane is no
// surface), and where no finite depth gives `z`.
fn projected_view_depth(projection:mat4x4<f32>,z:f32,oblique:f32)->f32 {
 let p=projection;
 let denominator=z*p[2][3]-p[2][2]+oblique;
 if z<=0. || denominator==0. {
  return SCENE_DEPTH_FAR;
 }
 return clamp(-(p[3][2]-z*p[3][3])/denominator,0.,SCENE_DEPTH_FAR);
}
