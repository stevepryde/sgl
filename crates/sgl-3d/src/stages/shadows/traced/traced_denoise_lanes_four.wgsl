// The ray-traced shadow denoiser's lanes (denoise.rs `Shape`): the slots
// one invocation of AMD's port filters (ffx_denoiser_shadows_*.wgsl), a
// lane each of its values. Four here: slots 0 to 3, a texel's channels.
// traced_denoise_lanes_one.wgsl holds the other count; a program takes
// one of them.
alias FfxDnsrFloat=vec4<f32>;
alias FfxDnsrUint=vec4<u32>;
alias FfxDnsrBool=vec4<bool>;
// The lanes, and each lane's bit in a word of the group's votes.
const FFX_DNSR_LANES:u32=4u;
const FFX_DNSR_LANE_BITS:FfxDnsrUint=vec4(1u,2u,4u,8u);
fn ffx_dnsr_all(lanes:FfxDnsrBool)->bool {
 return all(lanes);
}
fn ffx_dnsr_or(lanes:FfxDnsrUint)->u32 {
 return lanes.x|lanes.y|lanes.z|lanes.w;
}
// The lanes from a texel's channels, slot s in channel s.
fn ffx_dnsr_uint(texel:vec4<u32>)->FfxDnsrUint {
 return texel;
}
fn ffx_dnsr_float(texel:vec4<f32>)->FfxDnsrFloat {
 return texel;
}
fn ffx_dnsr_bool(texel:vec4<bool>)->FfxDnsrBool {
 return texel;
}
// A texel's channels from the lanes.
fn ffx_dnsr_uint_texel(lanes:FfxDnsrUint)->vec4<u32> {
 return lanes;
}
fn ffx_dnsr_float_texel(lanes:FfxDnsrFloat)->vec4<f32> {
 return lanes;
}
