// The lit provider of the Basic binding tier, which binds none of lit group
// 0's Extended bindings (bind_lit_extended.wgsl): baked light takes no
// directional lobe, the all-zero lobe of a layer without one
// (baked_map_irradiance), and no dynamic GI volume lights a receiver, as in a
// frame the volume does not light (dynamic_gi_sample.wgsl). A program that
// composes surface.wgsl composes it or lit_extended.wgsl, exactly one.
fn baked_lightmap_direction(uv:vec2<f32>,layer:i32)->vec4<f32> {
 return vec4(0.);
}
fn baked_atlas_direction(uv:vec2<f32>,layer:i32)->vec4<f32> {
 return vec4(0.);
}
fn dynamic_gi_irradiance(position:vec3<f32>,normal:vec3<f32>,view:vec3<f32>,probe_hit:bool,moving:bool)->vec4<f32> {
 return vec4(0.);
}
