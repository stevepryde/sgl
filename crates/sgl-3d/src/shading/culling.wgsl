// The GPU draw lists' layouts and caps (Rust mirror: shading/culling.rs)
// and the clip volume test the cull stage (stages/cull.wgsl) makes of a
// candidate's and a section's bounds. Reads `cull_view`.
// The most alternatives a mesh registers: a level chain's walk is bounded by
// it (AR-12).
const MAX_MESH_LODS:u32=8u;
// The most sections a mesh holds: a section cull's stride over a mesh is
// bounded by MAX_MESH_SECTIONS / CULL_WORKGROUP (AR-12).
const MAX_MESH_SECTIONS:u32=65536u;
// A GPU-built draw instance's vertices: a section's most triangles, three
// each; past its triangles, a dummy.
const SECTION_VERTICES:u32=384u;
// Invocations in a cull workgroup.
const CULL_WORKGROUP:u32=64u;
// The most workgroups a dispatch takes along x; past it, two dimensions.
const CULL_MAX_WORKGROUPS:u32=65535u;
// DrawCandidate.draw_set of a free slot, and DrawCandidate.chain of a mesh
// without alternatives.
const NO_SET:u32=4294967295u;
const NO_CHAIN:u32=4294967295u;
// DrawSet.flags: its material casts the directional shadow.
const SET_CASTS_DIRECTIONAL_SHADOW:u32=1u;
// CullView.flags: the camera's population (else a cascade's), the clip
// volume test, its near plane, the level of detail, and per-candidate
// statistics.
const CULL_CAMERA:u32=1u;
const CULL_FRUSTUM:u32=2u;
const CULL_NEAR:u32=4u;
const CULL_LOD:u32=8u;
const CULL_CANDIDATE_STATISTICS:u32=16u;
// A view's draws (array<atomic<u32>>): its statistics' words, the sections
// appended and their triangles by mobility, then each set's indirect draw,
// whose instance count the section cull adds to, then, with
// CULL_CANDIDATE_STATISTICS, each candidate's appended sections and
// triangles from CullView.candidate_statistics.
const CULL_STATISTICS_WORDS:u32=4u;
const CULL_STATIC_SECTIONS:u32=0u;
const CULL_STATIC_TRIANGLES:u32=1u;
const CULL_MOVING_SECTIONS:u32=2u;
const CULL_MOVING_TRIANGLES:u32=3u;
const DRAW_COMMAND_WORDS:u32=4u;
const DRAW_COMMAND_INSTANCE_COUNT:u32=1u;
const CANDIDATE_STATISTICS_WORDS:u32=2u;
// One instance's mesh, which a GPU-built view may draw: its bounds in its
// model's space, its object record's index, its mesh's record word in the
// scene source, its set and its level chain.
struct DrawCandidate {
 bounds_min:vec3<f32>,
 object:u32,
 bounds_max:vec3<f32>,
 mesh:u32,
 draw_set:u32,
 chain:u32,
}
// One alternative of a mesh: its bounds in the base mesh's space, its error
// bound in metres and its mesh's record word.
struct ChainLevel {
 bounds_min:vec3<f32>,
 error:f32,
 bounds_max:vec3<f32>,
 mesh:u32,
}
// A mesh's alternatives, detailed to coarse.
struct LodChain {
 levels:array<ChainLevel,MAX_MESH_LODS>,
 count:u32,
}
// What draws with one pipeline and one material: its region of each
// GPU-built view's cluster list, its material's visibility group and
// SET_* bits.
struct DrawSet {
 region:u32,
 capacity:u32,
 visibility_group:u32,
 flags:u32,
}
// One GPU-built view's cull: its clip volume's planes in world space, each
// plane's tolerance row, the level of detail's transforms and render size,
// the early instance cull's candidates and its dispatch's workgroups along
// x, the frame's visibility mask, CULL_* bits and where the per-candidate
// statistics start in its draws.
struct CullView {
 planes:array<vec4<f32>,6>,
 plane_errors:array<vec4<f32>,6>,
 lod_clip_from_world:mat4x4<f32>,
 lod_magnitude:mat4x4<f32>,
 lod_size:vec2<f32>,
 candidates:u32,
 candidate_side:u32,
 visibility_mask:u32,
 flags:u32,
 candidate_statistics:u32,
}
// A view's lists: the visible list's appends, the count the finalize clamped
// to its capacity and its dispatch's workgroups along x, then its entries,
// a candidate and its chosen level's mesh record word each.
struct CullLists {
 visible_count:atomic<u32>,
 visible_dispatched:u32,
 visible_side:u32,
 visible:array<vec2<u32>>,
}
// Whether the 32-bit pattern of each component is finite, which fast math
// cannot fold away as it may `x != x`.
fn cull_finite(v:vec3<f32>)->bool {
 let exponent=bitcast<vec3<u32>>(v)&vec3(0x7f800000u);
 return all(exponent!=vec3(0x7f800000u));
}
// The world-space plane `plane` in the space of a model at pose `model`:
// dot(plane, model * p) is dot(transpose(model) * plane, p).
fn cull_plane_in_model(plane:vec4<f32>,model:mat4x4<f32>)->vec4<f32> {
 return vec4(dot(plane,model[0]),dot(plane,model[1]),dot(plane,model[2]),dot(plane,model[3]));
}
// Whether any part of the bounds `lo`..`hi` of a model at pose `model` may
// lie inside the view's clip volume: false only when the whole box lies
// outside one plane by more than its tolerance. The planes and tolerance
// rows are view::culling's, built pose-free in double precision; the pose is
// applied here in f32, its rounding covered by the tolerance rows, which
// allow twice what raster's own rounding needs (view::culling::ViewPlanes).
// Nonfinite bounds never reject.
fn cull_reaches(model:mat4x4<f32>,lo:vec3<f32>,hi:vec3<f32>)->bool {
 if !cull_finite(lo) || !cull_finite(hi) {
  return true;
 }
 let center=vec4((lo+hi)*0.5,1.);
 let extent=vec4((hi-lo)*0.5,0.);
 let largest=vec4(max(abs(lo),abs(hi)),1.);
 let absolute=matrix_absolute(model);
 let planes=select(5u,6u,(cull_view.flags&CULL_NEAR)!=0u);
 for(var plane=0u;plane<6u;plane++) {
  if plane>=planes {
   break;
  }
  let local=cull_plane_in_model(cull_view.planes[plane],model);
  let tolerance=dot(cull_plane_in_model(cull_view.plane_errors[plane],absolute),largest);
  if dot(local,center)+dot(abs(local),extent)< -tolerance {
   return false;
  }
 }
 return true;
}
