// Measurement (#204): traced_normal_store.wgsl in a word, the width
// Wicked keeps its normals copy at (R11G11B10): the G-buffer's octahedral
// base normal as two halves, as the G-buffer holds it.
@group(3) @binding(8) var traced_half_normal:texture_storage_2d<r32uint,write>;
fn traced_store_normal(q:vec2<u32>,encoded:vec2<f32>,normal:vec3<f32>) {
 textureStore(traced_half_normal,q,vec4(pack2x16float(encoded),0u,0u,0u));
}
