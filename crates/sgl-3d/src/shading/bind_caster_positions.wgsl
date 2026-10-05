// Group 3 of a GPU-built directional cascade's casters, which the executor
// binds per set (view::draw_list::gpu): the positions slab its set's meshes
// hold their positions in (scene::geometry), each a CasterVertex of
// CASTER_VERTEX_WORDS f32, as a CPU-built caster list's vertex buffer holds
// them. Rust layout: shading::bind::caster_positions.
const CASTER_VERTEX_WORDS:u32=3u;
@group(3) @binding(0) var<storage,read> caster_positions:array<f32>;
