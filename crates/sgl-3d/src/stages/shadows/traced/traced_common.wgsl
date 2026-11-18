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
 // The slots from slot 0 the denoiser filtered this frame (denoise.rs
 // `Shape`): 4, or 1 for the directional light's alone. The temporal
 // blend takes those from it and blends word 0's others.
 denoised:u32,
 // 1 where the projection's clip w is the view depth (a perspective
 // projection), so the G-buffer's depth motion is a receiver's change of
 // linear depth; 0 where it carries none (an orthographic projection,
 // whose depth motion is 0).
 depth_motion:u32,
}
// The linear depth a texel the G-buffer drew nothing at records.
const TRACED_SKY_DEPTH:f32=1e30;
// Whether a tracing texel of linear depth `depth` holds a receiver: not the
// sky, nor a pixel the G-buffer drew nothing lit at, which the trace
// records as the sky (traced_pixel). The passes after weigh no other
// texel's visibility.
fn traced_holds_receiver(depth:f32)->bool {
 return depth<TRACED_SKY_DEPTH;
}
// The slots AMD's shadow denoiser may filter, as Wicked's first four: the
// directional light's and slots 1 to 3's local lights, each kept by the
// light that holds it, the packed visibility's word 0; which of them it filters a frame is
// TracedParams.denoised. The others take the temporal blend alone.
const TRACED_DENOISED_SLOTS:u32=4u;
// Wicked's upsample threshold: the weight lost per metre of linear depth
// between a tracing texel and a pixel, and the least weight; how close
// texel depth `a` is to `b`. A texel above the least weight is the pixel's
// surface, to the trace's choice of a block's pixel and to the upsample.
const TRACED_DEPTH_FALLOFF:f32=2.;
const TRACED_LEAST_WEIGHT:f32=.001;
fn traced_closeness(a:f32,b:f32)->f32 {
 return max(TRACED_LEAST_WEIGHT,1.-saturate(abs(a-b)*TRACED_DEPTH_FALLOFF));
}
// The pixels of a tracing texel's 2×2 block, by index: x, then y.
const TRACED_BLOCK_PIXELS:u32=4u;
// Pixel `index` (x, then y) of tracing texel `q`'s 2×2 block, within the
// render size.
fn traced_block_pixel(q:vec2<u32>,index:u32)->vec2<u32> {
 return min(q*2u+vec2(index&1u,index>>1u),vec2<u32>(traced.full.xy)-1u);
}
// The full-resolution pixel tracing texel `q` stands for, whose depth,
// normals and motion every pass takes for it: the pixel of its block the
// trace cast its rays from (traced_trace.wgsl traced_block_choice), pixel
// 2q but where that holds no receiver or the block holds a feature in
// front that the even pixels miss, whose index in the block the trace
// records in the fourth component of the texel's half-resolution normals,
// `record`.
fn traced_texel_pixel(q:vec2<u32>,record:vec4<f32>)->vec2<u32> {
 return traced_block_pixel(q,u32(record.w));
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
// The linear depth that the receiver of linear depth `depth` at world
// `position` had from the last submitted frame's view, which both history
// passes compare with the stage's own last-frame depth where its history
// lies: its depth now plus the G-buffer's depth motion `motion`
// (gbuffer.wgsl; NRD's viewZprev = viewZ + motion.z), which holds the
// camera's travel and the surface's own, so a surface the camera follows
// keeps its history as a still one does, as ambient occlusion's temporal
// pass and the reflections take it; 0, no history, where that depth motion
// is invalid (on or behind the previous camera). Under a projection whose
// clip w is no view depth (TracedParams.depth_motion 0, orthographic), the
// camera's reprojection of `position`, right for still surfaces alone.
fn traced_previous_depth(depth:f32,motion:vec4<f32>,position:vec3<f32>)->f32 {
 if traced.depth_motion==0u {
  return -(traced.previous_view*vec4(position,1.)).z;
 }
 return select(0.,depth+motion.z,motion.w>0.);
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
