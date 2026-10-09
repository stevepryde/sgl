// The cull stage's dispatches (stages/cull.rs): per GPU-built view, the
// instance cull, its finalize and the section cull, which build the view's
// draws from the scene's draw candidates, in the form of Bevy 9d12036's
// meshlet culling (crates/bevy_pbr/src/meshlet/, MIT OR Apache-2.0, see
// src/LICENSE-bevy.txt): `cull_instances.wesl`, one invocation a candidate,
// population, level of detail, frustum and occlusion, the occluded pushed
// to a second pass; `cull_clusters.wesl`, each section that passes
// appended to its set's draw by an atomic add on the draw's instance count
// (79-91), one the first pass finds occluded queued for the second
// (50-58); `remap_1d_to_2d_dispatch.wesl`, the dispatch remapped to two
// dimensions past the device's limit. For the camera while occlusion
// culling runs, two phases, as Bevy's two-phase culling: the early phase
// tests against the last submitted frame's depth pyramid at each object's
// previous pose (`cull_instances`, `cull_finalize`, `cull_sections`), the
// late phase against this frame's at its pose, over what the early phase
// found occluded (`cull_instances_late`, `cull_finalize_late`,
// `cull_sections_late`); the occlusion test is culling.wgsl's
// `cull_occluded`. Changed:
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
@group(0) @binding(9) var<storage,read_write> cull_dispatch:array<u32,CULL_DISPATCH_WORDS>;
@group(0) @binding(10) var cull_pyramid:texture_2d<f32>;
@group(0) @binding(11) var<uniform> cull_occlusion:CullOcclusion;


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

// Bounds `lo` to `hi` in a mesh's space, grown by its set's displacement
// bound (DrawSet.displacement_bound): where its material's shader may move
// its vertices, as Godot b130438 grows an instance's bounds by its
// extra_cull_margin (renderer_scene_cull.cpp 2010–2081). Every test of a
// candidate's or a section's bounds takes them grown.
struct CullBounds {
 lo:vec3<f32>,
 hi:vec3<f32>,
}
fn cull_grown(lo:vec3<f32>,hi:vec3<f32>,draw_set:u32)->CullBounds {
 let bound=cull_sets[draw_set].displacement_bound;
 return CullBounds(lo-vec3(bound),hi+vec3(bound));
}

// A candidate's chosen level: its mesh record word and its bounds in its
// model's space.
struct CullLevel {
 mesh:u32,
 lo:vec3<f32>,
 hi:vec3<f32>,
}

// The level `candidate` draws at pose `model`: the last admissible
// alternative of its chain where the view chooses one, else its own mesh.
fn cull_level(candidate:DrawCandidate,model:mat4x4<f32>)->CullLevel {
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
 return CullLevel(mesh,lo,hi);
}

// The early instance cull: one invocation a candidate slot, dispatched
// directly over the view's candidates (CullView.candidates, its workgroups
// along x CullView.candidate_side). A candidate in the view's population
// takes its level and is tested against the view's clip volume by that
// level's bounds; one that passes and, where the early phase tests
// occlusion, is not hidden behind the last submitted frame's depth at its
// previous pose joins the visible list with its level's mesh, and one that
// is joins the late list. Each list holds a slot for every candidate.
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
 let level=cull_level(candidate,model);
 let bounds=cull_grown(level.lo,level.hi,candidate.draw_set);
 if (cull_view.flags&CULL_FRUSTUM)!=0u && !cull_reaches(model,bounds.lo,bounds.hi) {
  return;
 }
 if cull_occludes_early() && cull_occluded(cull_occlusion.previous,cull_objects[candidate.object].previous_model,bounds.lo,bounds.hi) {
  let slot=atomicAdd(&cull_lists.late_count,1u);
  if slot<cull_view.candidates {
   cull_lists.entries[cull_view.candidates+slot]=vec2(index,level.mesh);
  }
  return;
 }
 let slot=atomicAdd(&cull_lists.visible_count,1u);
 if slot<cull_view.candidates {
  cull_lists.entries[slot]=vec2(index,level.mesh);
 }
}

