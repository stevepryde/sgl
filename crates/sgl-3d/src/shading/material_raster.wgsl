// A rasterized texel's base colour: the material's base times the vertex
// colour and the base map, sampled with the view's mip bias. Every raster
// pass that reads it, lit and caster alike, takes it from here.
fn material_base_color(uv:vec2<f32>,color:vec4<f32>)->vec4<f32> {
 return material.base*color*textureSampleBias(base_map,tex_sampler,uv,view.mip_bias);
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
