// Group 3 of the opaque stage's lighting pass while contact shadows run
// (the architecture's Contact shadows), which the opaque stage binds from
// what the contact shadow stage lends it: the contact mask, the
// directional light's visibility at each render pixel in its red channel.
// Its number stays clear of the blended group 3's and screen-space indirect
// light's, which the same program declares, and of the shadow mask's, which
// no program binds with it.
// Rust layout: shading::bind::contact_shadow.
@group(3) @binding(12) var contact_shadow_mask:texture_2d<f32>;
