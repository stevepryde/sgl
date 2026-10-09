// The scene depth provider of every pass but the camera's blended draws on
// the Extended binding tier (shader_scene_depth.wgsl): the opaque depth is
// being written, or the pass is no camera's, or the device's sampled
// textures are spent (Basic). A program composes it or
// shader_scene_depth.wgsl, exactly one.
fn scene_depth_available()->bool {
 return false;
}
fn scene_depth(pixel:vec2<f32>)->f32 {
 return 0.;
}
fn scene_depth_behind(ctx:SurfaceContext)->f32 {
 return 0.;
}
