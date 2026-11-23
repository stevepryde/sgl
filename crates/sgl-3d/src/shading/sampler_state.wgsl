// A material map's sampler state (asset::Sampler), one byte of the record's
// map_samplers: U's wrap mode in bits 0-1 and V's in 2-3, linear
// magnification and minification at 16 and 32, and the mipmap filter in
// bits 6-7. Rust owner: shading::samplers, which encodes it.
const SAMPLER_WRAP_REPEAT:u32=0u;
const SAMPLER_WRAP_MIRRORED_REPEAT:u32=1u;
const SAMPLER_WRAP_CLAMP_TO_EDGE:u32=2u;
const SAMPLER_MAG_LINEAR:u32=16u;
const SAMPLER_MIN_LINEAR:u32=32u;
const SAMPLER_MIP_SHIFT:u32=6u;
const SAMPLER_MIP_NONE:u32=0u;
const SAMPLER_MIP_NEAREST:u32=1u;
const SAMPLER_MIP_LINEAR:u32=2u;
// State `state`'s wrap mode along `axis`, 0 for U and 1 for V.
fn sampler_wrap_mode(state:u32,axis:u32)->u32 {
 return (state>>(axis*2u))&3u;
}
fn sampler_mip_filter(state:u32)->u32 {
 return (state>>SAMPLER_MIP_SHIFT)&3u;
}
// The four texels a linear filter blends at `p`, in texels (Vulkan, Texel
// Linear Filtering): the first's coordinate, the others one step along
// each axis, and the share of the second along each axis.
struct SamplerLinear {
 texel:vec2<i32>,
 weight:vec2<f32>,
}
fn sampler_linear(p:vec2<f32>)->SamplerLinear {
 let q=p-.5;
 return SamplerLinear(vec2<i32>(floor(q)),q-floor(q));
}
// Texel `p` of a level `size` texels long, wrapped by `mode` (glTF 2.0
// 3.8.4, the Vulkan specification's wrapping operation): repeated, repeated
// with every other copy mirrored, or clamped to the edge. The one wrap
// formula of the shader-sampled maps and ray hits.
fn sampler_wrap(p:i32,size:i32,mode:u32)->i32 {
 if mode==SAMPLER_WRAP_CLAMP_TO_EDGE {
  return clamp(p,0,size-1);
 }
 if mode==SAMPLER_WRAP_MIRRORED_REPEAT {
  let q=((p%(size*2))+size*2)%(size*2);
  return min(q,size*2-1-q);
 }
 return ((p%size)+size)%size;
}
