// The ray-traced shadow denoiser's lanes (traced_denoise_lanes_four.wgsl):
// one here, slot 0, a texel's first channel, the others zero; AMD's port
// then runs as upstream, a light an invocation.
alias FfxDnsrFloat=f32;
alias FfxDnsrUint=u32;
alias FfxDnsrBool=bool;
const FFX_DNSR_LANES:u32=1u;
const FFX_DNSR_LANE_BITS:FfxDnsrUint=1u;
fn ffx_dnsr_all(lanes:FfxDnsrBool)->bool {
 return lanes;
}
fn ffx_dnsr_or(lanes:FfxDnsrUint)->u32 {
 return lanes;
}
fn ffx_dnsr_uint(texel:vec4<u32>)->FfxDnsrUint {
 return texel.x;
}
fn ffx_dnsr_float(texel:vec4<f32>)->FfxDnsrFloat {
 return texel.x;
}
fn ffx_dnsr_bool(texel:vec4<bool>)->FfxDnsrBool {
 return texel.x;
}
fn ffx_dnsr_uint_texel(lanes:FfxDnsrUint)->vec4<u32> {
 return vec4(lanes,0u,0u,0u);
}
fn ffx_dnsr_float_texel(lanes:FfxDnsrFloat)->vec4<f32> {
 return vec4(lanes,0.,0.,0.);
}
