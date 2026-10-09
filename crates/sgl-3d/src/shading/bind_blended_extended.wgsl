// The blended pipelines' group 3 bindings of the Extended binding tier
// alone, beside bind_blended.wgsl's: the transparent stage's mipmapped copy
// of the composed frame, which transmitted light samples
// (transmission_extended.wgsl), and the opaque depth. Rust layout:
// shading::bind::blended.
@group(3) @binding(3) var blended_transmission:texture_2d<f32>;
// The opaque depth, which the blended draws test without writing it: the
// scene depth a game's shader reads (shader_scene_depth.wgsl).
@group(3) @binding(4) var blended_scene_depth:texture_depth_2d;
