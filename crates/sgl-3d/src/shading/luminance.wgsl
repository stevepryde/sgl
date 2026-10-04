// Rec. 709 luminance of linear RGB.
fn luminance(color:vec3<f32>)->f32 {
 return dot(color,vec3(.2126,.7152,.0722));
}
