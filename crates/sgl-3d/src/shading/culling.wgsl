// The GPU draw lists' layouts and caps (Rust mirror: shading/culling.rs)
// and the clip volume and occlusion tests the cull stage (stages/cull.wgsl)
// makes of a candidate's and a section's bounds. Reads `cull_view`,
// `cull_occlusion` and `cull_pyramid`.
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
// DrawSet.flags: its material casts the directional shadow; it is opaque,
// so a cascade draws its paired sections indexed.
const SET_CASTS_DIRECTIONAL_SHADOW:u32=1u;
const SET_PAIRS:u32=2u;
// CullView.flags: the camera's population (else a cascade's), the clip
// volume test, its near plane, the level of detail, per-candidate
// statistics, and paired sections appended to their sets' paired regions.
const CULL_CAMERA:u32=1u;
const CULL_FRUSTUM:u32=2u;
const CULL_NEAR:u32=4u;
const CULL_LOD:u32=8u;
const CULL_CANDIDATE_STATISTICS:u32=16u;
const CULL_PAIRED:u32=32u;
// CullOcclusion.flags: the early phase tests against the last submitted
// frame's depth pyramid.
const OCCLUSION_EARLY:u32=1u;
// A view's dispatch buffer: the indirect dispatches the finalizes write,
// three words each, at these words: the early section cull's, the late
// instance cull's and the late section cull's.
const DISPATCH_EARLY_SECTIONS:u32=0u;
const DISPATCH_LATE_INSTANCES:u32=3u;
const DISPATCH_LATE_SECTIONS:u32=6u;
const CULL_DISPATCH_WORDS:u32=9u;
// A view's draws (array<atomic<u32>>): its statistics' words, the sections
// appended and their triangles by mobility, then each set's indirect draws
// (early, late, paired), whose instance count the section cull adds to,
// then, with CULL_CANDIDATE_STATISTICS, each candidate's appended sections
// and triangles from CullView.candidate_statistics.
const CULL_STATISTICS_WORDS:u32=4u;
const CULL_STATIC_SECTIONS:u32=0u;
const CULL_STATIC_TRIANGLES:u32=1u;
const CULL_MOVING_SECTIONS:u32=2u;
const CULL_MOVING_TRIANGLES:u32=3u;
const DRAW_COMMAND_WORDS:u32=5u;
const DRAW_COMMAND_INSTANCE_COUNT:u32=1u;
const CANDIDATE_STATISTICS_WORDS:u32=2u;
// One instance's mesh, which a GPU-built view may draw: its bounds in its
// model's space, its object record's index, its mesh's record word in the
// scene source, its set, its level chain and its mesh's first vertex in its
// set's positions slab, or NO_POSITIONS for a mesh without slab positions.
struct DrawCandidate {
 bounds_min:vec3<f32>,
 object:u32,
 bounds_max:vec3<f32>,
 mesh:u32,
 draw_set:u32,
 chain:u32,
 positions:u32,
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
// x, the frame's visibility mask, CULL_* bits, where the per-candidate
// statistics start in its draws, the index of its first late command, its
// late section queue's capacity, and the index of its first paired command
// and where its paired regions start in its cluster list.
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
 late_command:u32,
 queue_capacity:u32,
 paired_command:u32,
 paired_region:u32,
}
// The camera's occlusion test for a frame, the cull stage's own: the last
// submitted frame's view-projection with that frame's jitter, which the
// early phase projects through, this frame's with its jitter, which the
// late phase projects through, the pyramid's levels the test may read and
// OCCLUSION_* bits.
struct CullOcclusion {
 previous:mat4x4<f32>,
 current:mat4x4<f32>,
 levels:u32,
 flags:u32,
}
// A view's lists: each list's appends, the count its consumer runs over
// (clamped to the list's capacity by a finalize) and that dispatch's
// workgroups along x, then the entries. Of a view's candidate count N: the
// early visible list at [0, N), a candidate and its chosen level's mesh
// record word each; the late list at [N, 2N), the candidates the early
// phase found occluded; the late visible list at [2N, 3N), those the late
// instance cull passed; the late section queue at [3N, 3N + queue
// capacity), each section the early phase found occluded as its early
// visible entry's index and the section's. Only a view that culls a late
// phase holds more than the early visible list.
struct CullLists {
 visible_count:atomic<u32>,
 visible_dispatched:u32,
 visible_side:u32,
 late_count:atomic<u32>,
 late_dispatched:u32,
 late_side:u32,
 late_visible_count:atomic<u32>,
 late_visible_dispatched:u32,
 queue_count:atomic<u32>,
 queue_dispatched:u32,
 late_sections_side:u32,
 entries:array<vec2<u32>>,
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
// allow twice what raster's own rounding needs (view::culling::Frustum::planes).
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
// Whether the bounds `lo`..`hi` of a model at pose `model`, seen through
// `clip_from_world`, lie wholly behind the depth pyramid: Bevy 9d12036's
// meshlet occlusion test (crates/bevy_pbr/src/meshlet/cull_shared.wesl,
// MIT OR Apache-2.0, see src/LICENSE-bevy.txt): the box's eight corners
// projected as zeux's approximate projected bounds (project_aabb 83-121),
// its screen rectangle's texels at the finest level whose 4x4 block holds
// it, and the farthest depth there against the box's nearest
// (occlusion_cull_screen_aabb 144-168, sample_hzb and sample_hzb_row
// 123-142). Changed: a box that reaches the near plane is kept by the
// clip-space test of a reversed-Z projection (z > w, or w <= 0), which is
// Bevy's `min w < near` for its projections and holds for any the game
// gives; a box is occluded
// only when its nearest depth is strictly behind the farthest, as Bevy's
// mesh preprocessing compares (mesh_preprocess.wesl 324), so a surface
// facing the camera square on is not hidden by its own depth; a rectangle
// that needs a level past the pyramid's built ones, and nonfinite bounds,
// are kept.
fn cull_occluded(clip_from_world:mat4x4<f32>,model:mat4x4<f32>,lo:vec3<f32>,hi:vec3<f32>)->bool {
 if !cull_finite(lo) || !cull_finite(hi) {
  return false;
 }
 let clip_from_local=clip_from_world*model;
 let extent=hi-lo;
 let sx=clip_from_local*vec4(extent.x,0.,0.,0.);
 let sy=clip_from_local*vec4(0.,extent.y,0.,0.);
 let sz=clip_from_local*vec4(0.,0.,extent.z,0.);
 let p0=clip_from_local*vec4(lo,1.);
 let p1=p0+sz;
 let p2=p0+sy;
 let p3=p2+sz;
 let p4=p0+sx;
 let p5=p4+sz;
 let p6=p4+sy;
 let p7=p6+sz;
 let corners=array(p0,p1,p2,p3,p4,p5,p6,p7);
 var lo_ndc=vec3(3.4e38);
 var hi_ndc=vec3(-3.4e38);
 for(var corner=0u;corner<8u;corner++) {
  let p=corners[corner];
  if p.w<=0. || p.z>p.w {
   return false;
  }
  let ndc=p.xyz/p.w;
  lo_ndc=min(lo_ndc,ndc);
  hi_ndc=max(hi_ndc,ndc);
 }
 // NDC to the pyramid's texture coordinates, y down.
 let uv_lo=vec2(lo_ndc.x,-hi_ndc.y)*0.5+0.5;
 let uv_hi=vec2(hi_ndc.x,-lo_ndc.y)*0.5+0.5;
 let size=vec2<f32>(textureDimensions(cull_pyramid));
 // Clamped as floats, so no conversion leaves u32's range.
 let min_texel=vec2<u32>(clamp(uv_lo*size,vec2(0.),size-1.));
 let max_texel=vec2<u32>(clamp(uv_hi*size,vec2(0.),size-1.));
 let texels=max_texel-min_texel;
 // firstLeadingBit(0) is ~0u, which the + 1 wraps to 0.
 var level=max(firstLeadingBit(max(texels.x,texels.y))+1u,2u)-2u;
 if any((max_texel>>vec2(level))>(min_texel>>vec2(level))+3u) {
  level+=1u;
 }
 if level>=cull_occlusion.levels {
  return false;
 }
 let first=min_texel>>vec2(level);
 let last=max_texel>>vec2(level);
 var farthest=3.4e38;
 for(var row=0u;row<4u;row++) {
  for(var column=0u;column<4u;column++) {
   let texel=min(first+vec2(column,row),last);
   farthest=min(farthest,textureLoad(cull_pyramid,texel,level).x);
  }
 }
 return hi_ndc.z<farthest;
}
