// Group 0 of the backdrop, additive effects and mist: view and frame data, the
// frame's backdrop (its environment map's panorama or, with FRAME_BACKDROP_SKY,
// the view's procedural sky-view table) and PMREM atlas, and its fog volume.
// Rust layout: shading::bind::unlit.
@group(0) @binding(0) var<uniform> view:View;
@group(0) @binding(1) var<uniform> frame:Frame;
@group(0) @binding(3) var backdrop_map:texture_2d<f32>;
@group(0) @binding(5) var environment_map:texture_2d_array<f32>;
@group(0) @binding(7) var environment_sampler:sampler;
// The frame's fog volume (frame_fog.wgsl) and its sampler.
@group(0) @binding(27) var fog_volume:texture_3d<f32>;
@group(0) @binding(28) var fog_sampler:sampler;
