// A volume's share of a receiver's indirect diffuse light by where the
// receiver lies on its lattice: whole within its extent, fading to nothing
// over the one lattice step past each face, as RTXGI's volume blend weight
// fades, so the receiver hands over to what follows the volume in the one
// determination (surface.wgsl) without a seam. The dynamic GI volume and the
// irradiance volume take it. `at` is the receiver's position in steps from
// the extent's least corner and `extent` the extent's size in steps.
fn volume_share(at:vec3<f32>,extent:vec3<f32>)->f32 {
 let beyond=max(-at,at-extent);
 let fade=saturate(1.-beyond);
 return fade.x*fade.y*fade.z;
}
