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
