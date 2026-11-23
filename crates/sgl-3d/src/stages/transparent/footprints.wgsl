// Instance footprints (stages/transparent/footprints.rs, the register's
// M-23): a changed instance's opaque and masked meshes where it stood or
// stands, marking transparency and composition 1 in the temporal
// composition pass. Each draw instance names a record at its object index
// (group 1's objects: the scene's footprint records for where it stood,
// its object records for where it stands) and a mesh in the scene source,
// whose triangles the draw pulls whole, at the record's pose and, where
// the record names deformed positions, at those (scene_shown_position):
// the base level of detail, without the material's vertex function or
// alpha test, so a masked mesh's cut-out texels are marked too.
@vertex fn footprint_vs(@builtin(vertex_index) vertex:u32,drawn:DrawInstance)->@builtin(position) vec4<f32> {
 let index=scene_pulled_vertex(drawn.mesh,drawn_index(drawn,vertex));
 let position=scene_shown_position(drawn.object,drawn.mesh,index);
 return view.view_projection*objects[drawn.object].model*vec4(position,1.);
}
// Transparency and composition 1; the reactive target is not written
// (view::targets::composition_targets).
@fragment fn footprint_fs()->@location(1) f32 {
 return 1.;
}
