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
// derivatives (T, -B) are carried (map_uv_frame), their dual taken
// (map_uv_relief_frame) and its bitangent turned back.
fn map_uv_authored_frame(t:MapUv,authored:mat3x3<f32>)->mat3x3<f32> {
 let carried=map_uv_frame(t,authored[0],-authored[1]);
 let frame=map_uv_relief_frame(carried.du,carried.dv,authored[2],authored[2]);
 return mat3x3(frame[0],-frame[1],frame[2]);
}
// The frame a relief texel (a normal, bump or clearcoat normal map's) is
// taken on about the normal `n`, where the surface's position moves `du`
// and `dv` along the texel's coordinates: their dual (cotangent) frame, the
// surface gradient construction of three.js r185's TangentUtils, its axes
// scaled alike. The dual keeps the signed UV Jacobian, measured against
// `facing`, the normal of the side the derivatives were taken on: glTF's
// tangent.w, the handedness of mirrored coordinates.
fn map_uv_relief_frame(du:vec3<f32>,dv:vec3<f32>,n:vec3<f32>,facing:vec3<f32>)->mat3x3<f32> {
 let orientation=sign(dot(cross(du,dv),facing));
 let tangent=cross(dv,n)*orientation;
 let bitangent=cross(n,du)*orientation;
 let scale=inverseSqrt(max(max(dot(tangent,tangent),dot(bitangent,bitangent)),0.0000001));
 return mat3x3(tangent*scale,bitangent*scale,n);
}
