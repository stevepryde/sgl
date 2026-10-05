// The mask provider of every lit program but the opaque stage's lighting
// pass while ray-traced shadows run (the architecture's Geometry
// pipelines): the fused pass, probe captures, blended surfaces, ray hits
// and the fog, which hold no slot and take every light's shadow from the
// maps. A program composes exactly one provider (shadow_mask.wgsl being
// the other); it depends on the modules that call it, never they on it.
fn camera_shadow_mask(key:u32,pixel:vec2<f32>)->f32 {
 return SHADOW_MASK_NO_SLOT;
}
