// Group 1 of scene geometry: every instance's object record, at its index in
// the scene's object buffer. The scene's ray buffers (scene_rays.wgsl) are
// bindings 1 and 2 of the same group. Rust layout: shading::bind::scene.
@group(1) @binding(0) var<storage,read> objects:array<Object>;
// One instance of a draw (shading::vertex::DrawInstance), stepped per
// instance from the frame's draw instances: the index of the instance's
// object record, the drawn mesh's record in the scene source, and the base
// vertex an indexed draw of it adds to its indices. A draw of many instances
// reaches each one's record through it.
struct DrawInstance {
 @location(1) object:u32,
 @location(2) mesh:u32,
 @location(3) first_vertex:u32,
}
