// Group 2's maps of the Extended binding tier alone, beside
// bind_material.wgsl's (shading::bind::group2::MAP_BINDINGS). Bindings 9 and
// 10 and 16 to 19 are reserved for #243's and #245's maps.
@group(2) @binding(8) var anisotropy_map:texture_2d<f32>;
// KHR_materials_transmission's map (red) and KHR_materials_volume's
// thickness map (green).
@group(2) @binding(9) var transmission_map:texture_2d<f32>;
@group(2) @binding(10) var thickness_map:texture_2d<f32>;
@group(2) @binding(11) var clearcoat_map:texture_2d<f32>;
@group(2) @binding(12) var coat_roughness_map:texture_2d<f32>;
@group(2) @binding(13) var coat_normal_map:texture_2d<f32>;
@group(2) @binding(14) var iridescence_map:texture_2d<f32>;
@group(2) @binding(15) var iridescence_thickness_map:texture_2d<f32>;
