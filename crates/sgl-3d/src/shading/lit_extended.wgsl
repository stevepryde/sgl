// The lit provider of the Extended binding tier: reads lit group 0's
// bindings only it binds (bind_lit_extended.wgsl). A program that composes
// surface.wgsl composes it or lit_basic.wgsl, exactly one; with it comes the
// dynamic GI volume's irradiance (dynamic_gi_sample.wgsl).
// The lightmap's directional lobe at `uv` in layer `layer`.
fn baked_lightmap_direction(uv:vec2<f32>,layer:i32)->vec4<f32> {
 return textureSampleLevel(static_lightmap_direction,baked_sampler,uv,layer,0.);
}
// The irradiance atlas's directional lobe at `uv` in layer `layer`.
fn baked_atlas_direction(uv:vec2<f32>,layer:i32)->vec4<f32> {
 return textureSampleLevel(static_direction_atlas,baked_sampler,uv,layer,0.);
}
