// Whether no component of `value` is infinite or NaN, judged by the
// exponent bits so no fast-math assumption removes the test.
fn all_finite(value:vec4<f32>)->bool {
 let exponent=bitcast<vec4<u32>>(value)&vec4(0x7f800000u);
 return all(exponent!=vec4(0x7f800000u));
}
