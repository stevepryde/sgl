// The hardware path's shared module (the architecture's Hardware ray
// tracing): the scene's TLAS, which only the passes that trace bind, in
// their group 3 at this one entry (`shading::bind::tlas_entry`). Each
// instance's custom index is its entry's index, and its mask its kind
// (`scene::rays::acceleration`, `MASK_STATIC` and `MASK_MOVING`).
enable wgpu_ray_query;
@group(3) @binding(16) var scene_tlas:acceleration_structure;
