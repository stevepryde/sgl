// WGSL port of DiligentFX Shaders/Common/private/ComputeClosestMotion.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.

#include "BasicStructures.fxh"
#include "FullScreenTriangleVSOutput.fxh"
#include "PostFX_Common.fxh"

#if POSTFX_OPTION_INVERTED_DEPTH
    #define DepthFarPlane  0.0
#else
    #define DepthFarPlane  1.0
#endif // POSTFX_OPTION_INVERTED_DEPTH

// WGSL: the depth buffer binds as a depth texture.
@group(0) @binding(0) var g_TextureDepth: texture_depth_2d;
@group(0) @binding(1) var g_TextureMotion: texture_2d<f32>;

fn SampleDepth(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoadDepth(g_TextureDepth, PixelCoord, 0);
}

fn SampleMotion(PixelCoord: vec2<i32>) -> vec2<f32>
{
    return HlslLoad(g_TextureMotion, PixelCoord, 0).xy;
}

fn SampleClosestMotion(PixelCoord: vec2<i32>) -> vec2<f32>
{
    var ClosestDepth: f32 = DepthFarPlane;
    var ClosestOffset = vec2<i32>(0, 0);

    const SearchRadius = 1;
    for (var x = -SearchRadius; x <= SearchRadius; x++)
    {
        for (var y = -SearchRadius; y <= SearchRadius; y++)
        {
            let Coord = PixelCoord + vec2<i32>(x, y);
            let NeighborDepth = SampleDepth(Coord);
#if POSTFX_OPTION_INVERTED_DEPTH
            if (NeighborDepth > ClosestDepth)
#else
            if (NeighborDepth < ClosestDepth)
#endif
            {
                ClosestOffset = vec2<i32>(x, y);
                ClosestDepth = NeighborDepth;
            }
        }
    }

    return SampleMotion(PixelCoord + ClosestOffset);
}

@fragment
fn ComputeClosestMotionPS(VSOut: FullScreenTriangleVSOutput) -> @location(0) vec2<f32>
{
    let Position = VSOut.f4PixelPos;
    return SampleClosestMotion(vec2<i32>(Position.xy));
}
