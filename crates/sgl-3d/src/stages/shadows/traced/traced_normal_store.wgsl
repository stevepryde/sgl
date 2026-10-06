// The trace's copy of each tracing pixel's shading normal for the
// denoiser, Wicked's half-resolution normals copy: the normal as it is.
@group(3) @binding(8) var traced_half_normal:texture_storage_2d<rgba16float,write>;
// Stores tracing pixel `q`'s shading normal `normal`, whose G-buffer
// encoding is `encoded`; zeros for a pixel that is no receiver.
fn traced_store_normal(q:vec2<u32>,encoded:vec2<f32>,normal:vec3<f32>) {
 textureStore(traced_half_normal,q,vec4(normal,0.));
}
