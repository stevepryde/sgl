// Interleaved gradient noise (Jimenez 2014, "Next Generation Post Processing
// in Call of Duty: Advanced Warfare"), varied per frame.
//
// Ports Bevy 9d12036 crates/bevy_pbr/src/render/utils.wesl
// interleaved_gradient_noise, MIT OR Apache-2.0 (src/LICENSE-bevy.txt).
fn interleaved_gradient_noise(pixel_coordinates:vec2<f32>,frame:u32)->f32 {
 let xy=pixel_coordinates+5.588238*f32(frame%64u);
 return fract(52.9829189*fract(0.06711056*xy.x+0.00583715*xy.y));
}
