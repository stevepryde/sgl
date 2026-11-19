// Group 3 of the opaque stage's fused and lighting passes while
// screen-space indirect light has a kept result, which the opaque stage
// binds from its pass: the kept result of the last finished frame and the
// view it was kept in (screen_indirect.wgsl). Its numbers stay clear of the
// blended group 3's and the shadow mask's, which the same program declares.
// Rust layout: shading::bind::screen_indirect.
@group(3) @binding(10) var screen_indirect_kept:texture_2d<u32>;
@group(3) @binding(11) var<uniform> screen_indirect_view:ScreenIndirectView;
