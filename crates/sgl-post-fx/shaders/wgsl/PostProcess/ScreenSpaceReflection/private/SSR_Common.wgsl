// WGSL port of DiligentFX Shaders/PostProcess/ScreenSpaceReflection/private/SSR_Common.fxh
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// VanDerCorputSequenceBase2, HammersleySequence, VogelDiskSample and
// MapSquareToDisk are unused by the passes and omitted.

#ifndef _SSR_COMMON_FXH_
#define _SSR_COMMON_FXH_

#include "PostFX_Common.fxh"

// WGSL: ClosestDepth is a function; WGSL has no function-like macros.
#if SSR_OPTION_INVERTED_DEPTH
    fn ClosestDepth(a: f32, b: f32) -> f32 { return max(a, b); }
    #define DepthFarPlane 0.0
#else
    fn ClosestDepth(a: f32, b: f32) -> f32 { return min(a, b); }
    #define DepthFarPlane 1.0
#endif // SSR_OPTION_INVERTED_DEPTH

// WGSL: SSR_ATTRIBUTE_EARLY_DEPTH_STENCIL ([earlydepthstencil]) has no WGSL
// equivalent. The passes that carry it neither discard nor write depth, so
// the depth test already runs before the fragment shader (PROVENANCE.md).

fn IsBackground(Depth: f32) -> bool
{
#if SSR_OPTION_INVERTED_DEPTH
    return Depth < 1e-6;
#else
    return Depth >= (1.0 - 1e-6);
#endif // SSR_OPTION_INVERTED_DEPTH
}

fn IsReflectionSample(Roughness: f32, Depth: f32, RoughnessThreshold: f32) -> bool
{
    return Roughness <= RoughnessThreshold && !IsBackground(Depth);
}

fn IsMirrorReflection(Roughness: f32) -> bool
{
    return Roughness < 0.01;
}

#endif // _SSR_COMMON_FXH_
