// The key a slot of the slot table holds (shadow_mask_slots.wgsl). Reads
// `shadow_mask_slots`, which the program's bindings declare: the lighting
// pass's (bind_shadow_mask.wgsl) or the ray-traced shadow stage's own.
// The key slot `slot` holds: SHADOW_MASK_EMPTY, SHADOW_MASK_DIRECTIONAL or
// a scene light's index.
fn shadow_mask_slot_key(slot:u32)->u32 {
 return shadow_mask_slots.lights[slot/4u][slot%4u];
}
