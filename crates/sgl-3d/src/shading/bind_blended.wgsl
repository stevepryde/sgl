// Group 3 of the blended pipelines, which the transparent stage binds: what
// the frame's screen-space method returned at the render size (radiance
// premultiplied by confidence, and the confidence), the surface depth (the
// Surface contract, specs/sgl3d-architecture.md), the method's cutoff and
// fade and whether the transmission copy and the volume layers hold the
// frame; on the Extended binding tier also that copy, the opaque depth and
// those layers (bind_blended_extended.wgsl). Rust layout:
// shading::bind::blended.
struct BlendedTrace {
 // Perceptual roughness at which the method traces no lobe; 0 while no
 // result composes (the draw into the reflection input, or no method).
 cutoff:f32,
 // The width of its fade below the cutoff.
 fade:f32,
 // 1 where the transmission copy holds this frame's composed frame (the
 // draw onto it, on the Extended tier, in a frame that shows a transmissive
 // material), else 0.
 transmission:u32,
 // 1 where the volume layers hold this frame's (the Extended tier, in a
 // frame whose volume paths are in effect and whose blended list holds a
 // material whose shader reads its volume path), else 0.
 volumes:u32,
}
@group(3) @binding(0) var blended_reflections:texture_2d<f32>;
@group(3) @binding(1) var blended_surface_depth:texture_depth_2d;
@group(3) @binding(2) var<uniform> blended_trace:BlendedTrace;
