// Shadow casters: depth-only passes for the directional cascades and the
// local-light shadow atlas's faces, over the positions of the drawn mesh or
// of a deforming instance's deformed vertices (CasterVertex), each instance of
// a draw at its own object record's pose (DrawInstance). Each cascade and
// face is a view; its draw list holds only the frame's visibility groups, and
// each batch's cull selects the side its material and pose cast
// (draw_list::Population::cull).
@vertex fn shadow_vs(@location(0) position:vec3<f32>,drawn:DrawInstance)->@builtin(position) vec4<f32> {
 return view.view_projection*objects[drawn.object].model*vec4(position,1.);
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
@vertex fn shadow_unclipped_vs(@location(0) position:vec3<f32>,drawn:DrawInstance)->UnclippedCaster {
 var out:UnclippedCaster;
 out.position=view.view_projection*objects[drawn.object].model*vec4(position,1.);
 out.unclipped_depth=out.position.z;
 out.position.z=min(out.position.z,1.0);
 return out;
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
fn masked_caster(position:vec3<f32>,drawn:DrawInstance,index:u32)->MaskedCaster {
 let vertex=scene_vertex_word(drawn.mesh,index-drawn.first_vertex);
 var out:MaskedCaster;
 out.position=view.view_projection*objects[drawn.object].model*vec4(position,1.);
 out.uv=scene_vertex_uv(drawn.mesh,vertex);
 out.color=scene_vertex_color(vertex);
 out.unclipped_depth=out.position.z;
 return out;
}
@vertex fn shadow_masked_vs(@location(0) position:vec3<f32>,drawn:DrawInstance,@builtin(vertex_index) index:u32)->MaskedCaster {
 return masked_caster(position,drawn,index);
}
@vertex fn shadow_masked_unclipped_vs(@location(0) position:vec3<f32>,drawn:DrawInstance,@builtin(vertex_index) index:u32)->MaskedCaster {
 var out=masked_caster(position,drawn,index);
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
