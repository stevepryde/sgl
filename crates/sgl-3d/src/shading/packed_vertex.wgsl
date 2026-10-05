// The scene's packed vertex (the architecture's Vertex encoding): eight words
// in the ray source. Rust owner and encoder: shading::packed_vertex. A
// reader loads the words and decodes them here: the position is three f32;
// each 16-bit pair holds its first value in the low half.
//
// Godot b130438's decoding (MIT, see LICENSE-godot.txt):
// Vector3::octahedron_decode (core/math/vector3.cpp) and axis_angle_to_tbn
// and _unpack_vertex_attributes
// (servers/rendering/renderer_rd/shaders/forward_clustered/scene_forward_clustered.glsl).
const PACKED_VERTEX_WORDS:u32=8u;
const PACKED_VERTEX_POSITION:u32=0u;
const PACKED_VERTEX_AXIS:u32=3u;
const PACKED_VERTEX_ANGLE_CHART:u32=4u;
const PACKED_VERTEX_UV:u32=5u;
const PACKED_VERTEX_COLOR:u32=6u;
const PACKED_VERTEX_LIGHTMAP_UV:u32=7u;
// A vertex's frame: its normal, and its tangent with the bitangent's
// handedness in w (bitangent = cross(normal, tangent) * w).
struct PackedFrame {
 normal:vec3<f32>,
 tangent:vec4<f32>,
}
// The unit vector at `encoded` on Godot's octahedron (0..1 on each axis).
fn packed_vertex_octahedral(encoded:vec2<f32>)->vec3<f32> {
 let f=encoded*2.-vec2(1.);
 var n=vec3(f,1.-abs(f.x)-abs(f.y));
 let fold=clamp(-n.z,0.,1.);
 n.x+=select(fold,-fold,n.x>=0.);
 n.y+=select(fold,-fold,n.y>=0.);
 return normalize(n);
}
// The frame of a vertex's `axis` and `angle_chart` words: the rows of the
// rotation by the angle about the axis are its tangent, bitangent and
// normal; the angle's half holds the handedness.
fn packed_vertex_frame(axis:u32,angle_chart:u32)->PackedFrame {
 let rotation_axis=packed_vertex_octahedral(unpack2x16unorm(axis));
 let code=f32(angle_chart&0xffffu)/65535.;
 let handedness=select(-1.,1.,code>.5);
 let angle=abs(code*2.-1.)*3.14159265358979;
 let c=cos(angle);
 let s=sin(angle);
 let complement=(1.-c)*rotation_axis;
 let sine_axis=s*rotation_axis;
 let tangent=complement.x*rotation_axis+vec3(c,-sine_axis.z,sine_axis.y);
 let normal=complement.z*rotation_axis+vec3(-sine_axis.y,sine_axis.x,c);
 return PackedFrame(normal,vec4(tangent,handedness));
}
// The lightmap chart's index in its model's table.
fn packed_vertex_chart(angle_chart:u32)->u32 {
 return angle_chart>>16u;
}
// The UV of `uv` across its mesh's rectangle `rect` (min in xy, extent in
// zw).
fn packed_vertex_uv(uv:u32,rect:vec4<f32>)->vec2<f32> {
 return rect.xy+unpack2x16unorm(uv)*rect.zw;
}
// The linear colour and alpha of `color`, whose colour is sRGB-encoded.
fn packed_vertex_color(color:u32)->vec4<f32> {
 let encoded=unpack4x8unorm(color);
 return vec4(srgb_to_linear(encoded.rgb),encoded.a);
}
fn packed_vertex_lightmap_uv(lightmap_uv:u32)->vec2<f32> {
 return unpack2x16unorm(lightmap_uv);
}
