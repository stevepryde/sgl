// Group 2 of scene geometry: the material (material.wgsl), its maps and
// sampler, and whether lightmap charts light it.
@group(2) @binding(0) var<uniform> material:Material;
@group(2) @binding(1) var base_map:texture_2d<f32>;
@group(2) @binding(2) var mr_map:texture_2d<f32>;
@group(2) @binding(3) var tex_sampler:sampler;
@group(2) @binding(4) var emission_map:texture_2d<f32>;
@group(2) @binding(5) var normal_map:texture_2d<f32>;
@group(2) @binding(6) var bump_map:texture_2d<f32>;
@group(2) @binding(8) var anisotropy_map:texture_2d<f32>;
@group(2) @binding(7) var<uniform> baked_material:vec4<u32>;
