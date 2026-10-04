// Group 3 of the blended pipelines, which the transparent stage binds: what
// the frame's screen-space method returned at the render size (radiance
// premultiplied by confidence, and the confidence), the surface depth (the
// Surface contract, specs/sgl3d-architecture.md) and the method's cutoff and
// fade. Rust layout: shading::bind::blended.
struct BlendedTrace {
 // Perceptual roughness at which the method traces no lobe; 0 while no
 // result composes (the draw into the reflection input, or no method).
 cutoff:f32,
 // The width of its fade below the cutoff.
 fade:f32,
 padding:vec2<f32>,
}
@group(3) @binding(0) var blended_reflections:texture_2d<f32>;
@group(3) @binding(1) var blended_surface_depth:texture_depth_2d;
@group(3) @binding(2) var<uniform> blended_trace:BlendedTrace;
