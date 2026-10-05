// The key a slot of the slot table holds (shadow_mask_slots.wgsl). Reads
// `shadow_mask_slots`, which the program's bindings declare: the lighting
// pass's (bind_shadow_mask.wgsl) or the ray-traced shadow stage's own.
// The key slot `slot` holds: SHADOW_MASK_EMPTY, SHADOW_MASK_DIRECTIONAL or
// a scene light's index.
fn shadow_mask_slot_key(slot:u32)->u32 {
 return shadow_mask_slots.lights[shadow_mask_layer(slot)][shadow_mask_channel(slot)];
}
// The slot that holds `key`, RT_SHADOW_LIGHTS for none: a key is in at most
// one slot, so the table's four vectors are compared whole and the one
// that holds it gives its lane.
fn shadow_mask_slot_of(key:u32)->u32 {
 for (var layer=0u;layer<SHADOW_MASK_LAYERS;layer++) {
  let found=shadow_mask_slots.lights[layer]==vec4(key);
  if any(found) {
   return dot(select(vec4(0u),shadow_mask_layer_slots(layer),found),vec4(1u));
  }
 }
 return RT_SHADOW_LIGHTS;
}
