// The contact provider of the opaque stage's lighting pass while contact
// shadows run (the architecture's Contact shadows): the contact shadow
// stage's visibility of the directional light with the frame's cascades
// at the camera's pixel, which directional_light_shadow takes the lesser
// of with the cascades' before the one shadow-opacity blend, as Godot
// b130438's scene shader takes `min(shadow, sscs_shadow)`
// (scene_forward_clustered.glsl 2632–2641; MIT, src/LICENSE-godot.txt).
// Every other lit program composes contact_shadow_none.wgsl instead; a
// program composes exactly one. It depends on the modules that call it,
// never they on it.
fn camera_contact_shadow(pixel:vec2<f32>)->f32 {
 return textureLoad(contact_shadow_mask,vec2<i32>(pixel),0).r;
}
