// The one triangle that covers the viewport, drawn as vertices 0..3 without
// vertex buffers: each vertex's corner in [0, 2] and its clip position.
// fullscreen_vs.wgsl holds the vertex entry point that only draws it.
fn fullscreen_corner(id:u32)->vec2<f32> {
 return vec2(f32((id<<1u)&2u),f32(id&2u));
}
fn fullscreen_position(corner:vec2<f32>)->vec4<f32> {
 return vec4(corner*2.-1.,0.,1.);
}
