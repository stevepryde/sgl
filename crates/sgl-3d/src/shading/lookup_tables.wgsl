// Lit group 0's lookup tables (scene/lookup_tables.rs), one 64×64 RGBA16F
// texture array: the GGX fit rectangle lights are shaded with
// (rect_light.wgsl) in two layers, its inverse matrix's four free elements,
// then its magnitude and Fresnel weights; and the 16×16 DFG table in red and
// green at the start of a third.
const LOOKUP_LTC_MATRIX_LAYER:i32=0;
const LOOKUP_LTC_WEIGHTS_LAYER:i32=1;
const LOOKUP_DFG_LAYER:i32=2;
// Texels on each side of the DFG table, and of a layer.
const LOOKUP_DFG_SIZE:f32=16.;
const LOOKUP_LAYER_SIZE:f32=64.;
// The DFG table at a receiver's N.V and perceptual roughness, filtered by
// `filtering` between the table's first and last texel centres.
fn lookup_dfg(tables:texture_2d_array<f32>,filtering:sampler,nv:f32,rough:f32)->vec2<f32> {
 let uv=clamp(vec2(rough,nv),vec2(.5/LOOKUP_DFG_SIZE),vec2(1.-.5/LOOKUP_DFG_SIZE))*(LOOKUP_DFG_SIZE/LOOKUP_LAYER_SIZE);
 return textureSampleLevel(tables,filtering,uv,LOOKUP_DFG_LAYER,0.).xy;
}
