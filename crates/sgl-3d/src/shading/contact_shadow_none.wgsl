// The contact provider of every lit program but the opaque stage's
// lighting pass while contact shadows run (the architecture's Contact
// shadows): the fused pass, the lighting pass otherwise, probe captures,
// blended surfaces, ray hits and the fog, which take the directional
// light's shadow from the maps alone. A program composes exactly one
// provider (contact_shadow.wgsl being the other); it depends on the
// modules that call it, never they on it.
fn camera_contact_shadow(pixel:vec2<f32>)->f32 {
 return 1.;
}
