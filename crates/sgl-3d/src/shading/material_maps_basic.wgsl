// The material-map provider of the Basic binding tier, which binds none of
// the Extended tier's maps: each reads white, the texel an absent map reads.
// Their bits are clear on Basic, and a map whose white texel is not its
// neutral is read only with its bit. A program composes it or
// material_maps_extended.wgsl, exactly one.
fn material_anisotropy_texel(uv:vec2<f32>)->vec3<f32> {
 return vec3(1.);
}
fn material_clearcoat_texel(uv:vec2<f32>)->vec4<f32> {
 return vec4(1.);
}
fn material_coat_roughness_texel(uv:vec2<f32>)->vec4<f32> {
 return vec4(1.);
}
fn material_coat_normal_texel(uv:vec2<f32>)->vec4<f32> {
 return vec4(1.);
}
fn material_iridescence_texel(uv:vec2<f32>)->vec4<f32> {
 return vec4(1.);
}
fn material_iridescence_thickness_texel(uv:vec2<f32>)->vec4<f32> {
 return vec4(1.);
}
fn material_sheen_color_texel(uv:vec2<f32>)->vec4<f32> {
 return vec4(1.);
}
fn material_sheen_roughness_texel(uv:vec2<f32>)->vec4<f32> {
 return vec4(1.);
}
fn material_diffuse_transmission_texel(uv:vec2<f32>)->vec4<f32> {
 return vec4(1.);
}
fn material_diffuse_transmission_color_texel(uv:vec2<f32>)->vec4<f32> {
 return vec4(1.);
}
