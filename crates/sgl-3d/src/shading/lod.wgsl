// The projected error bound of a mesh's alternative, which the GPU draw
// lists' instance cull chooses a level of detail by: the twin of
// shading/lod.rs's `projected_error`, which the CPU builder chooses by. The
// twin computes the same bound in f32 under margins that cover its own
// rounding, so it is never smaller and never admits an alternative the CPU
// bound would not.
const LOD_PIXELS:f32=0.5;
// The relative rounding the bound allows each of raster's separate model,
// view and projection products (shading/lod.rs, 32 f32 epsilons).
const LOD_ROUNDING:f32=3.8146973e-6;
// 1 + 2^-16: the twin's margin on the row lengths it computes and on its
// result, far above their few f32 roundings.
const LOD_MARGIN:f32=1.0000153;
// A bound past every pixel count: the bounds may reach the near plane. Well
// within f32, which a shader translator may print as decimal digits that
// would round past f32::MAX.
const LOD_UNBOUNDED:f32=1e30;
// The absolute value of each element of `m`.
fn matrix_absolute(m:mat4x4<f32>)->mat4x4<f32> {
 return mat4x4(abs(m[0]),abs(m[1]),abs(m[2]),abs(m[3]));
}
// The screen pixels, at `size`, that a displacement of at most `error`
// metres of the bounds `lo`..`hi` of a model at pose `model` can move,
// through `clip_from_world` (the camera's unjittered projection times its
// view) whose elements' absolute products are `magnitude_from_world`, or
// LOD_UNBOUNDED where the bounds may reach the near plane. It allows twice
// LOD_ROUNDING, since it composes the pose with the view in f32 where the
// CPU composes in f64.
fn lod_projected_error(clip_from_world:mat4x4<f32>,magnitude_from_world:mat4x4<f32>,model:mat4x4<f32>,lo:vec3<f32>,hi:vec3<f32>,error:f32,size:vec2<f32>)->f32 {
 let matrix=clip_from_world*model;
 let magnitude=magnitude_from_world*matrix_absolute(model);
 let largest=vec4(max(abs(lo),abs(hi)),1.);
 let rounding=magnitude*largest*(2.*LOD_ROUNDING);
 let rows=transpose(matrix);
 let lengths=vec4(length(rows[0].xyz),length(rows[1].xyz),length(rows[2].xyz),length(rows[3].xyz));
 // The original and alternative can round in opposite directions.
 let delta=lengths*error*LOD_MARGIN+2.*rounding;
 var minimum_w=LOD_UNBOUNDED;
 var maximum_ndc=vec2(0.);
 for(var corner=0u;corner<8u;corner++) {
  let p=select(lo,hi,vec3((corner&1u)!=0u,(corner&2u)!=0u,(corner&4u)!=0u));
  let clip=matrix*vec4(p,1.);
  // Reversed-Z: the near plane is z = w.
  let near=clip.w-clip.z-rounding.w-rounding.z;
  if clip.w-rounding.w<=delta.w || near<=delta.w+delta.z {
   return LOD_UNBOUNDED;
  }
  minimum_w=min(minimum_w,clip.w-rounding.w);
  maximum_ndc=max(maximum_ndc,(abs(clip.xy)+rounding.xy)/(clip.w-rounding.w));
 }
 let pixels=(delta.xy+maximum_ndc*delta.w)/(minimum_w-delta.w)*size*0.5;
 return length(pixels)*LOD_MARGIN;
}
