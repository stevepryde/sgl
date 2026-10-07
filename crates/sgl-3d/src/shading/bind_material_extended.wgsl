// Group 2's maps of the Extended binding tier alone, beside
// bind_material.wgsl's (shading::bind::group2::MAP_BINDINGS). Bindings 9 and
// 10 are reserved for #243's maps.
@group(2) @binding(8) var anisotropy_map:texture_2d<f32>;
@group(2) @binding(11) var clearcoat_map:texture_2d<f32>;
@group(2) @binding(12) var coat_roughness_map:texture_2d<f32>;
@group(2) @binding(13) var coat_normal_map:texture_2d<f32>;
@group(2) @binding(14) var iridescence_map:texture_2d<f32>;
@group(2) @binding(15) var iridescence_thickness_map:texture_2d<f32>;
@group(2) @binding(16) var sheen_color_map:texture_2d<f32>;
@group(2) @binding(17) var sheen_roughness_map:texture_2d<f32>;
@group(2) @binding(18) var diffuse_transmission_map:texture_2d<f32>;
@group(2) @binding(19) var diffuse_transmission_color_map:texture_2d<f32>;