// Whether the early phase tests occlusion: the view culls with its clip
// volume, and the last submitted frame built the pyramid it reads.
fn cull_occludes_early()->bool {
 return (cull_view.flags&CULL_FRUSTUM)!=0u && (cull_occlusion.flags&OCCLUSION_EARLY)!=0u;
}

// Writes the indirect dispatch at word `at` of `workgroups` workgroups,
// remapped to two dimensions past CULL_MAX_WORKGROUPS, and returns its
// workgroups along x, from which its invocations linearise their index.
fn cull_dispatch_of(at:u32,workgroups:u32)->u32 {
 let side=min(workgroups,CULL_MAX_WORKGROUPS);
 cull_dispatch[at]=side;
 cull_dispatch[at+1u]=workgroups/CULL_MAX_WORKGROUPS+select(0u,1u,workgroups%CULL_MAX_WORKGROUPS!=0u);
 cull_dispatch[at+2u]=1u;
 return side;
}

// The early finalize: one invocation, dispatched directly after the early
// instance cull. It clamps the visible and late lists' counts to their
// capacities and writes the early section cull's indirect dispatch, a
// workgroup a visible candidate, and the late instance cull's, an
// invocation a late candidate, the late list being final once the early
// instance cull has run.
@compute @workgroup_size(1)
fn cull_finalize() {
 let count=min(atomicLoad(&cull_lists.visible_count),cull_view.candidates);
 cull_lists.visible_dispatched=count;
 cull_lists.visible_side=cull_dispatch_of(DISPATCH_EARLY_SECTIONS,count);
 let late=min(atomicLoad(&cull_lists.late_count),cull_view.candidates);
 cull_lists.late_dispatched=late;
 cull_lists.late_side=cull_dispatch_of(DISPATCH_LATE_INSTANCES,(late+CULL_WORKGROUP-1u)/CULL_WORKGROUP);
}

// A word of the scene source as f32, and three as a vector.
fn cull_source_v3(at:u32)->vec3<f32> {
 return vec3(bitcast<f32>(cull_source[at]),bitcast<f32>(cull_source[at+1u]),bitcast<f32>(cull_source[at+2u]));
}

// Appends section `section` (its words at `at` in the scene source) of
// candidate slot `index`'s level `mesh` to its set's region of the bound
// cluster list, the late one's where `late`: the next slot by an atomic add
// on the set's command's instance count, written as a draw instance while
// the slot is within the region; an append past it subtracts its add back,
// so the count ends at the region's capacity. Where the view pairs
// (CULL_PAIRED, a cascade), a section whose triangles pair of an opaque set
// (SET_PAIRS) goes to its set's paired region and command instead, which
// the view draws indexed. Its
// first vertex is the candidate's in its positions slab where it draws the
// candidate's own mesh (a cascade's always, at level 0), where a cascade's
// caster pulls its positions; the camera's pulled passes read none. The
// view's statistics count the sections appended and their triangles by
// mobility, and, with CULL_CANDIDATE_STATISTICS, by candidate.
fn cull_append(index:u32,candidate:DrawCandidate,mesh:u32,at:u32,flags:u32,late:bool) {
 let draw_set=cull_sets[candidate.draw_set];
 let word=cull_source[at+SCENE_SECTION_TRIANGLES];
 let triangles=word&~SCENE_SECTION_PAIRED;
 let paired=(cull_view.flags&CULL_PAIRED)!=0u && (draw_set.flags&SET_PAIRS)!=0u && (word&SCENE_SECTION_PAIRED)!=0u;
 var first_command=select(0u,cull_view.late_command,late);
 var region=draw_set.region;
 if paired {
  first_command=cull_view.paired_command;
  region+=cull_view.paired_region;
 }
 let command=CULL_STATISTICS_WORDS+(first_command+candidate.draw_set)*DRAW_COMMAND_WORDS+DRAW_COMMAND_INSTANCE_COUNT;
 let slot=atomicAdd(&cull_draws[command],1u);
 if slot>=draw_set.capacity {
  atomicSub(&cull_draws[command],1u);
  return;
 }
 let first_vertex=select(NO_POSITIONS,candidate.positions,mesh==candidate.mesh);
 cull_regions[region+slot]=DrawInstance(candidate.object,mesh,cull_source[at+SCENE_SECTION_FIRST_INDEX],triangles,first_vertex);
 let moving=(flags&OBJECT_STATIC)==0u;
 atomicAdd(&cull_draws[select(CULL_STATIC_SECTIONS,CULL_MOVING_SECTIONS,moving)],1u);
 atomicAdd(&cull_draws[select(CULL_STATIC_TRIANGLES,CULL_MOVING_TRIANGLES,moving)],triangles);
 if (cull_view.flags&CULL_CANDIDATE_STATISTICS)!=0u {
  let candidate_word=cull_view.candidate_statistics+index*CANDIDATE_STATISTICS_WORDS;
  atomicAdd(&cull_draws[candidate_word],1u);
  atomicAdd(&cull_draws[candidate_word+1u],triangles);
 }
}

