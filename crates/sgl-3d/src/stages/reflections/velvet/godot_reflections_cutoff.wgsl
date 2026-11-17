// The perceptual roughness at which Velvet's trace stops (Godot's
// `roughness >= 0.7` in screen_space_reflection.glsl). The trace and the
// temporal pass read it here; composition reads its Rust twin,
// velvet::ROUGHNESS_CUTOFF, tied by the layout test.
const ROUGHNESS_CUTOFF: f32 = 0.7;
