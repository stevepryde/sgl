// One instance of a scene geometry draw (shading::vertex::DrawInstance),
// stepped per instance from a draw list's draw instances: the index of the
// instance's object record, the drawn mesh's record in the scene source, the
// first index it draws, relative to its mesh's indices, and its triangles,
// past which a GPU-built draw's vertices are dummies, and the base vertex an
// indexed caster draw of it adds to its indices. A draw of many instances
// reaches each one's record through it. The cull stage writes a GPU-built
// view's into its cluster list, so the struct is declared here, apart from
// any binding.
struct DrawInstance {
 @location(1) object:u32,
 @location(2) mesh:u32,
 @location(3) first_index:u32,
 @location(4) triangles:u32,
 @location(5) first_vertex:u32,
}
