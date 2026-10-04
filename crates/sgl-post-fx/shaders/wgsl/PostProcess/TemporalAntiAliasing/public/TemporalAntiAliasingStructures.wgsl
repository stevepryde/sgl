// WGSL port of DiligentFX Shaders/PostProcess/TemporalAntiAliasing/public/TemporalAntiAliasingStructures.fxh
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.

#ifndef _TEMPORAL_ANTI_ALIASING_STRUCTURES_FXH_
#define _TEMPORAL_ANTI_ALIASING_STRUCTURES_FXH_

#include "ShaderDefinitions.fxh"


// This parameter sets the minimum value for the variance gamma.
// The variance gamma is used to adjust the influence of historical data in the anti-aliasing process.
// A lower value means that the algorithm is less influenced by past frames, making it more responsive to changes but potentially less smooth
#define TAA_MIN_VARIANCE_GAMMA           0.75

// This parameter sets the maximum value for the variance gamma.
// A higher maximum value allows the algorithm to rely more heavily on historical data,
// which can produce smoother results but may also introduce more motion blur or ghosting effects in fast-moving scenes.
#define TAA_MAX_VARIANCE_GAMMA           2.5

// This parameter defines the threshold for pixel velocity difference that determines whether a pixel is considered to have "no history."
// If the difference in motion vectors between the current frame and the previous frame exceeds this value, the pixel is treated as if it has no historical data.
// This helps to prevent ghosting effects by not blending pixels with significantly different motion vectors.
#define TAA_MOTION_VECTOR_DIFF_FACTOR  256.0

// This parameter sets the threshold for depth disocclusion. It is used to determine how much a change in depth between frames should be considered as disocclusion,
// which occurs when previously occluded objects become visible. A small threshold value means that only significant depth changes will be treated as disocclusion,
// which can help in maintaining the stability of the image but may ignore some smaller, yet visually important changes.
#define TAA_DEPTH_DISOCCLUSION_THRESHOLD 0.9

// This parameter sets the max "distance" between source colour and target colour.
// Setting this to a larger value allows more bright pixels from the history buffer to be leaved unchanged.
#define TAA_VARIANCE_INTERSECTION_MAX_T  10.0

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
