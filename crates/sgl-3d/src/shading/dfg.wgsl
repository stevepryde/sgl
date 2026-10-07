// The DFG table (lookup_tables.wgsl) at a receiver's N.V and perceptual
// roughness.
fn surface_dfg(nv:f32,rough:f32)->vec2<f32> {
 return lookup_dfg(lookup_tables,environment_sampler,nv,rough);
}
// The sheen lobe's directional albedo (lookup_sheen_albedo) at a receiver's
// N.V and sheen perceptual roughness.
fn surface_sheen_albedo(nv:f32,rough:f32)->f32 {
 return lookup_sheen_albedo(lookup_tables,environment_sampler,nv,rough);
}
