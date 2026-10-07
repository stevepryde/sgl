// The material-map provider of the Extended binding tier: samples the maps
// only it binds (bind_material_extended.wgsl) with the view's mip bias. A
// program composes it or material_maps_basic.wgsl, exactly one, and calls
// it from surface_raster.wgsl alone.
// The anisotropy map's texel at `uv`.
fn material_anisotropy_texel(uv:vec2<f32>)->vec3<f32> {
 return textureSampleBias(anisotropy_map,tex_sampler,uv,view.mip_bias).rgb;
}
// The clearcoat, clearcoat roughness, clearcoat normal, iridescence and
// iridescence thickness maps' texels at `uv`.
fn material_clearcoat_texel(uv:vec2<f32>)->vec4<f32> {
 return textureSampleBias(clearcoat_map,tex_sampler,uv,view.mip_bias);
}
fn material_coat_roughness_texel(uv:vec2<f32>)->vec4<f32> {
 return textureSampleBias(coat_roughness_map,tex_sampler,uv,view.mip_bias);
}
fn material_coat_normal_texel(uv:vec2<f32>)->vec4<f32> {
 return textureSampleBias(coat_normal_map,tex_sampler,uv,view.mip_bias);
}
fn material_iridescence_texel(uv:vec2<f32>)->vec4<f32> {
 return textureSampleBias(iridescence_map,tex_sampler,uv,view.mip_bias);
}
fn material_iridescence_thickness_texel(uv:vec2<f32>)->vec4<f32> {
 return textureSampleBias(iridescence_thickness_map,tex_sampler,uv,view.mip_bias);
}
// The transmission map's texel at `uv`.
fn material_transmission_texel(uv:vec2<f32>)->vec4<f32> {
 return textureSampleBias(transmission_map,tex_sampler,uv,view.mip_bias);
}
// The thickness map's texel at `uv`.
fn material_thickness_texel(uv:vec2<f32>)->vec4<f32> {
 return textureSampleBias(thickness_map,tex_sampler,uv,view.mip_bias);
}
// The sheen colour and roughness and the diffuse transmission and its
// colour maps' texels at `uv`.
fn material_sheen_color_texel(uv:vec2<f32>)->vec4<f32> {
 return textureSampleBias(sheen_color_map,tex_sampler,uv,view.mip_bias);
}
fn material_sheen_roughness_texel(uv:vec2<f32>)->vec4<f32> {
 return textureSampleBias(sheen_roughness_map,tex_sampler,uv,view.mip_bias);
}
fn material_diffuse_transmission_texel(uv:vec2<f32>)->vec4<f32> {
 return textureSampleBias(diffuse_transmission_map,tex_sampler,uv,view.mip_bias);
}
fn material_diffuse_transmission_color_texel(uv:vec2<f32>)->vec4<f32> {
 return textureSampleBias(diffuse_transmission_color_map,tex_sampler,uv,view.mip_bias);
}
