// Group 3 of the opaque stage's lighting pass while ray-traced shadows run
// (the architecture's Ray-traced shadows), which the opaque stage binds
// from what the ray-traced shadow stage lends it: the shadow mask, each
// slot's visibility at each full-resolution pixel, and the slot table.
// Its numbers stay clear of the blended group 3's, which the same program
// declares. Rust layout: shading::bind::shadow_mask.
@group(3) @binding(8) var shadow_mask:texture_2d_array<f32>;
@group(3) @binding(9) var<uniform> shadow_mask_slots:ShadowMaskSlots;
