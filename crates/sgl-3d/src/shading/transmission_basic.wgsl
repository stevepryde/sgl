// The transmission provider of the Basic binding tier, which binds no copy
// of the frame: none is held, so a transmissive surface takes the fallback,
// the light behind it blended through unrefracted (transmission.wgsl); nor
// any volume layer, whose passes the Basic tier never creates. A program
// composes it or transmission_extended.wgsl, exactly one.
fn transmission_frame_held()->bool {
 return false;
}
fn transmission_frame_size()->vec2<f32> {
 return vec2(1.);
}
fn transmission_frame_sample(uv:vec2<f32>,lod:f32)->vec4<f32> {
 return vec4(0.);
}
// The farthest depth (reversed-Z), behind which a second exit pass, which
// this tier never creates, would keep nothing.
fn volume_exit_layer(texel:vec2<i32>)->f32 {
 return 0.;
}
