// Shared by the ray-traced shadow stage's passes (stages/shadows/traced.rs):
// its parameters, the camera's reconstruction from the G-buffer's depth,
// and Wicked Engine 2ff1d9e's 8-bit packing of a slot's visibility, four
// slots a word of an RGBA32Uint texel (screenspaceshadowCS.hlsl 298–301,
// rtshadow_denoise_temporalCS.hlsl load_shadow and store_shadow; MIT,
// src/LICENSE-wicked.txt). Changed: a visibility packs rounded to the
// nearest step, where Wicked's truncates, so a temporal blend that keeps
// re-packing its history does not drift down a step at a time.
struct TracedParams {
 // The camera's view-projection as it rasterized the G-buffer (jittered),
 // inverted, and its view; its projection as it rasterized, inverted; and
 // the last submitted frame's view in this frame's render frame (this
 // frame's after a restart).
 inverse_view_projection:mat4x4<f32>,
 view:mat4x4<f32>,
 inverse_projection:mat4x4<f32>,
 previous_view:mat4x4<f32>,
 // The full (render) and the tracing resolutions: width, height,
 // 1/width, 1/height.
 full:vec4<f32>,
 reduced:vec4<f32>,
 // The camera's position in xyz.
 eye:vec4<f32>,
 // Frames since the stage's history restarted; 0 restarts every slot.
 frame:u32,
 // Turns the rays' draws on the lights from frame to frame.
 seed:u32,
}
// The linear depth a texel the G-buffer drew nothing at records.
const TRACED_SKY_DEPTH:f32=1e30;
// The slots AMD's shadow denoiser filters, as Wicked's first four: the
// directional light and the three longest-held local lights. The others
// take the temporal blend alone.
const TRACED_DENOISED_SLOTS:u32=4u;
// The full-resolution pixel whose depth and normals tracing pixel `q`
// takes: pixel 2q, within the render size.
fn traced_full_pixel(q:vec2<u32>)->vec2<u32> {
 return min(q*2u,vec2<u32>(traced.full.xy)-1u);
}
// Wicked's globals.hlsli reconstruct_position: the world position of the
// G-buffer's device depth `z` at full-resolution `uv`.
fn traced_position(uv:vec2<f32>,z:f32)->vec3<f32> {
 let h=traced.inverse_view_projection*vec4(uv.x*2.-1.,(1.-uv.y)*2.-1.,z,1.);
 return h.xyz/h.w;
}
// Metres along the camera's view axis to `position`, whatever its
// projection.
fn traced_linear_depth(position:vec3<f32>)->f32 {
 return -(traced.view*vec4(position,1.)).z;
}
// Slot `slot`'s visibility in `words`.
fn traced_load(words:vec4<u32>,slot:u32)->f32 {
 let shift=(slot%4u)*8u;
 return f32((words[slot/4u]>>shift)&0xffu)/255.;
}
// `words` with slot `slot`'s visibility, which they held as zero, set to
// `visibility`.
fn traced_store(words:vec4<u32>,slot:u32,visibility:f32)->vec4<u32> {
 var stored=words;
 let shift=(slot%4u)*8u;
 stored[slot/4u]|=u32(round(saturate(visibility)*255.))<<shift;
 return stored;
}
