// A rasterized texel's base colour: the material's base times the vertex
// colour and the base map, sampled with the view's mip bias. Every raster
// pass that reads it, lit and caster alike, takes it from here.
fn material_base_color(uv:vec2<f32>,color:vec4<f32>)->vec4<f32> {
 return material.base*color*textureSampleBias(base_map,tex_sampler,uv,view.mip_bias);
}
// A rasterized texel's material maps, white where a view samples none: its
// base colour (material_base_color), its emission map's colour and its
// metallic-roughness, clearcoat, clearcoat roughness, transmission and
// thickness maps' texels.
struct MaterialTexels {
 base_color:vec4<f32>,
 emission:vec3<f32>,
 metallic_roughness:vec4<f32>,
 coat:vec4<f32>,
 coat_roughness:vec4<f32>,
 transmission:vec4<f32>,
 thickness:vec4<f32>,
}
// The texels of a view that samples the base map alone (a masked caster).
fn material_base_texels(uv:vec2<f32>,color:vec4<f32>)->MaterialTexels {
 return MaterialTexels(material_base_color(uv,color),vec3(1.),vec4(1.),vec4(1.),vec4(1.),vec4(1.),vec4(1.));
}
// The material's surface (shader_contract.wgsl's MaterialSurface) at
// texels `t` with the mapped normal `normal`: each value its record's
// factor times its map's channel (material.wgsl), as a material's shader
// function receives it.
fn material_texel_surface(t:MaterialTexels,normal:vec3<f32>)->MaterialSurface {
 let mr=t.metallic_roughness;
 return MaterialSurface(t.base_color,material.emission*t.emission,material.metallic*mr.b,material.roughness*mr.g,normal,material_occlusion(material,mr),material.specular,material_coat(material,t.coat),material_coat_roughness(material,t.coat_roughness),material_transmission(material,t.transmission),material_thickness(material,t.thickness),material.attenuation,material.ior,material.dispersion);
}
// Whether the pipeline draws a masked material, whose fragments discard the
// texels it cuts out: a pipeline constant (view::pipelines::Alpha), so
// pipelines of opaque materials have no discard and keep early depth.
override alpha_mask:bool=false;
// Discards a fragment of base alpha `alpha` that the masked material cuts
// out (material_cut_out). Called after a fragment's last derivative.
fn material_alpha_discard(alpha:f32) {
 if alpha_mask && material_cut_out(material,alpha) {
  discard;
 }
}
