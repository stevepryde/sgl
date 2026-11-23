// The material-map provider of the Extended binding tier: samples the maps
// only it binds (bind_material_extended.wgsl) with the view's mip bias. A
// program composes it or material_maps_basic.wgsl, exactly one, and calls
// it from surface_raster.wgsl alone. Each reads its map where its binding
// samples (material_map_uv) at a fragment whose first and second UV sets are
// `uv` and `uv1`.
// The anisotropy map's texel.
fn material_anisotropy_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec3<f32> {
 return material_map_sample(anisotropy_map,MATERIAL_SLOT_ANISOTROPY,material_map_uv(MATERIAL_SLOT_ANISOTROPY,uv,uv1),view.mip_bias).rgb;
}
// The clearcoat, clearcoat roughness, clearcoat normal, iridescence and
// iridescence thickness maps' texels.
fn material_clearcoat_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 return material_map_sample(clearcoat_map,MATERIAL_SLOT_CLEARCOAT,material_map_uv(MATERIAL_SLOT_CLEARCOAT,uv,uv1),view.mip_bias);
}
fn material_coat_roughness_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 return material_map_sample(coat_roughness_map,MATERIAL_SLOT_COAT_ROUGHNESS,material_map_uv(MATERIAL_SLOT_COAT_ROUGHNESS,uv,uv1),view.mip_bias);
}
fn material_coat_normal_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 return material_map_sample(coat_normal_map,MATERIAL_SLOT_COAT_NORMAL,material_map_uv(MATERIAL_SLOT_COAT_NORMAL,uv,uv1),view.mip_bias);
}
fn material_iridescence_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 return material_map_sample(iridescence_map,MATERIAL_SLOT_IRIDESCENCE,material_map_uv(MATERIAL_SLOT_IRIDESCENCE,uv,uv1),view.mip_bias);
}
fn material_iridescence_thickness_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 return material_map_sample(iridescence_thickness_map,MATERIAL_SLOT_IRIDESCENCE_THICKNESS,material_map_uv(MATERIAL_SLOT_IRIDESCENCE_THICKNESS,uv,uv1),view.mip_bias);
}
// The transmission map's texel.
fn material_transmission_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 return material_map_sample(transmission_map,MATERIAL_SLOT_TRANSMISSION,material_map_uv(MATERIAL_SLOT_TRANSMISSION,uv,uv1),view.mip_bias);
}
// The thickness map's texel.
fn material_thickness_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 return material_map_sample(thickness_map,MATERIAL_SLOT_THICKNESS,material_map_uv(MATERIAL_SLOT_THICKNESS,uv,uv1),view.mip_bias);
}
// The sheen colour and roughness and the diffuse transmission and its
// colour maps' texels.
fn material_sheen_color_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 return material_map_sample(sheen_color_map,MATERIAL_SLOT_SHEEN_COLOR,material_map_uv(MATERIAL_SLOT_SHEEN_COLOR,uv,uv1),view.mip_bias);
}
fn material_sheen_roughness_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 return material_map_sample(sheen_roughness_map,MATERIAL_SLOT_SHEEN_ROUGHNESS,material_map_uv(MATERIAL_SLOT_SHEEN_ROUGHNESS,uv,uv1),view.mip_bias);
}
fn material_diffuse_transmission_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 return material_map_sample(diffuse_transmission_map,MATERIAL_SLOT_DIFFUSE_TRANSMISSION,material_map_uv(MATERIAL_SLOT_DIFFUSE_TRANSMISSION,uv,uv1),view.mip_bias);
}
fn material_diffuse_transmission_color_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 return material_map_sample(diffuse_transmission_color_map,MATERIAL_SLOT_DIFFUSE_TRANSMISSION_COLOR,material_map_uv(MATERIAL_SLOT_DIFFUSE_TRANSMISSION_COLOR,uv,uv1),view.mip_bias);
}
