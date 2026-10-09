// The blended pipelines' group 3 bindings of the Extended binding tier
// alone, beside bind_blended.wgsl's: the transparent stage's mipmapped copy
// of the composed frame, which transmitted light samples
// (transmission_extended.wgsl), the opaque depth and the volume layers.
// Rust layout: shading::bind::blended.
@group(3) @binding(3) var blended_transmission:texture_2d<f32>;
// The opaque depth, which the blended draws test without writing it: the
// scene depth a game's shader reads (shader_scene_depth.wgsl).
@group(3) @binding(4) var blended_scene_depth:texture_depth_2d;
// The transparent stage's volume layers (stages/transparent/volumes.rs),
// each a copy of the opaque depth with the nearest faces of the blended
// materials whose shader reads its volume path drawn over it: front faces
// (entry), back faces (exit), and back faces behind the exit layer's
// (second exit); the opaque depth in their place where BlendedTrace.volumes
// is 0. A game's shader reads them through scene_volume_path
// (shader_scene_depth.wgsl), and the second exit's pass the exit layer.
@group(3) @binding(5) var blended_volume_entry:texture_depth_2d;
@group(3) @binding(6) var blended_volume_exit:texture_depth_2d;
@group(3) @binding(7) var blended_volume_second_exit:texture_depth_2d;
