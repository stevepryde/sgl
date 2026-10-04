// WGSL port of DiligentFX Shaders/PostProcess/ScreenSpaceReflection/private/SSR_ComputeDownsampledStencilMask.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.

#include "ScreenSpaceReflectionStructures.fxh"
#include "SSR_Common.fxh"
#include "FullScreenTriangleVSOutput.fxh"

// cbuffer cbScreenSpaceReflectionAttribs
@group(0) @binding(0) var<uniform> g_SSRAttribs: ScreenSpaceReflectionAttribs;

@group(0) @binding(1) var g_TextureRoughness: texture_2d<f32>;
// WGSL: the depth buffer binds as a depth texture.
@group(0) @binding(2) var g_TextureDepth: texture_depth_2d;

fn UpdateClosestDepthAndMaxRoughness(Location_: vec2<i32>, Dimension: vec2<i32>, MinDepth: ptr<function, f32>, MaxRoughness: ptr<function, f32>)
{
    // WGSL: parameters are immutable; the upstream in-parameter is copied.
    let Location = ClampScreenCoord(Location_, Dimension);

    let Depth     = HlslLoadDepth(g_TextureDepth, Location, 0);
    let Roughness = HlslLoad(g_TextureRoughness, Location, 0).x;

    *MinDepth     = ClosestDepth(*MinDepth, Depth);
    *MaxRoughness = max(*MaxRoughness, Roughness);
}

@fragment
fn ComputeDownsampledStencilMaskPS(VSOut: FullScreenTriangleVSOutput)
{
    let RemappedPosition = vec2<i32>(2.0 * floor(VSOut.f4PixelPos.xy));

    let TextureDimension = vec2<i32>(textureDimensions(g_TextureDepth));

    var MinDepth: f32 = DepthFarPlane;
    var MaxRoughness: f32 = 0.0f;

    UpdateClosestDepthAndMaxRoughness(RemappedPosition + vec2<i32>(0, 0), TextureDimension, &MinDepth, &MaxRoughness);
    UpdateClosestDepthAndMaxRoughness(RemappedPosition + vec2<i32>(1, 0), TextureDimension, &MinDepth, &MaxRoughness);
    UpdateClosestDepthAndMaxRoughness(RemappedPosition + vec2<i32>(0, 1), TextureDimension, &MinDepth, &MaxRoughness);
    UpdateClosestDepthAndMaxRoughness(RemappedPosition + vec2<i32>(1, 1), TextureDimension, &MinDepth, &MaxRoughness);

    let IsWidthOdd  = (TextureDimension.x & 1) != 0;
    let IsHeightOdd = (TextureDimension.y & 1) != 0;

    if (IsWidthOdd)
    {
        UpdateClosestDepthAndMaxRoughness(RemappedPosition + vec2<i32>(2, 0), TextureDimension, &MinDepth, &MaxRoughness);
        UpdateClosestDepthAndMaxRoughness(RemappedPosition + vec2<i32>(2, 1), TextureDimension, &MinDepth, &MaxRoughness);
    }

    if (IsHeightOdd)
    {
        UpdateClosestDepthAndMaxRoughness(RemappedPosition + vec2<i32>(0, 2), TextureDimension, &MinDepth, &MaxRoughness);
        UpdateClosestDepthAndMaxRoughness(RemappedPosition + vec2<i32>(1, 2), TextureDimension, &MinDepth, &MaxRoughness);
    }

    if (IsWidthOdd && IsHeightOdd)
    {
        UpdateClosestDepthAndMaxRoughness(RemappedPosition + vec2<i32>(2, 2), TextureDimension, &MinDepth, &MaxRoughness);
    }

    if (!IsReflectionSample(MaxRoughness, MinDepth, g_SSRAttribs.RoughnessThreshold)) {
        discard;
    }
}
