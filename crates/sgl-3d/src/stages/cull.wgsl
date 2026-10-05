// The cull stage's dispatches (stages/cull.rs): per GPU-built view, the
// instance cull, its finalize and the section cull, which build the view's
// draws from the scene's draw candidates, in the form of Bevy 9d12036's
// meshlet culling (crates/bevy_pbr/src/meshlet/, MIT OR Apache-2.0, see
// src/LICENSE-bevy.txt): `cull_instances.wesl`, one invocation a candidate,
// population, level of detail and frustum; `cull_clusters.wesl`, each
// section that passes appended to its set's draw by an atomic add on the
// draw's instance count (79-91); `remap_1d_to_2d_dispatch.wesl`, the
// dispatch remapped to two dimensions past the device's limit. Changed:
// Bevy's cull walks a cluster BVH of its own asset; here a workgroup per
// visible candidate strides its chosen level's sections, the leaves of the
// mesh's range hierarchy, from the mesh's section table in the scene
// source; a candidate's level of detail is chosen by SGL3D's projected error
// bound (lod.wgsl), where Bevy's meshlets choose by a sphere's error; the
// frustum test is conservative in f32 (culling.wgsl's `cull_reaches`); an
// append past its set's region subtracts its add back, so the count ends
// at the region's capacity; and the finalize remaps in integers, and the
// section cull linearises its workgroup from what the finalize wrote, not
// from `num_workgroups`, which DX12 reports as zero for an indirect
// dispatch without wgpu's validation.
@group(0) @binding(0) var<uniform> cull_view:CullView;
@group(0) @binding(1) var<storage,read> cull_candidates:array<DrawCandidate>;
@group(0) @binding(2) var<storage,read> cull_chains:array<LodChain>;
@group(0) @binding(3) var<storage,read> cull_objects:array<Object>;
@group(0) @binding(4) var<storage,read> cull_sets:array<DrawSet>;
@group(0) @binding(5) var<storage,read_write> cull_lists:CullLists;
@group(0) @binding(6) var<storage,read> cull_source:array<u32>;
@group(0) @binding(7) var<storage,read_write> cull_regions:array<DrawInstance>;
@group(0) @binding(8) var<storage,read_write> cull_draws:array<atomic<u32>>;
@group(0) @binding(9) var<storage,read_write> cull_dispatch:array<u32,3>;

// Whether the view's population holds an object with `flags` drawn in
// `set`: the camera's, a `visible` instance whose material's group the
// mask enables; a cascade's, a `capture_visible` instance whose material
// casts in an enabled group.
fn cull_population(flags:u32,draw_set:DrawSet)->bool {
 let group=draw_set.visibility_group;
 let enabled=(group&cull_view.visibility_mask)==group;
 if (cull_view.flags&CULL_CAMERA)!=0u {
  return (flags&OBJECT_VISIBLE)!=0u && enabled;
 }
 let casts=(draw_set.flags&SET_CASTS_DIRECTIONAL_SHADOW)!=0u;
 return (flags&OBJECT_CAPTURE_VISIBLE)!=0u && enabled && casts;
}

// The early instance cull: one invocation a candidate slot, dispatched
// directly over the view's candidates (CullView.candidates, its workgroups
// along x CullView.candidate_side). A candidate in the view's population
// takes its level, the last admissible alternative of its chain where the
// view chooses one, and is tested against the view's clip volume by that
// level's bounds; one that passes joins the visible list with its level's
// mesh. The list holds a slot for every candidate.
@compute @workgroup_size(CULL_WORKGROUP)
fn cull_instances(@builtin(workgroup_id) workgroup:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
 let index=(workgroup.y*cull_view.candidate_side+workgroup.x)*CULL_WORKGROUP+lane;
 if index>=cull_view.candidates {
  return;
 }
 let candidate=cull_candidates[index];
 if candidate.draw_set==NO_SET {
  return;
 }
 let flags=cull_objects[candidate.object].flags;
 if !cull_population(flags,cull_sets[candidate.draw_set]) {
  return;
 }
 let model=cull_objects[candidate.object].model;
 var mesh=candidate.mesh;
 var lo=candidate.bounds_min;
 var hi=candidate.bounds_max;
 if (cull_view.flags&CULL_LOD)!=0u && candidate.chain!=NO_CHAIN {
  let levels=min(cull_chains[candidate.chain].count,MAX_MESH_LODS);
  // Coarsest first: the first admissible is the last in the chain.
  for(var step=0u;step<MAX_MESH_LODS;step++) {
   if step>=levels {
    break;
   }
   let level=cull_chains[candidate.chain].levels[levels-1u-step];
   let union_lo=min(candidate.bounds_min,level.bounds_min);
   let union_hi=max(candidate.bounds_max,level.bounds_max);
   let pixels=lod_projected_error(cull_view.lod_clip_from_world,cull_view.lod_magnitude,model,union_lo,union_hi,level.error,cull_view.lod_size);
   if pixels<=LOD_PIXELS {
    mesh=level.mesh;
    lo=level.bounds_min;
    hi=level.bounds_max;
    break;
   }
  }
 }
 if (cull_view.flags&CULL_FRUSTUM)!=0u && !cull_reaches(model,lo,hi) {
  return;
 }
 let slot=atomicAdd(&cull_lists.visible_count,1u);
 if slot<cull_view.candidates {
  cull_lists.visible[slot]=vec2(index,mesh);
 }
}

