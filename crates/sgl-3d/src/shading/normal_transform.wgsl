// Cofactor(model) is inverse-transpose(model) times its determinant. Only
// normal direction survives interpolation; remove the determinant sign here.
// It needs no inverse, so a singular `model` (an axis scaled to zero) still
// gives the flattened surface's normal: cofactor(M)(a × b) = Ma × Mb for
// the surface's tangents a and b, zero only where the surface collapses.
fn object_normal(model:mat4x4<f32>,normal:vec3<f32>)->vec3<f32> {
 let x=model[0].xyz;
 let y=model[1].xyz;
 let z=model[2].xyz;
 let cofactor=mat3x3(cross(y,z),cross(z,x),cross(x,y));
 return (cofactor*normal)*select(1.,-1.,dot(x,cofactor[0])<0.);
}
