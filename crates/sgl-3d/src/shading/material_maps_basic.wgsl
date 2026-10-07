// The material-map provider of the Basic binding tier, which binds none of
// the Extended tier's maps: each reads white, the texel an absent map reads.
// Their bits are clear on Basic, and a map whose white texel is not its
// neutral is read only with its bit. A program composes it or
// material_maps_extended.wgsl, exactly one.
// The anisotropy map's texel at `uv`.
fn material_anisotropy_texel(uv:vec2<f32>)->vec3<f32> {
 return vec3(1.);
}
