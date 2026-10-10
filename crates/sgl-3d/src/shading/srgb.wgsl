// sRGB's transfer function (IEC 61966-2-1), each way. shading::srgb encodes
// and decodes on the CPU.
// The linear colour of sRGB-encoded `value`.
fn srgb_to_linear(value:vec3<f32>)->vec3<f32> {
 return select(pow((value+vec3(.055))/1.055,vec3(2.4)),value/12.92,value<=vec3(.04045));
}
// The sRGB encoding of linear `value`, at least 0.
fn linear_to_srgb(value:vec3<f32>)->vec3<f32> {
 return select(1.055*pow(value,vec3(1./2.4))-.055,value*12.92,value<=vec3(.0031308));
}
