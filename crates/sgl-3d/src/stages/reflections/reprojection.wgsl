// The depth of a reflection's virtual point: AMD FidelityFX Denoiser,
// revision d7dfecbabe7b9523b14e7b067216e06b86e8d189,
// ffx-reflection-dnsr/ffx_denoiser_reflections_reproject.h,
// FFX_DNSR_Reflections_GetHitPositionReprojection (MIT,
// src/LICENSE-amd-fidelityfx-denoiser.txt). Extend the normalised view ray
// by the reflected hit distance, rather than adding that distance to its
// axial depth. SGL3D's infinite reversed-Z depth is inversely proportional
// to axial depth, so the same ray-length ratio divides its device depth.
// Velvet's trace and world-space rays' resolve share this formula.
fn reflection_virtual_depth(surface_depth:f32,surface_distance:f32,hit_distance:f32)->f32 {
 return surface_depth/(1.+hit_distance/max(surface_distance,1e-6));
}
