// Group 1 of scene geometry: every instance's object record, at its index in
// the scene's object buffer. The scene's ray buffers (scene_rays.wgsl) are
// bindings 1 and 2 of the same group. Rust layout: shading::bind::scene.
@group(1) @binding(0) var<storage,read> objects:array<Object>;
