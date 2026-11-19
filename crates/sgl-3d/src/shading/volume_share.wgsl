// A volume's share of a receiver's indirect diffuse light by where the
// receiver lies on its lattice: whole within its extent, fading to nothing
// over the one lattice step past each face, as RTXGI's volume blend weight
// fades, so the receiver hands over to what follows the volume in the one
// determination (surface.wgsl) without a seam. The dynamic GI volume and the
// irradiance volume take it, and give their light as a VolumeLight. `at` is
// the receiver's position in steps from the extent's least corner and
// `extent` the extent's size in steps.
fn volume_share(at:vec3<f32>,extent:vec3<f32>)->f32 {
 let beyond=max(-at,at-extent);
 let fade=saturate(1.-beyond);
 return fade.x*fade.y*fade.z;
}
// A volume's light at a receiver, which the one determination takes: its
// own light, irradiance / PI; its sky visibility a(n), the share of the sky
// that reaches the receiver, which occludes the sky's share of its
// environment specular (occlusion_environment); and the volume's share of
// the receiver's indirect diffuse light, 0 where the volume does not light
// the frame or reach the receiver, where its sky visibility is 1.
struct VolumeLight {
 light:vec3<f32>,
 sky_visibility:f32,
 share:f32,
}
