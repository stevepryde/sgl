// The screen-space indirect light provider of every lit program but the
// opaque stage's fused and lighting passes while the pass has a kept
// result: probe captures, blended surfaces, and the camera's opaque
// surfaces while the setting is off or nothing is kept, which take none
// and bind nothing for it. A program composes exactly one provider
// (camera_screen_indirect.wgsl being the other).
fn camera_screen_indirect(previous_clip:vec4<f32>,normal:vec3<f32>)->ScreenIndirect {
 return screen_indirect_none();
}
