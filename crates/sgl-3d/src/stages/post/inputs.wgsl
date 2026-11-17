// What every post pass binds as group 0, and their full-screen vertex.
struct Output {
 @builtin(position) position:vec4<f32>,
 @location(0) uv:vec2<f32>,
}
// What bloom reads: Bevy's blend constant of its last upsample, the share of
// the completed scene the bloom replaces; and exp2(Exposure::stops), the
// exposure Bevy's scene already carries when its first downsample weighs it.
struct BloomSettings {
 composite_blend:f32,
 exposure:f32,
}
@group(0) @binding(0) var scene:texture_2d<f32>;
@group(0) @binding(1) var bloom:texture_2d<f32>;
@group(0) @binding(2) var linear_sampler:sampler;
@group(0) @binding(3) var<uniform> bloom_settings:BloomSettings;
@vertex fn vs(@builtin(vertex_index) id:u32)->Output {
 let p=fullscreen_corner(id);
 var o:Output;
 o.position=fullscreen_position(p);
 o.uv=vec2(p.x,1.-p.y);
 return o;
}
// The linear sampler's bilinear filter of `input` at `uv`, `offset` texels
// across, with every texel read through finite_scene_color: the hardware
// filter of a texel of +infinity gives infinity or NaN before any bound can
// act, so a filtered read of the completed scene whose result is not
// finite, which only an infinite or NaN texel makes it, is taken again
// through this. A NaN texel, outside the scene colour contract
// (shading/scene_color.wgsl), may still read as NaN.
fn bounded_filter(input:texture_2d<f32>,uv:vec2<f32>,offset:vec2<i32>)->vec4<f32> {
 let size=vec2<i32>(textureDimensions(input));
 let position=uv*vec2<f32>(size)-.5+vec2<f32>(offset);
 let corner=floor(position);
 let weight=position-corner;
 // The sampler clamps to the edge.
 let low=clamp(vec2<i32>(corner),vec2(0),size-1);
 let high=clamp(vec2<i32>(corner)+1,vec2(0),size-1);
 let top=mix(
  finite_scene_color(textureLoad(input,low,0)),
  finite_scene_color(textureLoad(input,vec2(high.x,low.y),0)),
  weight.x);
 let bottom=mix(
  finite_scene_color(textureLoad(input,vec2(low.x,high.y),0)),
  finite_scene_color(textureLoad(input,high,0)),
  weight.x);
 return mix(top,bottom,weight.y);
}
// `sampled`, the linear sampler's filter of `input` at `uv`, `offset`
// texels across, where it is finite, as it is wherever the footprint is
// finite, so a finite scene reads exactly as sampled; else
// bounded_filter's.
fn finite_filtered(input:texture_2d<f32>,sampled:vec4<f32>,uv:vec2<f32>,offset:vec2<i32>)->vec4<f32> {
 if all_finite(sampled) {
  return sampled;
 }
 return bounded_filter(input,uv,offset);
}
