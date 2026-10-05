// sRGB's transfer function (IEC 61966-2-1), decoding: the linear colour of
// sRGB-encoded `value`. shading::srgb encodes and decodes on the CPU.
fn srgb_to_linear(value:vec3<f32>)->vec3<f32> {
 return select(pow((value+vec3(.055))/1.055,vec3(2.4)),value/12.92,value<=vec3(.04045));
}
