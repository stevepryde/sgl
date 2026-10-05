// The mask provider of the opaque stage's lighting pass while ray-traced
// shadows run (the architecture's Geometry pipelines): the ray-traced
// shadow stage's visibility of a light at the camera's pixel, where a slot
// holds the light, which the lit library takes through the one
// shadow-opacity blend in place of the light's map, as Wicked Engine
// 2ff1d9e's lighting multiplies a light by its mask under
// SHADOW_MASK_ENABLED and never for TRANSPARENT (lightingHF.hlsli 58–85;
// MIT, src/LICENSE-wicked.txt). Every other lit program composes
// shadow_mask_none.wgsl instead; a program composes exactly one. It
// depends on the modules that call it, never they on it.
// The visibility of the light `key` names (SHADOW_MASK_DIRECTIONAL, or a
// scene light's index) at the camera's `pixel`, or SHADOW_MASK_NO_SLOT
// where no slot holds it.
fn camera_shadow_mask(key:u32,pixel:vec2<f32>)->f32 {
 for (var slot=0u;slot<RT_SHADOW_LIGHTS;slot++) {
  if shadow_mask_slot_key(slot)==key {
   let texel=textureLoad(shadow_mask,vec2<i32>(pixel),i32(shadow_mask_layer(slot)),0);
   return texel[shadow_mask_channel(slot)];
  }
 }
 return SHADOW_MASK_NO_SLOT;
}
