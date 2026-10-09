// The scene depth provider of the camera's blended draws on the Extended
// binding tier: the opaque depth (bind_blended_extended.wgsl's
// blended_scene_depth), which those draws test without writing it, as
// Godot b130438 gives its transparent pass the depth copied after its
// opaque pass (render_forward_clustered.cpp 2451–2512) and Filament ef1a133
// its refractive pass the opaque pass's (RendererUtils.cpp 312–381). It
// holds no blended surface, no receiver and no effect. Reads `view`.
fn scene_depth_available()->bool {
 return true;
}
// The linear view depth in metres of device depth `z` under the view's
// projection: whatever projection the camera has, its depth rows give the
// view-space z whose clip z over clip w is `z`. SCENE_DEPTH_FAR where no
// finite depth does (the sky under an infinite projection).
fn scene_view_depth(z:f32)->f32 {
 let p=view.projection;
 let denominator=z*p[2][3]-p[2][2];
 if denominator==0. {
  return SCENE_DEPTH_FAR;
 }
 return clamp(-(p[3][2]-z*p[3][3])/denominator,0.,SCENE_DEPTH_FAR);
}
// The linear view depth in metres of the opaque surface at `pixel`, a
// position in the render target in texels: SCENE_DEPTH_FAR at the sky.
fn scene_depth(pixel:vec2<f32>)->f32 {
 let last=vec2<i32>(textureDimensions(blended_scene_depth))-vec2(1);
 let texel=clamp(vec2<i32>(floor(pixel)),vec2(0),last);
 return scene_view_depth(textureLoad(blended_scene_depth,texel,0));
}
// The distance in metres along the fragment's view ray from it to the
// opaque surface behind it, 0 where that surface is in front of it: along
// a perspective view's ray from the eye, its depth difference over the
// ray's cosine with the view axis; along an orthographic one's, the
// difference. At most SCENE_DEPTH_FAR.
fn scene_depth_behind(ctx:SurfaceContext)->f32 {
 let behind=max(scene_depth(ctx.pixel)-ctx.view_depth,0.);
 if view.projection[2][3]==0. {
  return behind;
 }
 let position=(view.view*vec4(ctx.position,1.)).xyz;
 return min(behind*length(position)/max(-position.z,1e-6),SCENE_DEPTH_FAR);
}
