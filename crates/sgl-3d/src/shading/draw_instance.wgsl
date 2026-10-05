// One instance of a scene geometry draw (shading::vertex::DrawInstance),
// stepped per instance from a draw list's draw instances: the index of the
// instance's object record, the drawn mesh's record in the scene source, the
// first index it draws, relative to its mesh's indices, and its triangles,
// past which a GPU-built draw's vertices are dummies, and the base vertex a
// caster of it adds to its vertex indices to reach its position in its
// positions slab (NO_POSITIONS for a GPU-built draw of a mesh without one).
// A draw of many instances reaches each one's record through it. The cull
// stage writes a GPU-built view's into its cluster list, so the struct is
// declared here, apart from any binding.
struct DrawInstance {
 @location(1) object:u32,
 @location(2) mesh:u32,
 @location(3) first_index:u32,
 @location(4) triangles:u32,
 @location(5) first_vertex:u32,
}
// DrawInstance.first_vertex of a GPU-built draw whose mesh has no positions
// in a slab, whose casters pull them from the scene source.
const NO_POSITIONS:u32=4294967295u;
