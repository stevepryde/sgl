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
 // inverted, and its view.
 inverse_view_projection:mat4x4<f32>,
 view:mat4x4<f32>,
 // The full (render) and the tracing resolutions: width, height,
 // 1/width, 1/height.
 full:vec4<f32>,
 reduced:vec4<f32>,
 // Frames since the stage's history restarted; 0 restarts every slot.
 frame:u32,
}
// The linear depth a texel the G-buffer drew nothing at records.
const TRACED_SKY_DEPTH:f32=1e30;
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
// A word's four slots' visibility, slot 4w + i in bits 8i to 8i + 7 of
// word w: the passes read and write a word's four slots together, as the
// mask's layer w holds them in its channels.
fn traced_unpack(word:u32)->vec4<f32> {
 return vec4<f32>((vec4(word)>>vec4(0u,8u,16u,24u))&vec4(0xffu))/255.;
}
// A texel's four words' slots, word w's in column w.
fn traced_unpack_words(words:vec4<u32>)->mat4x4<f32> {
 return mat4x4(traced_unpack(words.x),traced_unpack(words.y),traced_unpack(words.z),traced_unpack(words.w));
}
fn traced_pack(visibility:vec4<f32>)->u32 {
 let bytes=vec4<u32>(round(saturate(visibility)*255.))<<vec4(0u,8u,16u,24u);
 return bytes.x|bytes.y|bytes.z|bytes.w;
}
// `words` with slot `slot`'s visibility, which they held as zero, set to
// `visibility`.
fn traced_store(words:vec4<u32>,slot:u32,visibility:f32)->vec4<u32> {
 var stored=words;
 stored[slot/4u]|=traced_pack(select(vec4(0.),vec4(visibility),vec4(slot%4u)==vec4(0u,1u,2u,3u)));
 return stored;
}
