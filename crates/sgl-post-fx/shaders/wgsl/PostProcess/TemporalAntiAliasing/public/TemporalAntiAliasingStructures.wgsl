// WGSL port of DiligentFX Shaders/PostProcess/TemporalAntiAliasing/public/TemporalAntiAliasingStructures.fxh
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.

#ifndef _TEMPORAL_ANTI_ALIASING_STRUCTURES_FXH_
#define _TEMPORAL_ANTI_ALIASING_STRUCTURES_FXH_

#include "ShaderDefinitions.fxh"


// PROVENANCE.md DFX-14: Godot's velocity disocclusion. History is kept while the difference between
// a pixel's motion and the previous frame's motion where it was stays within this many pixels
// (taa.cpp disocclusion_threshold), and beyond it loses this share per pixel (taa_resolve.glsl
// DISOCCLUSION_SCALE), all of it 100 pixels further.
#define TAA_MOTION_DIFF_THRESHOLD_PIXELS    2.5
#define TAA_MOTION_DIFF_REJECTION_PER_PIXEL 0.01

// PROVENANCE.md DFX-14: Godot's speed-scaled variance box (taa_resolve.glsl clip_history_3x3).
// The box spans Godot's variance_dynamic standard deviations at rest and narrows smoothly to none
// at this speed of the pixel's closest motion, in screen fractions per frame.
#define TAA_VARIANCE_BOX_ZERO_SPEED 0.02

// Godot's variance_dynamic (taa.cpp): 1.1 standard deviations at 1080 rows, scaled inversely with
// the height and kept between 0.75 and 1.
#define TAA_VARIANCE_DYNAMIC_BASE        1.1
#define TAA_VARIANCE_DYNAMIC_BASE_HEIGHT 1080.0
#define TAA_VARIANCE_DYNAMIC_MIN         0.75
#define TAA_VARIANCE_DYNAMIC_MAX         1.0

// This parameter sets the threshold for depth disocclusion. It is used to determine how much a change in depth between frames should be considered as disocclusion,
// which occurs when previously occluded objects become visible. A small threshold value means that only significant depth changes will be treated as disocclusion,
// which can help in maintaining the stability of the image but may ignore some smaller, yet visually important changes.
#define TAA_DEPTH_DISOCCLUSION_THRESHOLD 0.9

// Defaults are those of the host structure (src/structures.rs).
struct TemporalAntiAliasingAttribs
{
    // The value is responsible for interpolating between the current and previous frame. Increasing the value increases temporal stability but may introduce ghosting
    TemporalStabilityFactor: f32,

    // If this parameter is set to true, the current frame will be written to the current history buffer without interpolation with the previous history buffer
    // WGSL: BOOL is u32 (ShaderDefinitions.wgsl).
    ResetAccumulation: u32,

    SkipRejection: u32,

    Padding0: f32,
}

#endif //_TEMPORAL_ANTI_ALIASING_STRUCTURES_FXH_
