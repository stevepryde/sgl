// The DFG table (lookup_tables.wgsl) at a receiver's N.V and perceptual
// roughness.
fn surface_dfg(nv:f32,rough:f32)->vec2<f32> {
 return lookup_dfg(lookup_tables,environment_sampler,nv,rough);
}