// Section `section`'s words in the scene source, of the mesh record `mesh`.
fn cull_section_words(mesh:u32,section:u32)->u32 {
 return cull_source[mesh+SCENE_MESH_SECTIONS]+section*SCENE_SECTION_WORDS;
}

// A mesh record's sections, at most MAX_MESH_SECTIONS.
fn cull_section_count(mesh:u32)->u32 {
 return min(cull_source[mesh+SCENE_MESH_SECTION_COUNT],MAX_MESH_SECTIONS);
}

// The early section cull: a workgroup a visible candidate, its invocations
// striding its level's sections, at most MAX_MESH_SECTIONS. A section that
// passes the view's clip volume (every one of a deforming candidate, whose
// sections' bounds are its rest pose's) is appended to its set's region,
// unless the early phase tests occlusion and finds it hidden behind the
// last submitted frame's depth at the object's previous pose: that one
// joins the late section queue, as its visible entry and section, while
// the queue has a slot, and is appended otherwise.
@compute @workgroup_size(CULL_WORKGROUP)
fn cull_sections(@builtin(workgroup_id) workgroup:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
 let index=workgroup.y*cull_lists.visible_side+workgroup.x;
 if index>=cull_lists.visible_dispatched {
  return;
 }
 let entry=cull_lists.entries[index];
 let candidate=cull_candidates[entry.x];
 let mesh=entry.y;
 let flags=cull_objects[candidate.object].flags;
 let model=cull_objects[candidate.object].model;
 let previous_model=cull_objects[candidate.object].previous_model;
 let count=cull_section_count(mesh);
 let tested=(cull_view.flags&CULL_FRUSTUM)!=0u && (flags&OBJECT_DEFORMING)==0u;
 let occludes=tested && cull_occludes_early();
 let queue=3u*cull_view.candidates;
 for(var stride=0u;stride<MAX_MESH_SECTIONS/CULL_WORKGROUP;stride++) {
  let section=stride*CULL_WORKGROUP+lane;
  if section>=count {
   break;
  }
  let at=cull_section_words(mesh,section);
  let bounds=cull_section_bounds(at,candidate.draw_set);
  if tested && !cull_reaches(model,bounds.lo,bounds.hi) {
   continue;
  }
  if occludes && cull_occluded(cull_occlusion.previous,previous_model,bounds.lo,bounds.hi) {
   let slot=atomicAdd(&cull_lists.queue_count,1u);
   if slot<cull_view.queue_capacity {
    cull_lists.entries[queue+slot]=vec2(index,section);
    continue;
   }
  }
  cull_append(entry.x,candidate,mesh,at,flags,false);
 }
}

// The late instance cull: an invocation a late list entry, dispatched
// indirectly by the early finalize. A candidate the early phase found
// occluded takes its level again (the same as the early phase's: the view
// and pose are this frame's) and is tested against this frame's depth
// pyramid at its pose; one that is not hidden joins the late visible list.
@compute @workgroup_size(CULL_WORKGROUP)
fn cull_instances_late(@builtin(workgroup_id) workgroup:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
 let index=(workgroup.y*cull_lists.late_side+workgroup.x)*CULL_WORKGROUP+lane;
 if index>=cull_lists.late_dispatched {
  return;
 }
 let slot_index=cull_lists.entries[cull_view.candidates+index].x;
 let candidate=cull_candidates[slot_index];
 let model=cull_objects[candidate.object].model;
 let level=cull_level(candidate,model);
 let bounds=cull_grown(level.lo,level.hi,candidate.draw_set);
 if cull_occluded(cull_occlusion.current,model,bounds.lo,bounds.hi) {
  return;
 }
 let slot=atomicAdd(&cull_lists.late_visible_count,1u);
 if slot<cull_view.candidates {
  cull_lists.entries[2u*cull_view.candidates+slot]=vec2(slot_index,level.mesh);
 }
}

