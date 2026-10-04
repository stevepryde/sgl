// A local-light shadow face's static layer, copied into the frame's atlas
// texel for texel: the two atlases share one layout. Depth textures copy only
// whole, so each face's copy is a draw over its viewport (fullscreen_vs),
// as HDRP blits its cached shadow atlas.
@group(0) @binding(0) var static_layers:texture_depth_2d;
@fragment fn copy_fs(@builtin(position) position:vec4<f32>)->@builtin(frag_depth) f32 {
 return textureLoad(static_layers,vec2<i32>(position.xy),0);
}
