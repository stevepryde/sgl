// Group 2 of scene geometry: the material (material.wgsl), the maps every
// binding tier binds and their sampler, and whether lightmap charts light it.
// relief_map holds the normal map, else the bump map. Rust layout:
// shading::bind::group2.
@group(2) @binding(0) var<uniform> material:Material;
@group(2) @binding(1) var base_map:texture_2d<f32>;
@group(2) @binding(2) var mr_map:texture_2d<f32>;
@group(2) @binding(3) var tex_sampler:sampler;
@group(2) @binding(4) var emission_map:texture_2d<f32>;
@group(2) @binding(5) var relief_map:texture_2d<f32>;
@group(2) @binding(7) var<uniform> baked_material:vec4<u32>;
