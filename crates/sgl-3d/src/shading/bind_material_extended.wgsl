// Group 2's maps of the Extended binding tier alone, beside
// bind_material.wgsl's (shading::bind::group2::MAP_BINDINGS).
@group(2) @binding(8) var anisotropy_map:texture_2d<f32>;
// KHR_materials_transmission's map (red) and KHR_materials_volume's
// thickness map (green).
@group(2) @binding(9) var transmission_map:texture_2d<f32>;
@group(2) @binding(10) var thickness_map:texture_2d<f32>;
