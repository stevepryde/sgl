// The occlusion and specular provider of a material that samples an
// occlusion map of its own or a specular or specular colour map, on the
// Extended binding tier (view::pipelines::Variant::occlusion_specular): each
// map's texel where its bit in the record's maps puts it in effect, else its
// fallback. A program composes it or material_occlusion_specular_none.wgsl,
// exactly one, so a material without those maps draws with programs that
// never reference their bindings. Each reads its map where its binding
// samples (material_more_map_sample) at a fragment whose first and second UV
// sets are `uv` and `uv1`, with the view's mip bias.
// The occlusion texel material_occlusion takes: its own occlusion map's,
// else the metallic-roughness texel `mr`, where packed occlusion lies.
fn material_occlusion_texel(uv:vec2<f32>,uv1:vec2<f32>,mr:vec4<f32>)->vec4<f32> {
 if (material.maps&MATERIAL_MAP_OCCLUSION)==0u {
  return mr;
 }
 return material_more_map_sample(occlusion_map,MATERIAL_SLOT_OCCLUSION,uv,uv1,view.mip_bias);
}
// The specular map's texel (its alpha the strength) and the specular colour
// map's (its RGB, sRGB), white without one.
fn material_specular_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 if (material.maps&MATERIAL_MAP_SPECULAR)==0u {
  return vec4(1.);
 }
 return material_more_map_sample(specular_map,MATERIAL_SLOT_SPECULAR,uv,uv1,view.mip_bias);
}
fn material_specular_color_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 if (material.maps&MATERIAL_MAP_SPECULAR_COLOR)==0u {
  return vec4(1.);
 }
 return material_more_map_sample(specular_color_map,MATERIAL_SLOT_SPECULAR_COLOR,uv,uv1,view.mip_bias);
}
