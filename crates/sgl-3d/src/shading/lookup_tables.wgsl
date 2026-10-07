// Lit group 0's lookup tables (scene/lookup_tables.rs), one 64×64 RGBA16F
// texture array: the GGX fit rectangle lights are shaded with
// (rect_light.wgsl) in two layers, its inverse matrix's four free elements,
// then its magnitude and Fresnel weights; and in a third the DFG table, the
// split sum's scale and bias in red and green over N.V across and perceptual
// roughness down, Bevy 9d12036's, and in blue the sheen lobe's directional
// albedo there, Filament ef1a133's (scene/lookup_tables.rs).
const LOOKUP_LTC_MATRIX_LAYER:i32=0;
const LOOKUP_LTC_WEIGHTS_LAYER:i32=1;
const LOOKUP_DFG_LAYER:i32=2;
// Texels on each side of a layer.
const LOOKUP_LAYER_SIZE:f32=64.;
// The DFG table at a receiver's N.V and perceptual roughness, filtered by
// `filtering` between the table's first and last texel centres, which
// hold N.V and roughness of 0.5/64 and 63.5/64 (Bevy's F_AB samples it at
// (N.V, roughness) under a clamping sampler).
fn lookup_dfg(tables:texture_2d_array<f32>,filtering:sampler,nv:f32,rough:f32)->vec2<f32> {
 let uv=clamp(vec2(nv,rough),vec2(.5/LOOKUP_LAYER_SIZE),vec2(1.-.5/LOOKUP_LAYER_SIZE));
 return textureSampleLevel(tables,filtering,uv,LOOKUP_DFG_LAYER,0.).xy;
}
// The sheen lobe's directional albedo E (sheen.wgsl) at a receiver's N.V and
// sheen perceptual roughness, filtered as lookup_dfg filters the DFG table:
// Filament ef1a133's prefilteredDFG(...).z (shaders/src/
// surface_shading_lit.fs 275).
fn lookup_sheen_albedo(tables:texture_2d_array<f32>,filtering:sampler,nv:f32,rough:f32)->f32 {
 let uv=clamp(vec2(nv,rough),vec2(.5/LOOKUP_LAYER_SIZE),vec2(1.-.5/LOOKUP_LAYER_SIZE));
 return textureSampleLevel(tables,filtering,uv,LOOKUP_DFG_LAYER,0.).z;
}
