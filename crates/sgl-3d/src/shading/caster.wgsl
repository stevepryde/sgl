// Shadow casters: depth-only passes for the directional cascades and the
// local-light shadow atlas's faces, each instance of a draw at its own object
// record's pose (DrawInstance). A CPU-built list's casters (a local-light
// face's, a probe capture's cascades) draw indexed positions of the drawn
// mesh or of a deforming instance's deformed vertices (CasterVertex); a
// GPU-built cascade's pull theirs by the draw instance's mesh, first index
// and triangles, as source_vs pulls the camera's: the index from the scene
// source and the position from the positions slab its set binds as group 3
// (bind_caster_positions.wgsl), the slab the CPU-built casters draw from,
// at the draw instance's first vertex, or a deforming instance's deformed
// one. Each cascade and face is a view; its draw list holds only the
// frame's visibility groups, and each draw's cull selects the side its
// material and pose cast (draw_list::Population::cull, and a GPU-built
// set's variant). Every caster vertex is placed by the material's vertex
// function (material_shader.wgsl) as the camera's are, from its position
// and the rest of its vertex in the scene source; without a shader that is
// its position, and the rest is read by nothing.
fn caster_clip(position:vec3<f32>,drawn:DrawInstance)->vec4<f32> {
 return view.view_projection*objects[drawn.object].model*vec4(position,1.);
}
// Vertex `vertex_index` of the mesh `drawn` names at `position`, its slab's
// or a deforming instance's deformed one, with its deformed frame where the
// instance deforms, as the material's vertex function places it.
fn caster_vertex(position:vec3<f32>,drawn:DrawInstance,vertex_index:u32)->MaterialVertex {
 let vertex=scene_vertex_word(drawn.mesh,vertex_index);
 var frame:PackedFrame;
 if (objects[drawn.object].flags&OBJECT_DEFORMING)!=0u {
  frame=scene_deformed_frame(objects[drawn.object].deformed_normals,drawn.mesh,vertex_index);
 } else {
  frame=scene_vertex_frame(vertex);
 }
 let rest=MaterialVertex(position,frame.normal,frame.tangent,scene_vertex_uv(vertex,scene_mesh_uv_rect(drawn.mesh)),scene_vertex_color(vertex),scene_vertex_shader_data(drawn.mesh,vertex_index),vec4(0.));
 return material_shaded_vertex(drawn.object,rest,false);
}
// An indexed draw's vertex `index` at `position`: its vertex index is
// `index` less the draw's base vertex.
fn indexed_caster_clip(position:vec3<f32>,drawn:DrawInstance,index:u32)->vec4<f32> {
 return caster_clip(caster_vertex(position,drawn,index-drawn.first_vertex).position,drawn);
}
@vertex fn shadow_vs(@location(0) position:vec3<f32>,drawn:DrawInstance,@builtin(vertex_index) index:u32)->@builtin(position) vec4<f32> {
 return indexed_caster_clip(position,drawn,index);
}
// A GPU-built cascade's caster's vertex `vertex_index` of the mesh `drawn`
// names: its position in its positions slab, 12 bytes a vertex, as a
// CPU-built caster list's vertex buffer reads it, rather than from the scene
// source's 32-byte vertex records, which cost the depth-only cascades their
// bandwidth (#192); a deforming instance's deformed position, or a mesh's
// without slab positions from the scene source.
fn pulled_caster_position(drawn:DrawInstance,vertex_index:u32)->vec3<f32> {
 if deformed_vertices || drawn.first_vertex==NO_POSITIONS {
  return scene_pulled_position(drawn.object,drawn.mesh,vertex_index);
 }
 let at=(drawn.first_vertex+vertex_index)*CASTER_VERTEX_WORDS;
 return vec3(caster_positions[at],caster_positions[at+1u],caster_positions[at+2u]);
}
// A GPU-built cascade's caster: draw vertex `vertex` of the section `drawn`
// names, or a dummy past its triangles.
fn pulled_caster_clip(drawn:DrawInstance,vertex:u32)->vec4<f32> {
 if drawn_dummy(drawn,vertex) {
  return SCENE_DUMMY_CLIP;
 }
 let vertex_index=scene_pulled_vertex(drawn.mesh,drawn_index(drawn,vertex));
 return caster_clip(caster_vertex(pulled_caster_position(drawn,vertex_index),drawn,vertex_index).position,drawn);
}
@vertex fn shadow_pulled_vs(@builtin(vertex_index) vertex:u32,drawn:DrawInstance)->@builtin(position) vec4<f32> {
 return pulled_caster_clip(drawn,vertex);
}
// A paired draw's slot `slot` of the section `drawn` names (CULL_PAIRED in
// culling.wgsl): its draw vertex, the corner of the section it stands for.
// Pair slot / 4's slots are its corners a, b, c and d, as PAIRED_INDICES
// (shading::culling) draws them, (a, b, c) then (a, c, d): its triangles'
// first three corners and the second's last, which a paired section's
// second triangle names after a and c. A lone last triangle's d is its a,
// so the pattern's second triangle, (a, c, a), has no area; past the
// section's triangles the slots are dummies'.
fn paired_corner(drawn:DrawInstance,slot:u32)->u32 {
 let pair=slot/4u;
 let corner=slot%4u;
 if corner==3u && 2u*pair+1u>=drawn.triangles {
  return pair*6u;
 }
 return pair*6u+select(corner,5u,corner==3u);
}
@vertex fn shadow_paired_vs(@builtin(vertex_index) slot:u32,drawn:DrawInstance)->@builtin(position) vec4<f32> {
 return pulled_caster_clip(drawn,paired_corner(drawn,slot));
}
// A directional cascade's caster where the device lacks DEPTH_CLIP_CONTROL:
// Bevy 9d12036's UNCLIPPED_DEPTH_ORTHO_EMULATION
// (crates/bevy_pbr/src/prepass/prepass.wesl), MIT OR Apache-2.0
// (src/LICENSE-bevy.txt). The vertex's depth is clamped to the near plane so
// that a caster between the light and the cascade is not clipped, and the
// fragment writes the depth it had, which the orthographic projection
// interpolates linearly. Changed: the fragment clamps it to the near plane's
// depth as unclipped depth does, rather than leaving that to the viewport.
struct UnclippedCaster {
 @builtin(position) position:vec4<f32>,
 @location(0) unclipped_depth:f32,
}
fn unclipped_caster(clip:vec4<f32>)->UnclippedCaster {
 var out:UnclippedCaster;
 out.position=clip;
 out.unclipped_depth=out.position.z;
 out.position.z=min(out.position.z,1.0);
 return out;
}
@vertex fn shadow_unclipped_vs(@location(0) position:vec3<f32>,drawn:DrawInstance,@builtin(vertex_index) index:u32)->UnclippedCaster {
 return unclipped_caster(indexed_caster_clip(position,drawn,index));
}
@vertex fn shadow_pulled_unclipped_vs(@builtin(vertex_index) vertex:u32,drawn:DrawInstance)->UnclippedCaster {
 return unclipped_caster(pulled_caster_clip(drawn,vertex));
}
@vertex fn shadow_paired_unclipped_vs(@builtin(vertex_index) slot:u32,drawn:DrawInstance)->UnclippedCaster {
 return unclipped_caster(pulled_caster_clip(drawn,paired_corner(drawn,slot)));
}
@fragment fn shadow_unclipped_fs(in:UnclippedCaster)->@builtin(frag_depth) f32 {
 return min(in.unclipped_depth,1.0);
}
// A masked material's casters: the texel coordinates and colour of its base
// alpha, pulled from the scene source's vertex records of the drawn mesh
// (whose record the draw instance names, at the vertex index less the draw's
// base vertex, as Bevy b56fc29's morph_vertex subtracts
// first_vertex_index), reach the fragment, which
// discards what the material cuts out (material_alpha_discard), as Bevy
// 9d12036's shadow casters of masked materials do (MAY_DISCARD in
// crates/bevy_pbr/src/render/light.rs, prepass_alpha_discard). The unclipped
// variants emulate unclipped depth as shadow_unclipped_vs and
// shadow_unclipped_fs do.
struct MaskedCaster {
 @builtin(position) position:vec4<f32>,
 @location(0) uv:vec2<f32>,
 @location(1) color:vec4<f32>,
 @location(2) unclipped_depth:f32,
}
// Vertex `vertex_index` of the drawn mesh at `position`.
fn masked_caster_vertex(position:vec3<f32>,drawn:DrawInstance,vertex_index:u32)->MaskedCaster {
 let v=caster_vertex(position,drawn,vertex_index);
 var out:MaskedCaster;
 out.position=caster_clip(v.position,drawn);
 out.uv=v.uv;
 out.color=v.color;
 out.unclipped_depth=out.position.z;
 return out;
}
// An indexed draw's vertex `index`, less the draw's base vertex.
fn masked_caster(position:vec3<f32>,drawn:DrawInstance,index:u32)->MaskedCaster {
 return masked_caster_vertex(position,drawn,index-drawn.first_vertex);
}
// A GPU-built cascade's draw vertex `vertex`, or a dummy past its
// triangles.
fn pulled_masked_caster(drawn:DrawInstance,vertex:u32)->MaskedCaster {
 if drawn_dummy(drawn,vertex) {
  var out:MaskedCaster;
  out.position=SCENE_DUMMY_CLIP;
  return out;
 }
 let vertex_index=scene_pulled_vertex(drawn.mesh,drawn_index(drawn,vertex));
 return masked_caster_vertex(pulled_caster_position(drawn,vertex_index),drawn,vertex_index);
}
@vertex fn shadow_masked_vs(@location(0) position:vec3<f32>,drawn:DrawInstance,@builtin(vertex_index) index:u32)->MaskedCaster {
 return masked_caster(position,drawn,index);
}
@vertex fn shadow_masked_unclipped_vs(@location(0) position:vec3<f32>,drawn:DrawInstance,@builtin(vertex_index) index:u32)->MaskedCaster {
 var out=masked_caster(position,drawn,index);
 out.position.z=min(out.position.z,1.0);
 return out;
}
@vertex fn shadow_pulled_masked_vs(@builtin(vertex_index) vertex:u32,drawn:DrawInstance)->MaskedCaster {
 return pulled_masked_caster(drawn,vertex);
}
@vertex fn shadow_pulled_masked_unclipped_vs(@builtin(vertex_index) vertex:u32,drawn:DrawInstance)->MaskedCaster {
 var out=pulled_masked_caster(drawn,vertex);
 out.position.z=min(out.position.z,1.0);
 return out;
}
@fragment fn shadow_masked_fs(in:MaskedCaster) {
 material_alpha_discard(material_base_color(in.uv,in.color).a);
}
@fragment fn shadow_masked_unclipped_fs(in:MaskedCaster)->@builtin(frag_depth) f32 {
 material_alpha_discard(material_base_color(in.uv,in.color).a);
 return min(in.unclipped_depth,1.0);
}
// A masked material's casters where it has a shader: its coverage is the
// alpha of its surface function's base colour (material_shader.wgsl), as
// Filament ef1a133 runs material() in its masked depth variants
// (shaders/src/surface_depth_main.fs 21–69), so each vertex carries what
// the function's context reads: the instance, its world position, normal
// and the vertex function's `custom`. The pipelines of a material without
// one draw MaskedCaster's entries, which carry its texel alone.
struct ShadedMaskedCaster {
 @builtin(position) position:vec4<f32>,
 @location(0) uv:vec2<f32>,
 @location(1) color:vec4<f32>,
 @location(2) unclipped_depth:f32,
 @location(3) world:vec3<f32>,
 @location(4) normal:vec3<f32>,
 @location(5) custom:vec4<f32>,
 @location(6) @interpolate(flat) object:u32,
}
fn shaded_masked_caster_vertex(position:vec3<f32>,drawn:DrawInstance,vertex_index:u32)->ShadedMaskedCaster {
 let v=caster_vertex(position,drawn,vertex_index);
 let model=objects[drawn.object].model;
 var out:ShadedMaskedCaster;
 out.position=caster_clip(v.position,drawn);
 out.uv=v.uv;
 out.color=v.color;
 out.unclipped_depth=out.position.z;
 out.world=(model*vec4(v.position,1.)).xyz;
 out.normal=object_normal(model,v.normal);
 out.custom=v.custom;
 out.object=drawn.object;
 return out;
}
fn pulled_shaded_masked_caster(drawn:DrawInstance,vertex:u32)->ShadedMaskedCaster {
 if drawn_dummy(drawn,vertex) {
  var out:ShadedMaskedCaster;
  out.position=SCENE_DUMMY_CLIP;
  return out;
 }
 let vertex_index=scene_pulled_vertex(drawn.mesh,drawn_index(drawn,vertex));
 return shaded_masked_caster_vertex(pulled_caster_position(drawn,vertex_index),drawn,vertex_index);
}
@vertex fn shadow_shaded_masked_vs(@location(0) position:vec3<f32>,drawn:DrawInstance,@builtin(vertex_index) index:u32)->ShadedMaskedCaster {
 return shaded_masked_caster_vertex(position,drawn,index-drawn.first_vertex);
}
@vertex fn shadow_shaded_masked_unclipped_vs(@location(0) position:vec3<f32>,drawn:DrawInstance,@builtin(vertex_index) index:u32)->ShadedMaskedCaster {
 var out=shaded_masked_caster_vertex(position,drawn,index-drawn.first_vertex);
 out.position.z=min(out.position.z,1.0);
 return out;
}
@vertex fn shadow_pulled_shaded_masked_vs(@builtin(vertex_index) vertex:u32,drawn:DrawInstance)->ShadedMaskedCaster {
 return pulled_shaded_masked_caster(drawn,vertex);
}
@vertex fn shadow_pulled_shaded_masked_unclipped_vs(@builtin(vertex_index) vertex:u32,drawn:DrawInstance)->ShadedMaskedCaster {
 var out=pulled_shaded_masked_caster(drawn,vertex);
 out.position.z=min(out.position.z,1.0);
 return out;
}
// The coverage of the caster's fragment `in` on the face raster takes as
// `raster_front`.
fn shaded_caster_coverage(in:ShadedMaskedCaster,raster_front:bool)->f32 {
 let front=object_front(in.object,raster_front);
 let geometry_normal=side_normal(in.normal,front);
 let recorded=material_texel_surface(material_base_texels(in.uv,in.color),geometry_normal);
 let context=material_context(in.object,in.world,geometry_normal,in.uv,in.color,in.custom,front,in.position.xy);
 return material_surface(recorded,context,material_shader_params(false)).base_color.a;
}
@fragment fn shadow_shaded_masked_fs(in:ShadedMaskedCaster,@builtin(front_facing) front:bool) {
 material_alpha_discard(shaded_caster_coverage(in,front));
}
@fragment fn shadow_shaded_masked_unclipped_fs(in:ShadedMaskedCaster,@builtin(front_facing) front:bool)->@builtin(frag_depth) f32 {
 material_alpha_discard(shaded_caster_coverage(in,front));
 return min(in.unclipped_depth,1.0);
}
