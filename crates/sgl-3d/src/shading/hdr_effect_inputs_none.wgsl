// The contract's functions as a game's module is parsed against them
// (shading::shader::validate): their signatures, no binding. No program
// composes them.
fn effect_color(uv:vec2<f32>)->vec3<f32> {
 return vec3(0.);
}
fn effect_view_depth(uv:vec2<f32>)->f32 {
 return SCENE_DEPTH_FAR;
}
fn effect_distance(uv:vec2<f32>)->f32 {
 return SCENE_DEPTH_FAR;
}
