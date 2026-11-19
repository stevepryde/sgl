// The screen-space indirect light provider of the opaque stage's fused and
// lighting passes while the pass has a kept result (the architecture's
// Screen-space indirect light): what the last finished frame kept, read at
// the fragment's previous position. Every other lit program composes
// camera_screen_indirect_none.wgsl instead; a program composes exactly one.
fn camera_screen_indirect(previous_clip:vec4<f32>,normal:vec3<f32>)->ScreenIndirect {
 return screen_indirect_read(screen_indirect_kept,screen_indirect_view,previous_clip,normal);
}
