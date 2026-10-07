// The blended pipelines' group 3 bindings of the Extended binding tier
// alone, beside bind_blended.wgsl's: the transparent stage's mipmapped copy
// of the composed frame, which transmitted light samples
// (transmission_extended.wgsl). Rust layout: shading::bind::blended.
@group(3) @binding(3) var blended_transmission:texture_2d<f32>;
