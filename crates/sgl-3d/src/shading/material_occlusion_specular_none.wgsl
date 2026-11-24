// The occlusion and specular provider of a material that samples none of an
// occlusion map of its own and the specular and specular colour maps, on
// either binding tier: their fallbacks, referencing none of their bindings,
// so its programs are those of a material before those maps existed. A
// program composes it or material_occlusion_specular.wgsl, exactly one.
// The occlusion texel material_occlusion takes: the metallic-roughness
// texel `mr`, where packed occlusion lies. A material with an occlusion map
// of its own is drawn with this provider only by passes that read no
// occlusion (view::pipelines::PipelineKey::new), where the value is unused.
fn material_occlusion_texel(uv:vec2<f32>,uv1:vec2<f32>,mr:vec4<f32>)->vec4<f32> {
 return mr;
}
// The specular and specular colour maps' texels: white, their neutral.
fn material_specular_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 return vec4(1.);
}
fn material_specular_color_texel(uv:vec2<f32>,uv1:vec2<f32>)->vec4<f32> {
 return vec4(1.);
}
