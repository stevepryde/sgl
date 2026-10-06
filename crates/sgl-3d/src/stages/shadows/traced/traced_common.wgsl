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
 // inverted, and its view; and the last submitted frame's view in this
 // frame's render frame (this frame's after a restart).
 inverse_view_projection:mat4x4<f32>,
 view:mat4x4<f32>,
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
// take the temporal blend alone. They are the packed visibility's word 0.
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
// A word's four slots' visibility: word w holds the slots of the mask's
// layer w (shadow_mask_layer_slots), channel i's in bits 8i to 8i + 7, so
// the passes read and write a word's four slots together, as the layer
// holds them in its channels.
const TRACED_CHANNEL_SHIFTS:vec4<u32>=vec4(0u,8u,16u,24u);
fn traced_unpack(word:u32)->vec4<f32> {
 return vec4<f32>((vec4(word)>>TRACED_CHANNEL_SHIFTS)&vec4(0xffu))/255.;
}
// A texel's four words' slots, word w's in column w.
fn traced_unpack_words(words:vec4<u32>)->mat4x4<f32> {
 return mat4x4(traced_unpack(words.x),traced_unpack(words.y),traced_unpack(words.z),traced_unpack(words.w));
}
fn traced_pack(visibility:vec4<f32>)->u32 {
 let bytes=vec4<u32>(round(saturate(visibility)*255.))<<TRACED_CHANNEL_SHIFTS;
 return bytes.x|bytes.y|bytes.z|bytes.w;
}
// `words` with slot `slot`'s visibility, which they held as zero, set to
// `visibility`.
fn traced_store(words:vec4<u32>,slot:u32,visibility:f32)->vec4<u32> {
 var stored=words;
 let word=shadow_mask_layer(slot);
 stored[word]|=traced_pack(select(vec4(0.),vec4(visibility),shadow_mask_layer_slots(word)==vec4(slot)));
 return stored;
}
