// A material's map coordinates (specs/sgl3d-architecture.md, Material map
// coordinates): which UV set each map binding samples and the transform it
// samples it through, KHR_texture_transform's. Rust owner and encoder:
// shading::map_uvs. Raster resolves a binding's coordinates with
// material_map_uv (material_sampling.wgsl) and ray hits with scene_map_uv
// (scene_rays.wgsl), both through map_uv_transform.
//
// Raster's pipeline constants (view::pipelines::Variant): whether a map
// binding the pass samples takes the second UV set, and whether one takes a
// transform. Where either is false the code for it compiles out, so a
// material whose maps all sample the first set untransformed draws as if
// neither existed; where it is true, each binding's bit in the record's
// map_uv_bits selects (material_map_uv). A vertex stage reads the second
// set only where material_uv1_enabled is true.
override material_uv1_enabled:bool=false;
override material_transforms_enabled:bool=false;
// A record's map_uv_bits: a bit by MATERIAL_SLOT_* in its low half for each
// map binding on the second set, and from this shift in its high half for
// each whose transform is not the identity.
const MAP_UV_TRANSFORMED_SHIFT:u32=16u;
// One map binding's transform: the rows of the matrix
// translation * rotation * scale, so u' = dot(u.xy, uv) + u.z and
// v' = dot(v.xy, uv) + v.z.
struct MapUv {
 u:vec3<f32>,
 v:vec3<f32>,
}
// `uv` through `t`.
fn map_uv_transform(t:MapUv,uv:vec2<f32>)->vec2<f32> {
 return vec2(dot(t.u.xy,uv)+t.u.z,dot(t.v.xy,uv)+t.v.z);
}
// How a surface's position moves along a map's coordinates: along its U and
// along its V.
struct MapUvFrame {
 du:vec3<f32>,
 dv:vec3<f32>,
}
// The position derivatives along the coordinates `t` gives, from those
// along the coordinates it takes, `du` and `dv`: [du dv] times the inverse
// of t's linear part, so a rotation turns them and a negative scale mirrors
// them. A transform that collapses an axis has no inverse and leaves them.
fn map_uv_frame(t:MapUv,du:vec3<f32>,dv:vec3<f32>)->MapUvFrame {
 let determinant=t.u.x*t.v.y-t.u.y*t.v.x;
 if determinant==0. {
  return MapUvFrame(du,dv);
 }
 return MapUvFrame((du*t.v.y-dv*t.v.x)/determinant,(dv*t.u.x-du*t.u.y)/determinant);
}
// The authored tangent frame `authored` (glTF's: T along increasing U and
// B = (N x T) w toward decreasing V, the normal texture's +Y) carried
// through `t`: the same frame of the coordinates `t` gives, T' along
// increasing U' and B' toward decreasing V'. B is -dp/dv, so the position
// derivatives (T, -B) are carried (map_uv_frame) and their dual taken
// (map_uv_relief_frame).
fn map_uv_authored_frame(t:MapUv,authored:mat3x3<f32>)->mat3x3<f32> {
 let carried=map_uv_frame(t,authored[0],-authored[1]);
 return map_uv_relief_frame(carried.du,carried.dv,authored[2],authored[2]);
}
// The frame a relief texel (a normal or clearcoat normal map's) is taken on
// about the unit normal `n`, where its coordinates' surface gradients are
// `gradient_u` and `gradient_v`, both perpendicular to `n` (any common
// scale; a negative one, a back's, reverses T and B): glTF's tangent
// space, orthonormal, T along increasing U and B = (N x T) w toward
// decreasing V, so the texture's +Y points up its image (glTF 2.0 3.9.3,
// 3.7.2.1). T is the surface's ∂p/∂U, which lies across the gradient of V,
// and B the unit gradient of V reversed: per triangle, the frame MikkTSpace
// builds (∂p/∂u normalised, its bitangent sign times N x T) and the Khronos
// glTF Sample Renderer 0686eb2's derivative fallback takes
// (material_info.glsl 164-171), which keeps no handedness. The one owner
// of that frame for raster's derivative frame, a ray hit's triangle frame
// and a carried authored frame (specs/sgl3d-architecture.md, Material map
// coordinates). three.js r187 instead keeps the gradients as they are,
// scaled by the longer one's inverse length (TangentUtils), so a stretched
// or sheared unwrap shades flatter along its less dense axis and leans with
// its shear, and takes B along the gradient of V, negating normalScale.y in
// its glTF loader (GLTFLoader.js 3642-3645). No floor: raster's gradients
// scale with a pixel's area. A degenerate frame (a gradient zero, or the two
// parallel) takes none of the texel's slope.
fn map_uv_texel_frame(gradient_u:vec3<f32>,gradient_v:vec3<f32>,n:vec3<f32>)->mat3x3<f32> {
 let handedness=sign(dot(cross(gradient_u,gradient_v),n));
 let squared=dot(gradient_v,gradient_v);
 if handedness==0. || squared<=0. {
  return mat3x3(vec3(0.),vec3(0.),n);
 }
 let scale=inverseSqrt(squared);
 return mat3x3(cross(gradient_v,n)*(handedness*scale),-gradient_v*scale,n);
}
// The frame a relief texel is taken on about the normal `n`, where the
// surface's position moves `du` and `dv` along the texel's coordinates:
// the surface gradients of their dual (cotangent) frame, three.js r185's
// TangentUtils', made glTF's frame by map_uv_texel_frame. The dual keeps
// the signed UV Jacobian, measured against `facing`, the normal of the side
// the derivatives were taken on: glTF's tangent.w, the handedness of
// mirrored coordinates.
fn map_uv_relief_frame(du:vec3<f32>,dv:vec3<f32>,n:vec3<f32>,facing:vec3<f32>)->mat3x3<f32> {
 let orientation=sign(dot(cross(du,dv),facing));
 return map_uv_texel_frame(cross(dv,n)*orientation,cross(n,du)*orientation,n);
}
