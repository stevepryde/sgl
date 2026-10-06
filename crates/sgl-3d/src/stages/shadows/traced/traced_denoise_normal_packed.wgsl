// Measurement (#204): traced_denoise_normal.wgsl for normals packed in a
// word (traced_normal_store_packed.wgsl): the G-buffer's octahedral base
// normal as two halves, decoded as the G-buffer decodes it.
@group(0) @binding(1) var denoise_normal:texture_2d<u32>;
fn traced_denoise_normal(p:vec2<u32>)->vec3<f32> {
 let word=textureLoad(denoise_normal,min(p,vec2<u32>(traced.reduced.xy)-1u),0).x;
 return gbuffer_octahedral_decode(unpack2x16float(word));
}
