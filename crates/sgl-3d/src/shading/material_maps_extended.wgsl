// The material-map provider of the Extended binding tier: samples the maps
// only it binds (bind_material_extended.wgsl) with the view's mip bias. A
// program composes it or material_maps_basic.wgsl, exactly one, and calls
// it from surface_raster.wgsl alone.
// The anisotropy map's texel at `uv`.
fn material_anisotropy_texel(uv:vec2<f32>)->vec3<f32> {
 return textureSampleBias(anisotropy_map,tex_sampler,uv,view.mip_bias).rgb;
}
// The transmission map's texel at `uv`.
fn material_transmission_texel(uv:vec2<f32>)->vec4<f32> {
 return textureSampleBias(transmission_map,tex_sampler,uv,view.mip_bias);
}
// The thickness map's texel at `uv`.
fn material_thickness_texel(uv:vec2<f32>)->vec4<f32> {
 return textureSampleBias(thickness_map,tex_sampler,uv,view.mip_bias);
}
