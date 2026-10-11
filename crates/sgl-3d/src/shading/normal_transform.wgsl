// The normal `normal` under the linear part of `model`, unnormalised: its
// cofactor times the normal, which is inverse-transpose(model) times its
// determinant. For a surface's tangents a and b it is Ma × Mb, so it faces
// the transformed surface's winding, needs no inverse, and stays the
// flattened surface's normal where `model` scales an axis to zero, zero only
// where it collapses the normal's plane to a line or a point.
fn cofactor_normal(model:mat4x4<f32>,normal:vec3<f32>)->vec3<f32> {
 let x=model[0].xyz;
 let y=model[1].xyz;
 let z=model[2].xyz;
 return mat3x3(cross(y,z),cross(z,x),cross(x,y))*normal;
}
