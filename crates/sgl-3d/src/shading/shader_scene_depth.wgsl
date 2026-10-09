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
 return scene_along_ray(ctx,scene_depth(ctx.pixel)-ctx.view_depth);
}
// A difference `depth` in linear view depth along the fragment's view ray,
// as a distance: over the ray's cosine with the view axis along a
// perspective view's ray from the eye, the difference along an
// orthographic one's. Within 0 and SCENE_DEPTH_FAR.
fn scene_along_ray(ctx:SurfaceContext,depth:f32)->f32 {
 let difference=max(depth,0.);
 if view.projection[2][3]==0. {
  return min(difference,SCENE_DEPTH_FAR);
 }
 let position=(view.view*vec4(ctx.position,1.)).xyz;
 return min(difference*length(position)/max(-position.z,1e-6),SCENE_DEPTH_FAR);
}
// The fragment's view ray inside the volume its material bounds, from the
// transparent stage's volume layers (bind_blended_extended.wgsl), where the
// draw's group 3 holds this frame's (BlendedTrace.volumes): each layer's
// texel is the opaque depth's where no face lies in front of the opaque
// surface, else the device depth (reversed-Z: nearer is greater) of the
// nearest front face (entry), back face (exit) and back face behind that
// (second exit). A `front` fragment's path runs to the nearest exit at or
// behind it while at most one exit lies in front of it, else to the opaque
// surface; a back fragment's from the nearest entry in front of it, else
// from the eye. Lengths are measured as scene_depth_behind measures them.
fn scene_volume_path(ctx:SurfaceContext)->VolumePath {
 if blended_trace.volumes==0u {
  return VolumePath(0.,VOLUME_NONE);
 }
 let last=vec2<i32>(textureDimensions(blended_scene_depth))-vec2(1);
 let texel=clamp(vec2<i32>(floor(ctx.pixel)),vec2(0),last);
 let opaque=textureLoad(blended_scene_depth,texel,0);
 if ctx.front {
  let to_opaque=scene_along_ray(ctx,scene_view_depth(opaque)-ctx.view_depth);
  let exit=textureLoad(blended_volume_exit,texel,0);
  if exit<=opaque {
   return VolumePath(to_opaque,VOLUME_OPAQUE);
  }
  let exit_depth=scene_view_depth(exit);
  if exit_depth>=ctx.view_depth {
   return VolumePath(scene_along_ray(ctx,exit_depth-ctx.view_depth),VOLUME_EXIT);
  }
  let second=textureLoad(blended_volume_second_exit,texel,0);
  if second<=opaque {
   return VolumePath(to_opaque,VOLUME_OPAQUE);
  }
  let second_depth=scene_view_depth(second);
  if second_depth>=ctx.view_depth {
   return VolumePath(scene_along_ray(ctx,second_depth-ctx.view_depth),VOLUME_EXIT);
  }
  return VolumePath(to_opaque,VOLUME_HIDDEN);
 }
 let entry=textureLoad(blended_volume_entry,texel,0);
 if entry>opaque {
  let entry_depth=scene_view_depth(entry);
  if entry_depth<ctx.view_depth {
   return VolumePath(scene_along_ray(ctx,ctx.view_depth-entry_depth),VOLUME_ENTRY);
  }
 }
 return VolumePath(scene_along_ray(ctx,ctx.view_depth),VOLUME_EYE);
}
