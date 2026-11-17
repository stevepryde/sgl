// Whether a value is neither infinite nor NaN, judged by its exponent bits:
// a comparison such as abs(v)<=f32::MAX or v==v is one a compiler under fast
// math (Metal's default, which wgpu-hal leaves) may assume true of every
// value and fold away.
const FINITE_EXPONENT_BITS:u32=0x7f800000u;
fn finite(value:f32)->bool {
 return (bitcast<u32>(value)&FINITE_EXPONENT_BITS)!=FINITE_EXPONENT_BITS;
}
// Whether no component of `value` is infinite or NaN. A narrower vector
// passes its components with finite padding, e.g. vec4(v,0.).
fn all_finite(value:vec4<f32>)->bool {
 let exponent=bitcast<vec4<u32>>(value)&vec4(FINITE_EXPONENT_BITS);
 return all(exponent!=vec4(FINITE_EXPONENT_BITS));
}