// The finalize: one invocation, dispatched directly after the instance
// cull. It clamps the visible list's count to its capacity and writes the
// section cull's indirect dispatch, a workgroup a visible candidate,
// remapped to two dimensions past CULL_MAX_WORKGROUPS, with the count and
// its workgroups along x beside the count, from which the section cull
// linearises its index.
@compute @workgroup_size(1)
fn cull_finalize() {
 let count=min(atomicLoad(&cull_lists.visible_count),cull_view.candidates);
 let side=min(count,CULL_MAX_WORKGROUPS);
 cull_lists.visible_dispatched=count;
 cull_lists.visible_side=side;
 cull_dispatch[0]=side;
 cull_dispatch[1]=count/CULL_MAX_WORKGROUPS+select(0u,1u,count%CULL_MAX_WORKGROUPS!=0u);
 cull_dispatch[2]=1u;
}

// A word of the scene source as f32, and three as a vector.
fn cull_source_v3(at:u32)->vec3<f32> {
 return vec3(bitcast<f32>(cull_source[at]),bitcast<f32>(cull_source[at+1u]),bitcast<f32>(cull_source[at+2u]));
}

// The section cull: a workgroup a visible candidate, its invocations
// striding its level's sections, at most MAX_MESH_SECTIONS. A section that
// passes the view's clip volume (every one of a deforming candidate, whose
// sections' bounds are its rest pose's) takes the next slot of its set's
// region by an atomic add on the set's command's instance count, and is
// written there as a draw instance while the slot is within the region; an
// append past it subtracts its add back, so the count ends at the region's
// capacity. The view's statistics count the sections appended and their
// triangles by mobility, and, with CULL_CANDIDATE_STATISTICS, by candidate.
@compute @workgroup_size(CULL_WORKGROUP)
fn cull_sections(@builtin(workgroup_id) workgroup:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
 let index=workgroup.y*cull_lists.visible_side+workgroup.x;
 if index>=cull_lists.visible_dispatched {
  return;
 }
 let entry=cull_lists.visible[index];
 let candidate=cull_candidates[entry.x];
 let mesh=entry.y;
 let flags=cull_objects[candidate.object].flags;
 let model=cull_objects[candidate.object].model;
 let draw_set=cull_sets[candidate.draw_set];
 let table=cull_source[mesh+SCENE_MESH_SECTIONS];
 let count=min(cull_source[mesh+SCENE_MESH_SECTION_COUNT],MAX_MESH_SECTIONS);
 let tested=(cull_view.flags&CULL_FRUSTUM)!=0u && (flags&OBJECT_DEFORMING)==0u;
 let command=CULL_STATISTICS_WORDS+candidate.draw_set*DRAW_COMMAND_WORDS+DRAW_COMMAND_INSTANCE_COUNT;
 let moving=(flags&OBJECT_STATIC)==0u;
 let sections_word=select(CULL_STATIC_SECTIONS,CULL_MOVING_SECTIONS,moving);
 let triangles_word=select(CULL_STATIC_TRIANGLES,CULL_MOVING_TRIANGLES,moving);
 let by_candidate=(cull_view.flags&CULL_CANDIDATE_STATISTICS)!=0u;
 let candidate_word=cull_view.candidate_statistics+entry.x*CANDIDATE_STATISTICS_WORDS;
 for(var stride=0u;stride<MAX_MESH_SECTIONS/CULL_WORKGROUP;stride++) {
  let section=stride*CULL_WORKGROUP+lane;
  if section>=count {
   break;
  }
  let at=table+section*SCENE_SECTION_WORDS;
  if tested && !cull_reaches(model,cull_source_v3(at+SCENE_SECTION_MIN),cull_source_v3(at+SCENE_SECTION_MAX)) {
   continue;
  }
  let slot=atomicAdd(&cull_draws[command],1u);
  if slot>=draw_set.capacity {
   atomicSub(&cull_draws[command],1u);
   continue;
  }
  let triangles=cull_source[at+SCENE_SECTION_TRIANGLES];
  cull_regions[draw_set.region+slot]=DrawInstance(candidate.object,mesh,cull_source[at+SCENE_SECTION_FIRST_INDEX],triangles,0u);
  atomicAdd(&cull_draws[sections_word],1u);
  atomicAdd(&cull_draws[triangles_word],triangles);
  if by_candidate {
   atomicAdd(&cull_draws[candidate_word],1u);
   atomicAdd(&cull_draws[candidate_word+1u],triangles);
  }
 }
}
