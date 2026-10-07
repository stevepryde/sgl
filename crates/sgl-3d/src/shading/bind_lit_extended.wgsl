// Lit group 0's bindings of the Extended binding tier alone, beside
// bind_lit.wgsl's (shading::bind::group0::tier): the lightmap's and the
// irradiance atlas's directionality (baked_lighting.wgsl), and the dynamic
// GI volume's probes (dynamic_gi.wgsl), filtered through `baked_sampler`; a
// stand-in in frames FRAME_DYNAMIC_GI leaves clear.
@group(0) @binding(19) var static_lightmap_direction:texture_2d_array<f32>;
@group(0) @binding(26) var static_direction_atlas:texture_2d_array<f32>;
@group(0) @binding(29) var dynamic_gi_probes:texture_2d<f32>;