// The late finalize: one invocation, dispatched directly after the late
// instance cull. It clamps the late visible list's and the queue's counts
// to their capacities and writes the late section cull's indirect
// dispatch: a workgroup a late visible candidate, then one per
// CULL_WORKGROUP queue entries.
@compute @workgroup_size(1)
fn cull_finalize_late() {
 let visible=min(atomicLoad(&cull_lists.late_visible_count),cull_view.candidates);
 let queued=min(atomicLoad(&cull_lists.queue_count),cull_view.queue_capacity);
 cull_lists.late_visible_dispatched=visible;
 cull_lists.queue_dispatched=queued;
 let workgroups=visible+(queued+CULL_WORKGROUP-1u)/CULL_WORKGROUP;
 cull_lists.late_sections_side=cull_dispatch_of(DISPATCH_LATE_SECTIONS,workgroups);
}

// The late section cull, one dispatch over two kinds of workgroup: first a
// workgroup a late visible candidate, striding its level's sections as the
// early section cull does; then a workgroup per CULL_WORKGROUP queue
// entries, an invocation a section the early phase found occluded within a
// candidate it found visible. A section that passes the view's clip volume
// and is not hidden behind this frame's depth pyramid at its object's pose
// (every one of a deforming candidate) is appended to its set's late
// region.
@compute @workgroup_size(CULL_WORKGROUP)
fn cull_sections_late(@builtin(workgroup_id) workgroup:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
 let index=workgroup.y*cull_lists.late_sections_side+workgroup.x;
 let visible=cull_lists.late_visible_dispatched;
 if index<visible {
  let entry=cull_lists.entries[2u*cull_view.candidates+index];
  let candidate=cull_candidates[entry.x];
  let mesh=entry.y;
  let flags=cull_objects[candidate.object].flags;
  let model=cull_objects[candidate.object].model;
  let count=cull_section_count(mesh);
  let tested=(flags&OBJECT_DEFORMING)==0u;
  for(var stride=0u;stride<MAX_MESH_SECTIONS/CULL_WORKGROUP;stride++) {
   let section=stride*CULL_WORKGROUP+lane;
   if section>=count {
    break;
   }
   let at=cull_section_words(mesh,section);
   if tested && !cull_late_visible(model,at,candidate.draw_set) {
    continue;
   }
   cull_append(entry.x,candidate,mesh,at,flags,true);
  }
  return;
 }
 let queued=(index-visible)*CULL_WORKGROUP+lane;
 if queued>=cull_lists.queue_dispatched {
  return;
 }
 let queue_entry=cull_lists.entries[3u*cull_view.candidates+queued];
 let entry=cull_lists.entries[queue_entry.x];
 let candidate=cull_candidates[entry.x];
 let at=cull_section_words(entry.y,queue_entry.y);
 if cull_late_visible(cull_objects[candidate.object].model,at,candidate.draw_set) {
  cull_append(entry.x,candidate,entry.y,at,cull_objects[candidate.object].flags,true);
 }
}

// The bounds of the section whose words are at `at`, of a candidate drawn
// in set `draw_set`, grown (cull_grown).
fn cull_section_bounds(at:u32,draw_set:u32)->CullBounds {
 return cull_grown(cull_source_v3(at+SCENE_SECTION_MIN),cull_source_v3(at+SCENE_SECTION_MAX),draw_set);
}
// Whether the section whose words are at `at`, of a candidate drawn in set
// `draw_set`, passes the view's clip volume and is not hidden behind this
// frame's depth pyramid at pose `model`.
fn cull_late_visible(model:mat4x4<f32>,at:u32,draw_set:u32)->bool {
 let bounds=cull_section_bounds(at,draw_set);
 return cull_reaches(model,bounds.lo,bounds.hi) && !cull_occluded(cull_occlusion.current,model,bounds.lo,bounds.hi);
}
