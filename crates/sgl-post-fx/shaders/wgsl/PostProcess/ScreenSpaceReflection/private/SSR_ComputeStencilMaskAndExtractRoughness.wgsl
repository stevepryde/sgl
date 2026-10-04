// WGSL port of DiligentFX Shaders/PostProcess/ScreenSpaceReflection/private/SSR_ComputeStencilMaskAndExtractRoughness.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.

#include "ScreenSpaceReflectionStructures.fxh"
#include "SSR_Common.fxh"
#include "FullScreenTriangleVSOutput.fxh"

// cbuffer cbScreenSpaceReflectionAttribs
@group(0) @binding(0) var<uniform> g_SSRAttribs: ScreenSpaceReflectionAttribs;

@group(0) @binding(1) var g_TextureMaterialParameters: texture_2d<f32>;
// WGSL: the depth buffer binds as a depth texture.
@group(0) @binding(2) var g_TextureDepth: texture_depth_2d;

fn LoadRoughness(PixelCoord: vec2<i32>) -> f32
{
    let MaterialParams = HlslLoad(g_TextureMaterialParameters, PixelCoord, 0);
    let RoughnessSelector = vec4<f32>(
        select(0.0, 1.0, g_SSRAttribs.RoughnessChannel == 0u),
        select(0.0, 1.0, g_SSRAttribs.RoughnessChannel == 1u),
        select(0.0, 1.0, g_SSRAttribs.RoughnessChannel == 2u),
        select(0.0, 1.0, g_SSRAttribs.RoughnessChannel == 3u));
    var Roughness = dot(MaterialParams, RoughnessSelector);
    if (g_SSRAttribs.IsRoughnessPerceptual == 0u) {
        Roughness = sqrt(Roughness);
    }
    return Roughness;
}

fn LoadDepth(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoadDepth(g_TextureDepth, PixelCoord, 0);
}

struct MaskAndRoughness
{
    @location(0) Roughness: f32,
    @builtin(frag_depth) Mask: f32,
}

@fragment
fn ComputeStencilMaskAndExtractRoughnessPS(VSOut: FullScreenTriangleVSOutput) -> MaskAndRoughness
{
    let Roughness = LoadRoughness(vec2<i32>(VSOut.f4PixelPos.xy));
    let Depth     = LoadDepth(vec2<i32>(VSOut.f4PixelPos.xy));
    // DFX-21: the half-resolution mask reads every pixel's roughness,
    // including rejected samples. Write both outputs without discarding.
    // The depth attachment is a 0/1 eligibility mask, not scene depth.
    return MaskAndRoughness(Roughness,
        select(0.0, 1.0, IsReflectionSample(Roughness, Depth, g_SSRAttribs.RoughnessThreshold)));
}
