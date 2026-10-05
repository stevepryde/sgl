// The ray-traced shadow stage's slots and its mask's layout (the
// architecture's Ray-traced shadows), which the stage writes and the
// opaque stage's lighting pass reads (shadow_mask.wgsl). Rust mirror:
// shading::shadow_mask; the layout test compares the two.
//
// The mask holds RT_SHADOW_LIGHTS slots, Wicked Engine 2ff1d9e's
// MAX_RTSHADOWS (screenspaceshadowCS.hlsl 9; MIT, src/LICENSE-wicked.txt):
// slot 0 the directional light with the frame's cascades, slots 1 to 15
// casting scene lights. At full resolution it is an Rgba8Unorm array of
// four layers, slot s in channel s % 4 of layer s / 4, where Wicked's
// R8_UNORM array, not a wgpu storage format, holds one slot a layer.
const RT_SHADOW_LIGHTS:u32=16u;
// The mask's layers, four slots a layer.
const SHADOW_MASK_LAYERS:u32=RT_SHADOW_LIGHTS/4u;
// A slot no light holds.
const SHADOW_MASK_EMPTY:u32=0xffffffffu;
// Slot 0's key while the frame's shadowed directional light (the one with
// DIRECTIONAL_LIGHT_SHADOW, which has the cascades) holds it; slots 1 to
// 15 hold scene lights by their index.
const SHADOW_MASK_DIRECTIONAL:u32=0xfffffffeu;
// What a mask provider (camera_shadow_mask) returns for a light no slot
// holds: the light takes its shadow from the maps.
const SHADOW_MASK_NO_SLOT:f32=-1.;
// The slot table: each slot's key, four to a vector, and the slots whose
// history restarts this frame because another light took them.
struct ShadowMaskSlots {
 lights:array<vec4<u32>,4>,
 // Bit s: slot s's history restarts.
 restart:u32,
}
// The mask's layer that holds slot `slot`, and its channel there; and the
// slots layer `layer` holds, by channel. The slot table's vectors and the
// stage's packed words hold the slots as the layers do.
fn shadow_mask_layer(slot:u32)->u32 {
 return slot/4u;
}
fn shadow_mask_channel(slot:u32)->u32 {
 return slot%4u;
}
fn shadow_mask_layer_slots(layer:u32)->vec4<u32> {
 return vec4(layer*4u)+vec4(0u,1u,2u,3u);
}
