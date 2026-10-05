// The full-screen triangle's vertex entry point, for passes that need
// nothing else of their vertices.
@vertex fn fullscreen_vs(@builtin(vertex_index) id:u32)->@builtin(position) vec4<f32> {
 return fullscreen_position(fullscreen_corner(id));
}
