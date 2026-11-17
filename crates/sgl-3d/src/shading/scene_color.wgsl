// The scene colour as the passes after composition read it: motion blur,
// post and the transmission copy here, TAA and FSR through their own ports'
// clamps; and as composition (reflection source completion) reads the lit
// colour and the ambient light within it, which hold +infinity alike. The
// composed frame is RGBA16F radiance with no upper bound where it is
// written, so a texel holds +infinity where radiance passed binary16's range
// on a backend that rounds overflow to infinity (#464).
// Read through finite_scene_color, it is binary16's largest finite value,
// as Metal stores that overflow and as FSR2's PrepareRgb and sgl-post-fx's
// SampleCurrColor (DFX-46) read their input. Every finite RGBA16F value
// reads unchanged. The bound is for infinity alone: a NaN texel is a defect
// of whatever wrote it, which no single value would correct, and WGSL
// leaves min of NaN undefined, so it is not mapped to anything here.
const SCENE_COLOR_MAX:f32=65504.;
fn finite_scene_color(color:vec4<f32>)->vec4<f32> {
 return min(color,vec4(SCENE_COLOR_MAX));
}
