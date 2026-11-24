// Group 2 of scene geometry: the material (material.wgsl), the maps every
// binding tier binds and their sampler, and what the scene keeps of it
// beside the values an edit rewrites (MaterialBindings): whether lightmap
// charts light it, in baked, the word of the map bindings past the record's
// sixteen (material_more_map_*), in more_maps, and each map binding's
// transform (map_uv.wgsl), by MATERIAL_SLOT_*. relief_map holds the normal map, else
// the bump map. Rust layout: shading::bind::group2 and
// shading::map_uvs::MaterialBindings.
struct MaterialBindings {
 baked:u32,
 more_maps:u32,
 map_uvs:array<MapUv,MATERIAL_SLOTS>,
}
@group(2) @binding(0) var<uniform> material:Material;
@group(2) @binding(1) var base_map:texture_2d<f32>;
@group(2) @binding(2) var mr_map:texture_2d<f32>;
@group(2) @binding(3) var tex_sampler:sampler;
@group(2) @binding(4) var emission_map:texture_2d<f32>;
@group(2) @binding(5) var relief_map:texture_2d<f32>;
@group(2) @binding(7) var<uniform> material_bindings:MaterialBindings;
